"""Real non-root native startup/restart proof for the reviewed local Hermes image.

Build personal-hermes-native-test:local from the finite release context first.
Fixture credentials stay in a temporary directory and are never printed.
"""

import argparse
import json
import os
import secrets
import tempfile
import time
import uuid
from pathlib import Path

from agent_platform.command import run
from agent_platform.privacy import ROOT, stage_contexts

IMAGE = "personal-hermes-native-test:local"
ISSUER = "https://sso.example.org/realms/agents"


def missing_instance_policies_are_refused():
    """The built image cannot fall back to unowned authentication or execution."""
    run(
        [
            "docker",
            "run",
            "--rm",
            "--network",
            "none",
            "--read-only",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--user",
            "1000:1000",
            "--tmpfs",
            "/tmp:mode=1777",
            "--env",
            "PYTHONDONTWRITEBYTECODE=1",
            "--env",
            "HERMES_HOME=/tmp/home",
            "--entrypoint",
            "/opt/hermes/.venv/bin/python",
            IMAGE,
            "-c",
            """
import os
from hermes_cli._platform_terminal_policy import enforce_backend, managed_directory
from plugins.dashboard_auth.self_hosted import SelfHostedOIDCProvider
# Environment pins cannot stand in for the mounted owner policy.
os.environ['HERMES_DASHBOARD_EXPECTED_ISSUER'] = 'https://sso.example.org/realms/agents'
os.environ['HERMES_DASHBOARD_EXPECTED_SUBJECT'] = 'owner-a'
for operation in [
    lambda: enforce_backend('local'),
    lambda: enforce_backend('managed_execution'),
    managed_directory,
    lambda: SelfHostedOIDCProvider(
        issuer='https://sso.example.org/realms/agents', client_id='native-hermes'),
]:
    try:
        operation()
    except (RuntimeError, ValueError):
        pass
    else:
        raise AssertionError('Missing instance policy was accepted')
""",
        ],
        timeout=30,
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build", action="store_true")
    args = parser.parse_args()
    if args.build:
        with tempfile.TemporaryDirectory(prefix="personal-hermes-build-") as directory:
            context = stage_contexts(ROOT, Path(directory) / "inputs")["hermes"]
            run(
                [
                    "docker",
                    "build",
                    "--file",
                    str(context / "runtimes/hermes/Dockerfile"),
                    "--tag",
                    IMAGE,
                    str(context),
                ],
                timeout=1200,
            )
    missing_instance_policies_are_refused()
    name = "personal-hermes-runtime-" + uuid.uuid4().hex[:12]
    with tempfile.TemporaryDirectory(prefix="personal-hermes-native-") as directory:
        fixture = Path(directory)
        fixture.chmod(0o755)
        for part in ["home", "workspace", "owner", "init", "credentials", "trust"]:
            (fixture / part).mkdir(mode=0o755)
            os.chown(fixture / part, 1000, 1000)
        context = {
            "schemaVersion": 1,
            "installationId": str(uuid.uuid4()),
            "instanceId": str(uuid.uuid4()),
            "materialId": str(uuid.uuid4()),
            "owner": {"issuer": ISSUER, "subject": "owner-a"},
            "template": {"id": "personal-default", "version": 1},
        }
        (fixture / "owner/context.json").write_text(json.dumps(context))
        (fixture / "init/init-key").write_text(secrets.token_hex(32))
        token = secrets.token_hex(32)
        (fixture / "credentials/dashboard-session-token").write_text(token)
        # A throwaway CA; its key stays outside every mounted directory.
        with tempfile.TemporaryDirectory(prefix="personal-hermes-ca-") as ca:
            run(
                [
                    "openssl",
                    "req",
                    "-x509",
                    "-newkey",
                    "ec",
                    "-pkeyopt",
                    "ec_paramgen_curve:P-256",
                    "-nodes",
                    "-keyout",
                    str(Path(ca) / "ca.key"),
                    "-out",
                    str(fixture / "trust/oidc-ca.crt"),
                    "-days",
                    "1",
                    "-subj",
                    "/CN=personal-hermes-test-ca",
                    "-addext",
                    "basicConstraints=critical,CA:TRUE",
                ],
                timeout=30,
            )
        # No model key or login, shared service credentials or prompts.
        settings = {
            "manager_url": "https://manager.example.org/execution",
            "token_file": "/run/hermes/credentials/execution-client-token",
            "ca_file": "/run/hermes/execution-ca/ca.crt",
            "mode": "per_session",
            "ssh_key": "/run/hermes/credentials/id_ed25519",
            "acquire_timeout_seconds": 180,
        }
        config = {
            "terminal": {"backend": "managed_execution", "cwd": "/workspace"},
            "plugins": {
                "enabled": ["managed_execution"],
                "disabled": [],
                "entries": {"managed_execution": {"settings": settings}},
            },
        }
        (fixture / "init/native-config.json").write_text(json.dumps(config))
        managed = {
            "terminal": config["terminal"],
            "agent": {"max_turns": 12},
            "plugins": config["plugins"],
        }
        (fixture / "trust/config.yaml").write_text(json.dumps(managed))
        (fixture / "trust/.env").write_text("TERMINAL_ENV=managed_execution\n")
        # Kubelet projects a writable file at terminationMessagePath. Exercise
        # the actual bind-mount EBUSY behavior, not an ordinary temporary file.
        result_file = fixture / "init-result.json"
        result_file.touch(mode=0o600)
        os.chown(result_file, 1000, 1000)
        common = [
            "--network",
            "none",
            "--read-only",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--user",
            "1000:1000",
            "--tmpfs",
            "/tmp:mode=1777",
            "--env",
            "PYTHONDONTWRITEBYTECODE=1",
            "--env",
            "HERMES_MANAGED_DIR=/run/hermes/execution-ca",
        ]
        for part, target in [
            ("home", "/opt/data"),
            ("workspace", "/workspace"),
            ("owner", "/run/hermes/owner"),
            ("trust", "/run/hermes/execution-ca"),
            ("credentials", "/run/hermes/credentials"),
        ]:
            mount = f"type=bind,src={fixture / part},dst={target}"
            if part in {"owner", "credentials", "trust"}:
                mount += ",readonly"
            common.extend(["--mount", mount])
        # OpenSSH rejects numeric users missing from passwd before any network
        # connection. Exercise the actual client as the Pod's non-root UID.
        run(
            [
                "docker",
                "run",
                "--rm",
                *common,
                "--entrypoint",
                "/usr/bin/ssh",
                IMAGE,
                "-G",
                "-F",
                "/dev/null",
                "worker@fixture.invalid",
            ],
            timeout=20,
        )
        run(
            [
                "docker",
                "run",
                "--rm",
                *common,
                "--mount",
                f"type=bind,src={fixture / 'init'},dst=/run/hermes/init,readonly",
                "--mount",
                f"type=bind,src={result_file},dst=/tmp/native-init-result.json",
                "--entrypoint",
                "/opt/hermes/.venv/bin/python",
                IMAGE,
                "/usr/local/lib/personal-init.py",
                "--context",
                "/run/hermes/owner/context.json",
            ],
            timeout=60,
        )
        assert json.loads(result_file.read_text())["record"]["complete"] is True
        try:
            run(
                [
                    "docker",
                    "run",
                    "-d",
                    "--name",
                    name,
                    *common,
                    "--env",
                    f"HERMES_DASHBOARD_OIDC_ISSUER={ISSUER}",
                    "--env",
                    "HERMES_DASHBOARD_OIDC_CLIENT_ID=native-hermes",
                    "--env",
                    "HERMES_DASHBOARD_PUBLIC_URL=https://hermes.example.org",
                    "--env",
                    "HERMES_PERSONAL_OIDC_CA_FILE=/run/hermes/execution-ca/oidc-ca.crt",
                    "--entrypoint",
                    "/opt/hermes/.venv/bin/python",
                    IMAGE,
                    "/usr/local/lib/personal-run.py",
                ],
                timeout=60,
            )
            check = """
import json, os, pathlib, urllib.request, urllib.error
os.environ['HERMES_HOME']='/opt/data'
from hermes_cli.config import load_config
managed=json.loads(pathlib.Path('/run/hermes/execution-ca/config.yaml').read_text())
effective=load_config()
assert effective['agent']['max_turns']==managed['agent']['max_turns']
raw=json.loads(pathlib.Path('/opt/data/config.yaml').read_text())
if 'fixture_preference' in raw.get('agent',{}):
    assert effective['agent']['fixture_preference']=='owner choice'
    assert raw['agent']['max_turns']==77
from hermes_cli.plugins import PluginContext, PluginManifest
context=PluginContext(PluginManifest(name='managed_execution',key='managed_execution'),None)
assert context.get_config('manager_url')=='https://manager.example.org/execution'
assert context.get_config('mode')=='per_session'
from hermes_cli.managed_scope import get_managed_dir
os.environ['HERMES_MANAGED_DIR']='/tmp/owner-selected-policy'
os.environ['TERMINAL_ENV']='local'
assert get_managed_dir()==pathlib.Path('/run/hermes/execution-ca')
from tools.terminal_scope import (set_terminal_scope, reset_terminal_scope,
    terminal_env, TerminalPolicyRefusal, TerminalPolicyUnavailable)
for scope in [None, {'TERMINAL_ENV':'local'}, {'TERMINAL_ENV':'ssh'}]:
    token_scope=set_terminal_scope(scope)
    try:
        assert terminal_env('TERMINAL_ENV')=='managed_execution'
    finally:
        reset_terminal_scope(token_scope)
token_scope=set_terminal_scope(TerminalPolicyRefusal('fixture'))
try:
    try:
        terminal_env('TERMINAL_ENV')
    except TerminalPolicyUnavailable:
        pass
    else:
        raise AssertionError('Refused profile scope was bypassed')
finally:
    reset_terminal_scope(token_scope)
from tools.terminal_tool_backends import _create_environment
for backend in ['local','docker','ssh']:
    try:
        _create_environment(backend,'','/workspace',5)
    except ValueError as error:
        assert 'require the managed_execution' in str(error)
    else:
        raise AssertionError('Unmanaged terminal backend was allowed')
from hermes_cli.plugins_discovery import (collect_directory_manifests,
    resolve_manifest_winners, gate_manifest)
shadow=pathlib.Path('/opt/data/plugins/managed_execution')
shadow.mkdir(parents=True,exist_ok=True)
(shadow/'plugin.yaml').write_text('name: managed_execution\\nversion: 0.0.1\\n')
winners=resolve_manifest_winners(collect_directory_manifests())
pinned=winners['managed_execution']
assert pinned.source=='bundled'
assert pathlib.Path(pinned.path)==pathlib.Path('/opt/hermes/plugins/managed_execution')
gate=gate_manifest(pinned,{'managed_execution'},set())
assert gate.action=='load' and gate.enabled
from hermes_cli._platform_terminal_policy import load_policy
assert load_policy()['terminal']['backend']=='managed_execution'
profile=pathlib.Path('/tmp/policy-profile')
profile.mkdir(exist_ok=True)
(profile/'config.yaml').write_text(json.dumps({
    'terminal':{'backend':'local','runtime_id':'python-tools'},
    'plugins':{'enabled':[],'disabled':['managed_execution']}}))
(profile/'.env').write_text('TERMINAL_ENV=ssh\\n')
os.environ['HERMES_HOME']=str(profile)
try:
    selected=load_config()
    assert selected['terminal']['backend']=='managed_execution'
    assert selected['terminal']['runtime_id']=='python-tools'
    assert 'managed_execution' in selected['plugins']['enabled']
    assert 'managed_execution' not in selected['plugins']['disabled']
    from tools.terminal_scope import build_profile_terminal_scope
    profile_scope=build_profile_terminal_scope(profile)
    assert profile_scope['TERMINAL_ENV']=='managed_execution'
    token_scope=set_terminal_scope(profile_scope)
    try:
        assert terminal_env('TERMINAL_ENV')=='managed_execution'
    finally:
        reset_terminal_scope(token_scope)
    import importlib.util
    from types import SimpleNamespace
    spec=importlib.util.spec_from_file_location('policy_fixture_provider',
        '/opt/hermes/plugins/managed_execution/__init__.py')
    module=importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    provider=module.Provider(SimpleNamespace(profile_name='research',
        get_config=lambda key,default: {'manager_url':'https://owner.invalid',
            'token_file':'/tmp/untrusted-token','ssh_key':'/tmp/untrusted-key'}.get(key,default)))
    settings=provider.settings()
    connection=managed['plugins']['entries']['managed_execution']['settings']
    for key in ['manager_url','token_file','ca_file','ssh_key']:
        assert settings[key]==connection[key]
    assert settings['runtime_id']=='python-tools'
    assert settings['mode']=='per_session' and 'profile' not in settings
finally:
    os.environ['HERMES_HOME']='/opt/data'
response=urllib.request.urlopen('http://127.0.0.1:9119/api/health',timeout=3)
assert response.status==200
body=response.read(2**20)
token=pathlib.Path('/run/hermes/credentials/dashboard-session-token').read_bytes()
assert token not in body
providers=json.load(urllib.request.urlopen(
    'http://127.0.0.1:9119/api/auth/providers',timeout=3))
assert [p['name'] for p in providers['providers']]==['self-hosted']
class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self,*args,**kwargs): return None
try:
    urllib.request.build_opener(NoRedirect()).open(
        'http://127.0.0.1:9119/',timeout=3)
except urllib.error.HTTPError as error:
    assert error.code==302
    assert error.headers['Location'].startswith('/auth/login?provider=self-hosted&')
else:
    raise AssertionError('Native root authentication was bypassed')
for path in ['/api/auth/me','/api/sessions']:
    try:
        urllib.request.urlopen('http://127.0.0.1:9119'+path,timeout=3)
    except urllib.error.HTTPError as error:
        assert error.code==401
    else:
        raise AssertionError('Native authentication was bypassed')
commands=[(p/'cmdline').read_bytes()
    for p in pathlib.Path('/proc').iterdir()
    if p.name.isdigit() and p.name not in {'1',str(__import__('os').getpid())}]
assert any(b'gateway\\x00run' in c for c in commands)
assert any(b'dashboard\\x00--host' in c for c in commands)
assert not pathlib.Path('/run/hermes/init/init-key').exists()
bundles=list(pathlib.Path('/tmp').glob('hermes-ca*.pem'))
assert bundles
assert all(p.read_bytes().count(b'BEGIN CERTIFICATE')>1 for p in bundles)
"""
            for attempt in range(2):
                deadline = time.monotonic() + 90
                while True:
                    running = run(
                        ["docker", "inspect", "--format", "{{.State.Running}}", name],
                        timeout=10,
                    ).strip()
                    if running != b"true":
                        raise RuntimeError("Native supervisor exited before readiness")
                    try:
                        run(
                            [
                                "docker",
                                "exec",
                                name,
                                "/opt/hermes/.venv/bin/python",
                                "-c",
                                check,
                            ],
                            timeout=10,
                        )
                        break
                    except RuntimeError:
                        if time.monotonic() >= deadline:
                            raise RuntimeError(
                                "Native startup readiness deadline exceeded"
                            ) from None
                        time.sleep(1)
                # Readiness is not a substitute for a continuously running gateway.
                time.sleep(10)
                run(
                    [
                        "docker",
                        "exec",
                        name,
                        "/opt/hermes/.venv/bin/python",
                        "-c",
                        check,
                    ],
                    timeout=10,
                )
                if attempt == 0:
                    # Dashboard writes belong to the retained user layer. An
                    # infrastructure update must override only managed leaves.
                    settings_path = fixture / "home/config.yaml"
                    settings = json.loads(settings_path.read_text())
                    settings["agent"] = {
                        "max_turns": 77,
                        "fixture_preference": "owner choice",
                    }
                    settings_path.write_text(json.dumps(settings))
                    managed["agent"]["max_turns"] = 24
                    (fixture / "trust/config.yaml").write_text(json.dumps(managed))
                    (fixture / "home/owner-memory.txt").write_text(
                        "owner fixture memory"
                    )
                    run(["docker", "kill", "--signal", "KILL", name], timeout=15)
                    run(["docker", "start", name], timeout=30)
            assert (
                fixture / "home/owner-memory.txt"
            ).read_text() == "owner fixture memory"
            print(
                "PASS: real non-root native gateway/dashboard; no model credentials; "
                "admin terminal policy, authentication, inactivity and abrupt "
                "restart preserve owner state"
            )
        finally:
            run(["docker", "rm", "--force", name], timeout=30)


if __name__ == "__main__":
    main()
