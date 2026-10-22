"""Personal Hermes terminal backend: manager-leased SSH workers with pinned host keys."""
import json
import base64
import ipaddress
import logging
import os
import re
import threading
import weakref
from pathlib import Path
import socket
import shlex
import ssl
import time
import tempfile
import urllib.error
import urllib.request
import uuid

from agent.terminal_env_provider import TerminalEnvironmentProvider
from tools.environments.ssh import SSHEnvironment

log = logging.getLogger(__name__)
# Written by the manager into each personal instance; the only connection source.
MANAGED_CONFIG = Path('/run/hermes/execution-ca/config.yaml')
WORKSPACE = '/workspace'


def personal_instance():
    return (Path('/run/hermes/owner/context.json').is_file()
            or bool(os.environ.get('HERMES_PERSONAL_INSTANCE_ID')))


def session_key():
    from gateway.session_context import get_session_env
    return get_session_env("HERMES_SESSION_KEY", "")


def invocation_cwd(cwd, task_id):
    """Recover an explicitly declared workspace directory after Hermes's generic
    container guard has sanitized a host-looking cwd."""
    from agent.runtime_cwd import scoped_session_cwd
    from tools.terminal_tool import get_session_cwd
    declared = get_session_cwd(task_id) or scoped_session_cwd()
    if declared and Path(declared).is_relative_to(WORKSPACE):
        return declared
    return cwd


