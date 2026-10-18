"""Native Hermes terminal provider; manager owns images, mounts and scheduling."""
import json
import hashlib
import logging
import os
import threading
import weakref
from pathlib import Path
import socket
import shlex
import ssl
import time
import urllib.error
import urllib.request
import uuid

from agent.terminal_env_provider import TerminalEnvironmentProvider
from tools.environments.ssh import SSHEnvironment

log = logging.getLogger(__name__)


def session_key():
    from gateway.session_context import get_session_env
    return get_session_env("HERMES_SESSION_KEY", "")


def multica_workdir(home=None):
    """The daemon's overlay identifies the task; cwd may change during a run."""
    from hermes_cli.config import get_hermes_home
    home = Path(home) if home is not None else get_hermes_home()
    root = Path('/opt/data/.multica/workspaces')
    if not home.is_relative_to(root):
        return None
    relative = home.relative_to(root)
    if len(relative.parts) != 3 or relative.parts[-1] != 'hermes-home':
        raise ValueError('Multica execution requires a managed task home')
    workdir = home.parent / 'workdir'
    if not workdir.is_dir() or workdir.resolve() != workdir:
        raise ValueError('Multica workdir is missing or traverses a symlink')
    return str(workdir)


def invocation_cwd(cwd, task_id):
    """Recover an explicitly declared controller workspace after Hermes's
    generic container guard has sanitized a host-looking cwd."""
    from agent.runtime_cwd import scoped_session_cwd, scope_terminal_cwd
    from tools.terminal_tool import get_session_cwd
    declared = get_session_cwd(task_id) or scoped_session_cwd()
    if not declared:
        configured = scope_terminal_cwd()
        if configured and Path(configured).is_relative_to('/opt/data/workspaces'):
            declared = configured
    # Native lazy environment creation passes config.cwd even when an explicit
    # session override exists. Both remote and controller profile roots matter.
    if declared and any(Path(declared).is_relative_to(root)
                        for root in ('/workspace', '/opt/data/workspaces')):
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
        context = ssl.create_default_context(cafile=self.settings.get('ca_file', '/etc/execution-manager/tls/ca.crt'))
        request = urllib.request.Request(url + path,
            data=json.dumps(body).encode() if body is not None else None,
            headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"})
        with urllib.request.urlopen(request, timeout=timeout, context=context) as response:
            return json.load(response)

    def acquire(self, session, parent_lease=None):
        # Idempotent retry protects against a lost response after the manager grants a lease.
        payload = {key: self.settings[key] for key in ("profile", "mode")}
        payload.update(session=session, request=uuid.uuid4().hex)
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
        self.multica_workdir = multica_workdir(self.runtime_home)
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
        self._initial_cwd, self._initial_timeout = self.multica_workdir or cwd, timeout
        self.managed_generation = None
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
        endpoint = reply['ssh']
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
        if self.managed_generation != reply['generation']:
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
        # IPs/NodePorts can be reused after recycle. Pin the lookup to this managed generation;
        # accept-new records the first key and rejects a changed key within that generation.
        alias = "hermes-managed-" + self.managed_environment + "-" + self.managed_generation
        return super()._target_flags(port_flag) + ["-o", "HostKeyAlias=" + alias]

    def _snapshot_excluded_passthrough_names(self):
        # The built-in SSH transport only excludes these in multiplex mode. ACP and direct
        # CLI runs also need missing task tokens to disappear instead of surviving a snapshot.
        from tools.env_passthrough import get_all_passthrough
        self._snapshot_passthrough_names.update(get_all_passthrough())
        return tuple(sorted(self._snapshot_passthrough_names))

    def _wrap_command(self, command, cwd):
        remote_home = (str(Path(self.multica_workdir).parent / 'hermes-home')
                       if self.multica_workdir else '/opt/data/profiles/' + self.settings['profile'])
        command = 'export HERMES_HOME=' + shlex.quote(remote_home) + '\n' + command
        if self.multica_workdir:
            identity = hashlib.sha256(self.managed_task_key.encode()).hexdigest()
            config_root = '/tmp/hermes-multica/' + identity
            # Apply after SSH passthrough/snapshot restoration. The controller's
            # task config and sibling task roots are not mounted into execution.
            values = {'MULTICA_TASK_CONFIG_ROOT': config_root,
                      'MULTICA_TASK_WORKSPACES_ROOT': self.multica_workdir,
                      'MULTICA_DAEMON_PORT': '19514', 'MULTICA_REPO_CHECKOUT_MODE': 'isolated'}
            exports = ' '.join(key + '=' + shlex.quote(value) for key, value in values.items())
            command = ('mkdir -p ' + shlex.quote(config_root) + ' && chmod 700 '
                       + shlex.quote(config_root) + ' || exit 1\nexport ' + exports + '\n' + command)
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
                        self.client.call('/v1/sessions/close', {
                            'profile': self.settings['profile'],
                            'session': self.session, 'runtime': self.runtime}, timeout=15)
                    except Exception:
                        log.warning('Execution session close failed; idle expiry will reclaim it')
                with self.provider._guard:
                    self.provider._environments.discard(self)


class Provider(TerminalEnvironmentProvider):
    name = "managed_execution"
    display_name = "Managed execution (SSH)"
    is_remote = True
    is_container = True
    session_isolated_when_nonpersistent = True
    skip_container_guards = False
    cache_path_base = "~/.hermes"
    strip_env_keys = frozenset({"EXECUTION_MANAGER_TOKEN"})

    def __init__(self, ctx):
        self.ctx = ctx
        self._guard = threading.RLock()
        self._environments = weakref.WeakSet()
        self._turns = {}
        self._creation_locks = weakref.WeakValueDictionary()

    def settings(self):
        defaults = dict(manager_url="https://execution-manager:8090", token_file="/run/secrets/execution-manager-token",
                        ca_file='/etc/execution-manager/tls/ca.crt', profile="", mode="per_session",
                        ssh_key="/opt/ssh/id_ed25519", acquire_timeout_seconds=180)
        settings = {key: self.ctx.get_config(key, value) for key, value in defaults.items()}
        # Existing native profiles preserve their config across image updates.
        # Upgrade the former built-in endpoint to the encrypted transport.
        if settings['manager_url'].rstrip('/') == 'http://execution-manager:8090':
            settings['manager_url'] = defaults['manager_url']
        from gateway.session_context import get_session_env
        session_profile = get_session_env('HERMES_SESSION_PROFILE', '')
        # A Multica overlay is a "custom" Hermes home, but its derived plugin
        # configuration still names the administrator-approved execution role.
        if session_profile in ('custom', 'default'):
            session_profile = ''
        settings["profile"] = (session_profile
                               or settings["profile"] or self.ctx.profile_name)
        if settings["mode"] not in ("per_session", "per_profile_on_demand", "per_profile_always_running"):
            raise ValueError("Invalid managed_execution mode")
        return settings

    def is_available(self):
        try:
            return Path(self.settings()["token_file"]).is_file()
        except (ValueError, OSError):
            return False

    def create_environment(self, *, cwd, timeout, task_id="default", image=None, container_config=None, **kwargs):
        settings = self.settings()
        cwd = invocation_cwd(cwd, task_id)
        from hermes_cli.config import get_hermes_home
        home = str(get_hermes_home())
        workdir = multica_workdir(home)
        runtime = {'hermes_home': home, 'workdir': workdir or cwd}
        if workdir or cwd not in ('/workspace', '/opt/data/workspaces/' + settings['profile']):
            # A profile-wide container cannot carry different task mounts safely.
            settings['mode'] = 'per_session'
        # Hermes uses both <session-id> (terminal calls) and session:<session-id>
        # (file/prompt calls without a task ID). Canonicalize inside the provider,
        # so its supported factory returns one transport for that conversation.
        key = task_id.removeprefix('session:')
        if key != 'prompt-backend-probe' and workdir and os.environ.get('MULTICA_TASK_ID'):
            key = 'multica:' + os.environ['MULTICA_TASK_ID']
        identity = (settings['manager_url'], settings['profile'], settings['mode'], key,
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
