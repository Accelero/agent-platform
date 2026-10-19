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
        public_ca = (
            ROOT
            / "services/hermes-manager/crates/hermes-adapters"
            / "tests/fixtures/template-ca.crt"
        )
        (fixture / "trust/oidc-ca.crt").write_bytes(public_ca.read_bytes())
        # No model key/login, shared service credentials, prompts or Multica seed.
        settings = {
            "manager_url": "https://manager.example.org/execution",
            "token_file": "/run/hermes/credentials/execution-client-token",
            "ca_file": "/run/hermes/execution-ca/ca.crt",
            "profile": "default",
            "mode": "per_session",
            "ssh_key": "/run/hermes/credentials/id_ed25519",
            "acquire_timeout_seconds": 180,
        }
        config = {
            "terminal": {"backend": "managed_execution", "cwd": "/workspace"},
            "plugins": {
                "enabled": ["managed_execution"],
                "entries": {"managed_execution": {"settings": settings}},
            },
        }
        (fixture / "init/native-config.json").write_text(json.dumps(config))
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
        run(
            [
                "docker",
                "run",
                "--rm",
                *common,
                "--mount",
                f"type=bind,src={fixture / 'init'},dst=/run/hermes/init,readonly",
                "--entrypoint",
                "/opt/hermes/.venv/bin/python",
                IMAGE,
                "/usr/local/lib/personal-init.py",
                "--context",
                "/run/hermes/owner/context.json",
            ],
            timeout=60,
        )
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
                    "--env",
                    f"HERMES_PERSONAL_INSTANCE_ID={context['instanceId']}",
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
from hermes_cli.plugins import PluginContext, PluginManifest
context=PluginContext(PluginManifest(name='managed_execution',key='managed_execution'),None)
assert context.get_config('manager_url')=='https://manager.example.org/execution'
assert context.get_config('mode')=='per_session'
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
                "authentication, inactivity and abrupt restart preserve owner state"
            )
        finally:
            run(["docker", "rm", "--force", name], timeout=30)


if __name__ == "__main__":
    main()
