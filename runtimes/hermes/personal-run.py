"""Run the native dashboard and always-running gateway as a non-root process.

The approved Pod mounts only this owner's home and selected runtime credentials.
Initialization keys remain exclusive to its init container.
"""

import hashlib
import json
import os
import re
import signal
import ssl
import subprocess
import sys
import tempfile
import time


def read(path, maximum):
    with open(path, "rb") as source:
        value = source.read(maximum + 1)
    if not value or len(value) > maximum:
        raise ValueError("invalid_personal_runtime_input")
    return value


def environment():
    if os.geteuid() == 0:
        raise ValueError("personal_runtime_requires_non_root")
    context = json.loads(read("/run/hermes/owner/context.json", 16384))
    owner = context["owner"]
    receipt = json.loads(
        read("/opt/data/.agent-platform/native-init-receipt.json", 16384)
    )
    binding = receipt["record"]["intent"]["binding"]
    expected = {
        "schemaVersion": 1,
        "installationId": context["installationId"],
        "instanceId": context["instanceId"],
        "materialId": context["materialId"],
        "ownerDigest": hashlib.sha256(
            json.dumps(
                owner, sort_keys=True, separators=(",", ":"), ensure_ascii=False
            ).encode()
        ).hexdigest(),
    }
    if binding != expected or receipt["record"]["complete"] is not True:
        raise ValueError("personal_runtime_home_conflict")
    token = (
        read("/run/hermes/credentials/dashboard-session-token", 128).decode().strip()
    )
    if not re.fullmatch(r"[a-f0-9]{64}", token):
        raise ValueError("invalid_personal_runtime_key")
    env = dict(os.environ)
    if env.get("HERMES_DASHBOARD_OIDC_ISSUER") != owner["issuer"] or not env.get(
        "HERMES_DASHBOARD_OIDC_CLIENT_ID"
    ):
        raise ValueError("personal_runtime_auth_conflict")
    env.update(
        {
            "HOME": "/opt/data",
            "HERMES_HOME": "/opt/data",
            "PYTHONDONTWRITEBYTECODE": "1",
            "HERMES_PERSONAL_INSTANCE_ID": context["instanceId"],
            "HERMES_DASHBOARD_EXPECTED_ISSUER": owner["issuer"],
            "HERMES_DASHBOARD_EXPECTED_SUBJECT": owner["subject"],
            "HERMES_DASHBOARD_SESSION_TOKEN": token,
            "API_SERVER_ENABLED": "false",
        }
    )
    if "HERMES_MANAGED_DIR" in env:
        if env["HERMES_MANAGED_DIR"] != "/run/hermes/execution-ca":
            raise ValueError("personal_runtime_managed_config_conflict")
        managed = json.loads(read("/run/hermes/execution-ca/config.yaml", 16384))
        if not isinstance(managed, dict):
            raise ValueError("personal_runtime_managed_config_invalid")
    for key in list(env):
        if key.startswith("HERMES_DASHBOARD_BASIC_AUTH_") or key.startswith(
            "HERMES_DASHBOARD_OAUTH_"
        ):
            del env[key]
    extra_ca = env.pop("HERMES_PERSONAL_OIDC_CA_FILE", None)
    if extra_ca is not None:
        if extra_ca != "/run/hermes/execution-ca/oidc-ca.crt":
            raise ValueError("personal_runtime_trust_conflict")
        # Retain public roots for independent model-provider onboarding. The
        # private identity-provider CA augments them instead of replacing them.
        trust = ssl.create_default_context()
        trust.load_verify_locations(cadata=read(extra_ca, 65536).decode())
        with tempfile.NamedTemporaryFile(
            prefix="hermes-ca-", suffix=".pem", delete=False
        ) as output:
            for cert in trust.get_ca_certs(binary_form=True):
                output.write(ssl.DER_cert_to_PEM_cert(cert).encode())
            output.flush()
            os.fsync(output.fileno())
            temporary_bundle = output.name
        bundle = "/tmp/hermes-ca.pem"
        os.replace(temporary_bundle, bundle)
        for name in ("SSL_CERT_FILE", "REQUESTS_CA_BUNDLE", "CURL_CA_BUNDLE"):
            env[name] = bundle
    return env


def run():
    env = environment()
    prepared = subprocess.run(
        [sys.executable, "/usr/local/lib/runtime-init.py"],
        env=env,
        capture_output=True,
        timeout=30,
    )
    if prepared.returncode:
        raise ValueError("personal_runtime_adapters_unavailable")
    stop = False

    def stopping(_signal, _frame):
        nonlocal stop
        stop = True

    signal.signal(signal.SIGTERM, stopping)
    signal.signal(signal.SIGINT, stopping)
    children = []
    status = 0
    try:
        for command in [
            ["/opt/hermes/.venv/bin/hermes", "gateway", "run"],
            [
                "/opt/hermes/.venv/bin/hermes",
                "dashboard",
                "--host",
                "0.0.0.0",
                "--port",
                "9119",
                "--no-open",
            ],
        ]:
            # Native commands can print login URLs or request details. Their
            # process output is never copied into shared manager/container logs.
            children.append(
                subprocess.Popen(
                    command,
                    env=env,
                    cwd="/opt/data",
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    start_new_session=True,
                )
            )
        while not stop:
            if any(child.poll() is not None for child in children):
                status = 1
                break
            time.sleep(0.25)
    finally:
        for child in children:
            if child.poll() is None:
                signal_group(child, signal.SIGTERM)
        deadline = time.monotonic() + 20
        for child in children:
            try:
                child.wait(timeout=max(0.1, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                signal_group(child, signal.SIGKILL)
                child.wait(timeout=5)
    return status


def signal_group(child, signum):
    try:
        os.killpg(child.pid, signum)
    except ProcessLookupError:
        # Exit can race the poll; still reap this exact child below.
        pass


if __name__ == "__main__":
    try:
        status = run()
    except (OSError, ValueError, TypeError, KeyError, subprocess.SubprocessError):
        raise SystemExit(
            "Personal runtime unavailable; private details suppressed."
        ) from None
    raise SystemExit(status)