class Client:
    def __init__(self, settings, runtime=None):
        self.settings = settings
        self.runtime = runtime

    def call(self, path, body=None, timeout=30):
        token = Path(self.settings["token_file"]).read_text().strip()
        url = self.settings['manager_url'].rstrip('/')
        if not url.startswith('https://'):
            raise ValueError('The execution manager requires HTTPS')
        if not self.settings.get('ca_file'):
            raise ValueError('The execution manager requires a pinned CA file')
        context = ssl.create_default_context(cafile=self.settings['ca_file'])
        request = urllib.request.Request(url + path,
            data=json.dumps(body).encode() if body is not None else None,
            headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"})
        with urllib.request.urlopen(request, timeout=timeout, context=context) as response:
            return json.load(response)

    def acquire(self, session, parent_lease=None):
        # Idempotent retry protects against a lost response after the manager grants a lease.
        payload = {key: self.settings[key] for key in ("profile", "mode")}
        payload.update(session=session, request=uuid.uuid4().hex)
        if self.settings.get('runtime_id'):
            payload['runtime_id'] = self.settings['runtime_id']
        if self.runtime:
            payload['runtime'] = self.runtime
        if parent_lease:
            payload['parent_lease'] = parent_lease
        deadline = time.monotonic() + self.settings["acquire_timeout_seconds"]
        while True:
            try:
                return self.call("/v1/leases/acquire", payload, timeout=max(1, deadline - time.monotonic()))
            except urllib.error.HTTPError as exc:
                if exc.code not in (409, 503) or time.monotonic() >= deadline:
                    raise RuntimeError(f"Execution manager rejected provisioning ({exc.code})") from None
            except (urllib.error.URLError, TimeoutError):
                if time.monotonic() >= deadline:
                    raise RuntimeError("Execution manager provisioning timed out") from None
            time.sleep(min(1, max(0, deadline - time.monotonic())))


class ManagedSSHEnvironment(SSHEnvironment):
    """A native environment owns its manager lease and its SSH transport."""

    def __init__(self, provider, settings, cwd, timeout, task_id, runtime=None):
        self.provider, self.settings = provider, settings
        from hermes_cli.config import get_hermes_home
        self.runtime = runtime or {'hermes_home': str(get_hermes_home()), 'workdir': cwd}
        self.runtime_home = self.runtime['hermes_home']
        self.client = Client(settings, self.runtime)
        self.managed_task_key = task_id
        # Hermes supplies the resolved session key, including its native delegate aliases.
        # Prompt probes are one-shot environments and must never close a conversation.
        self.session = ('probe:' + uuid.uuid4().hex if task_id == 'prompt-backend-probe'
                        else task_id)
        self.context_key = session_key()
        self.identities = {task_id, self.context_key} - {''}
        self._guard = threading.RLock()
        self._reply = None
        self._renew_stop = None
        self._renew_worker = None
        self._lost = False
        self._closed = False
        self._owners = set()
        self._initial_cwd, self._initial_timeout = cwd, timeout
        self.managed_generation = None
        self.managed_environment = None
        self._sync_manager = None
        with provider._guard:
            self._owners.update(token for token, (key, context, identity) in provider._turns.items()
                                if key == task_id or (context and context == self.context_key))
            self.identities.update(identity for token, (key, context, identity) in provider._turns.items()
                                   if token in self._owners and identity)
        try:
            self._ensure_lease()
        except BaseException:
            try:
                self.cleanup()
            except Exception:
                log.warning('Partial SSH setup cleanup failed')
            raise

    def _connect(self, reply):
        if self.managed_generation is not None:
            # An idle container can have been recreated: skip sync against the old endpoint.
            self._sync_manager = None
            super().cleanup()
        self.managed_generation = reply['generation']
        self.managed_environment = reply['environment']
        endpoint = dict(reply['ssh'])
        host_key = endpoint.pop('host_key', None)
        if not host_key:
            raise RuntimeError('Manager did not supply a verified SSH host identity')
        old_identity = getattr(self, '_host_identity_dir', None)
        if old_identity is not None:
            old_identity.cleanup()
        self._host_identity_dir = None
        self._known_hosts = None
        if endpoint.get('port') != 2222 or endpoint.get('user') != 'worker':
            raise RuntimeError('Invalid managed SSH endpoint')
        ipaddress.ip_address(endpoint['host'])
        if not isinstance(host_key, str) or len(host_key) > 1024 or not re.fullmatch(r'ssh-ed25519 [A-Za-z0-9+/=]+', host_key):
            raise RuntimeError('Invalid managed SSH host identity')
        base64.b64decode(host_key.split()[1], validate=True)
        uuid.UUID(self.managed_environment)
        if not re.fullmatch(r'[1-9][0-9]*', self.managed_generation):
            raise RuntimeError('Invalid managed SSH generation')
        self._host_identity_dir = tempfile.TemporaryDirectory(prefix='hermes-managed-ssh-')
        self._known_hosts = Path(self._host_identity_dir.name) / 'known_hosts'
        alias = 'hermes-managed-' + self.managed_environment + '-' + self.managed_generation
        self._known_hosts.write_text(alias + ' ' + host_key + '\n')
        self._known_hosts.chmod(0o600)
        deadline = time.monotonic() + 30
        while True:
            try:
                with socket.create_connection((endpoint['host'], endpoint['port']), timeout=2):
                    break
            except OSError:
                if time.monotonic() >= deadline:
                    raise RuntimeError('Managed environment SSH readiness timed out') from None
                time.sleep(.25)
        # Independent sockets and shell snapshots even when a profile shares one container.
        super().__init__(**endpoint, key_path=self.settings['ssh_key'], cwd=self._initial_cwd,
                         timeout=self._initial_timeout, probe_only=True)
        self._remote_home = self._detect_remote_home()
        self._ensure_remote_dirs()
        from tools.environments.file_sync import FileSyncManager
        self._sync_manager = FileSyncManager(
            get_files_fn=self._sync_files,
            upload_fn=self._scp_upload, delete_fn=self._ssh_delete,
            bulk_upload_fn=self._ssh_bulk_upload, bulk_download_fn=self._ssh_bulk_download)
        self._sync_manager.sync(force=True)
        self.init_session()
        # Hermes's native lifecycle keeps session environments between messages and
        # calls cleanup() on session close, process exit or its ordinary idle reaper.
        self._session_scoped = True

    def _sync_files(self):
        # Cleanup/reconnect may run outside the original turn's context. Native
        # sync must read this invocation's home, never another active profile.
        from hermes_constants import set_hermes_home_override, reset_hermes_home_override
        from tools.environments.file_sync import iter_sync_files
        token = set_hermes_home_override(self.runtime_home)
        try:
            return list(iter_sync_files(f'{self._remote_home}/.hermes'))
        finally:
            reset_hermes_home_override(token)

    def _ensure_lease(self):
        if self._closed:
            raise RuntimeError('Managed execution environment is closed')
        if self._lost:
            raise RuntimeError('Execution lease lost; close this environment and retry')
        if self._reply is not None:
            return
        reply = self.client.acquire(self.session)
        self._reply = reply
        self._renew_stop = threading.Event()
        self._renew_worker = threading.Thread(target=self._renew, args=(reply, self._renew_stop),
                                              daemon=True, name='managed-execution-lease')
        self._renew_worker.start()
        if (self.managed_environment, self.managed_generation) != (reply['environment'], reply['generation']):
            try:
                self._connect(reply)
            except BaseException:
                self._sync_manager = None
                if hasattr(self, 'control_socket'):
                    try:
                        super().cleanup()
                    except Exception:
                        log.warning('Partial SSH reconnect cleanup failed')
                self.managed_generation = None
                self._release_lease()
                raise

    def _renew(self, reply, stop):
        ttl, last = reply['ttl'], time.monotonic()
        while not stop.wait(min(15, ttl / 4)):
            try:
                self.client.call('/v1/leases/heartbeat', {'lease': reply['lease']}, timeout=min(10, ttl / 8))
                last = time.monotonic()
            except Exception:
                if time.monotonic() - last >= ttl / 2:
                    self._lost = True
                    log.error('Execution lease heartbeat lost; further commands are blocked')
                    return

    def _release_lease(self):
        reply, self._reply = self._reply, None
        if reply is None:
            return
        self._renew_stop.set()
        self._renew_worker.join(timeout=12)
        try:
            self.client.call('/v1/leases/release', {'lease': reply['lease']}, timeout=15)
        except Exception:
            log.warning('Execution lease release failed; expiry will reclaim it')

    def begin_turn(self, token, identity):
        with self._guard:
            self._owners.add(token)
            if identity:
                self.identities.add(identity)
            # The hook stays cheap. Acquire/reconnect through execute(), where errors
            # fail the tool rather than being swallowed by observer-hook dispatch.

    def end_turn(self, token):
        with self._guard:
            self._owners.discard(token)
            if not self._owners:
                self._release_lease()

    def execute(self, *args, **kwargs):
        with self._guard:
            self._ensure_lease()
            try:
                return super().execute(*args, **kwargs)
            except BaseException:
                # A failed SSH/tool invocation must not keep a lease indefinitely.
                if not self._owners:
                    self._release_lease()
                raise

    def _target_flags(self, port_flag):
        # Pod IPs can be reused. Pin the lookup to this generation's manager-supplied host key.
        alias = "hermes-managed-" + self.managed_environment + "-" + self.managed_generation
        return super()._target_flags(port_flag) + [
            '-o', 'HostKeyAlias=' + alias, '-o', 'StrictHostKeyChecking=yes',
            '-o', 'UserKnownHostsFile=' + str(self._known_hosts), '-o', 'GlobalKnownHostsFile=/dev/null']

    def _build_ssh_command(self, extra_args=None, send_env=()):
        command = super()._build_ssh_command(extra_args, send_env)
        # OpenSSH takes the first option: override the native default too.
        return ['StrictHostKeyChecking=yes' if arg == 'StrictHostKeyChecking=accept-new' else arg for arg in command]

    def _snapshot_excluded_passthrough_names(self):
        # The built-in SSH transport only excludes these in multiplex mode. ACP and direct
        # CLI runs also need missing task tokens to disappear instead of surviving a snapshot.
        from tools.env_passthrough import get_all_passthrough
        self._snapshot_passthrough_names.update(get_all_passthrough())
        return tuple(sorted(self._snapshot_passthrough_names))

    def _wrap_command(self, command, cwd):
        remote_home = self._remote_home + '/.hermes'
        command = 'export HERMES_HOME=' + shlex.quote(remote_home) + '\n' + command
        return super()._wrap_command(command, cwd)

    def _before_execute(self):
        if self._lost:
            raise RuntimeError('Execution lease lost; further commands are blocked')
        super()._before_execute()

    def cleanup(self):
        with self._guard:
            if self._closed:
                return
            self._closed = True
            try:
                if hasattr(self, 'control_socket'):
                    if self._reply is None or self._lost:
                        self._sync_manager = None
                    super().cleanup()
            finally:
                self._release_lease()
                if self.settings['mode'] == 'per_session':
                    try:
                        body = {'profile': self.settings['profile'],
                                'session': self.session, 'runtime': self.runtime}
                        if self.settings.get('runtime_id'):
                            body['runtime_id'] = self.settings['runtime_id']
                        self.client.call('/v1/sessions/close', body, timeout=15)
                    except Exception:
                        log.warning('Execution session close failed; idle expiry will reclaim it')
                with self.provider._guard:
                    self.provider._environments.discard(self)
                identity = getattr(self, '_host_identity_dir', None)
                if identity is not None:
                    identity.cleanup()


class Provider(TerminalEnvironmentProvider):
    name = "managed_execution"
    display_name = "Managed execution (SSH)"
    is_remote = True
    is_container = True
    session_isolated_when_nonpersistent = True
    skip_container_guards = False
    cache_path_base = "~/.hermes"

    def __init__(self, ctx):
        self.ctx = ctx
        self._guard = threading.RLock()
        self._environments = weakref.WeakSet()
        self._turns = {}
        self._creation_locks = weakref.WeakValueDictionary()

    def settings(self):
        """Connection settings come only from the manager's managed configuration;
        profiles choose a lifetime mode and an approved runtime_id."""
        managed = json.loads(MANAGED_CONFIG.read_text())
        connection = managed['plugins']['entries']['managed_execution']['settings']
        settings = {key: connection[key] for key in ('manager_url', 'token_file', 'ca_file', 'ssh_key')}
        settings.update(
            # The wire profile is the credential's fixed service scope. Native
            # profile isolation is carried by runtime.hermes_home and runtime_id.
            profile='default',
            mode=self.ctx.get_config('mode', 'per_profile_on_demand'),
            acquire_timeout_seconds=self.ctx.get_config('acquire_timeout_seconds', 180),
            runtime_id=self.ctx.get_config('runtime_id', ''),
        )
        # The schema-driven dashboard saves the selection in the active profile's
        # native config; the manager still checks it against the allowlist.
        try:
            from hermes_cli.config import load_config
        except ImportError:
            load_config = None
        if load_config is not None:
            terminal = load_config().get('terminal', {})
            if isinstance(terminal, dict) and 'runtime_id' in terminal:
                value = terminal['runtime_id']
                settings['runtime_id'] = '' if value is None else value
        if settings['mode'] not in ('per_session', 'per_profile_on_demand', 'per_profile_always_running'):
            raise ValueError('Invalid managed_execution mode')
        if not isinstance(settings['runtime_id'], str) or (settings['runtime_id']
                and re.fullmatch(r'[a-z][a-z0-9-]{0,62}', settings['runtime_id']) is None):
            raise ValueError('Invalid managed_execution runtime_id')
        return settings

    def is_available(self):
        if not personal_instance():
            return False
        try:
            return Path(self.settings()['token_file']).is_file()
        except (ValueError, OSError, KeyError):
            return False

    def create_environment(self, *, cwd, timeout, task_id="default", **kwargs):
        settings = self.settings()
        cwd = invocation_cwd(cwd, task_id)
        from hermes_cli.config import get_hermes_home
        runtime = {'hermes_home': str(get_hermes_home()), 'workdir': cwd}
        if cwd != WORKSPACE:
            # A profile-lifetime worker is shared; a distinct workdir gets its own session.
            settings['mode'] = 'per_session'
        # Hermes uses both <session-id> (terminal calls) and session:<session-id>
        # (file/prompt calls without a task ID). Canonicalize inside the provider,
        # so its supported factory returns one transport for that conversation.
        key = task_id.removeprefix('session:')
        identity = (settings['manager_url'], settings['runtime_id'], settings['mode'], key,
                    json.dumps(runtime, sort_keys=True))
        with self._guard:
            lock = self._creation_locks.setdefault(identity, threading.Lock())
        with lock:
            with self._guard:
                existing = next((env for env in self._environments
                                 if not env._closed and env.factory_identity == identity), None)
            if existing is not None and key != 'prompt-backend-probe':
                return existing
            environment = ManagedSSHEnvironment(self, settings, cwd, timeout, key, runtime=runtime)
            environment.factory_identity = identity
            with self._guard:
                self._environments.add(environment)
            return environment

    def _native_environment(self, task_id):
        from tools.terminal_tool_lifecycle import get_active_env
        environment = get_active_env(task_id)
        return environment if getattr(environment, 'provider', None) is self else None

    def on_turn_start(self, *, task_id=None, session_id=None, turn_id=None, **kwargs):
        token = turn_id or task_id or session_id
        if not token:
            return
        with self._guard:
            self._turns[token] = (task_id, session_key(), session_id)
        environment = self._native_environment(task_id)
        if environment is not None:
            environment.begin_turn(token, session_id)

    def on_turn_end(self, *, task_id=None, session_id=None, turn_id=None, **kwargs):
        token = turn_id or task_id or session_id
        with self._guard:
            self._turns.pop(token, None)
            environments = list(self._environments)
        native = self._native_environment(task_id)
        for environment in environments:
            if environment is native or token in environment._owners:
                environment.end_turn(token)

    def on_session_finalize(self, *, session_id=None, session_key=None, **kwargs):
        identities = {session_id, session_key} - {None, ''}
        with self._guard:
            environments = list(self._environments)
        for environment in environments:
            if identities & environment.identities:
                environment.cleanup()

    def on_session_reset(self, *, old_session_id=None, **kwargs):
        # Reset hooks identify the NEW session; never close it by mistake.
        if old_session_id:
            self.on_session_finalize(session_id=old_session_id)


def register(ctx):
    provider = Provider(ctx)
    ctx.register_terminal_environment_provider(provider)
    ctx.register_hook('pre_llm_call', provider.on_turn_start)
    ctx.register_hook('on_session_end', provider.on_turn_end)
    ctx.register_hook('on_session_finalize', provider.on_session_finalize)
    ctx.register_hook('on_session_reset', provider.on_session_reset)
