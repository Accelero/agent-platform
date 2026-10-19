"""Exercise the compiled manager process using disposable PostgreSQL and TLS.

Run after cargo build --workspace --bins --locked. Requires an existing
disposable PostgreSQL container (the Checks job supplies its service container).
All configuration, keys and registry records are ephemeral fixtures.
"""

import argparse
import http.client
import json
import socket
import ssl
import subprocess
import tempfile
import time
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BINARY = ROOT / "services/hermes-manager/target/debug/hermes-manager"
ROUTER_BINARY = ROOT / "services/hermes-manager/target/debug/hermes-router-auth"


def command(args, **kwargs):
    return subprocess.run(args, capture_output=True, text=True, timeout=30, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--postgres-container", default="personal-hermes-manager-tests")
    args = parser.parse_args()
    assert BINARY.is_file(), "Build hermes-manager before running the process test"
    assert ROUTER_BINARY.is_file(), "Build hermes-router-auth before the process test"
    database = "hermes_process_" + uuid.uuid4().hex

    def sql(statement, target="hermes_test"):
        result = command(
            [
                "docker",
                "exec",
                "-i",
                args.postgres_container,
                "psql",
                "-U",
                "hermes_test",
                "-d",
                target,
                "-v",
                "ON_ERROR_STOP=1",
                "-f",
                "-",
            ],
            input=statement,
        )
        assert result.returncode == 0, "Disposable database fixture command failed"

    sql(f"CREATE DATABASE {database};")
    try:
        ports = command(["docker", "port", args.postgres_container, "5432/tcp"])
        assert ports.returncode == 0
        # The test database is bound to loopback; the disposable fixture uses
        # trust authentication, with no secret in command-line arguments.
        host_port = ports.stdout.strip().splitlines()[0].rsplit(":", 1)[1]
        with tempfile.TemporaryDirectory(prefix="hermes-api-process-") as temporary:
            directory = Path(temporary)
            ca = directory / "tls.crt"
            key = directory / "tls.key"
            result = command(
                [
                    "openssl",
                    "req",
                    "-x509",
                    "-newkey",
                    "rsa:2048",
                    "-nodes",
                    "-keyout",
                    str(key),
                    "-out",
                    str(ca),
                    "-days",
                    "1",
                    "-subj",
                    "/CN=hermes-api-fixture",
                    "-addext",
                    "subjectAltName=IP:127.0.0.1",
                    "-addext",
                    "basicConstraints=critical,CA:FALSE",
                    "-addext",
                    "extendedKeyUsage=serverAuth",
                ]
            )
            assert result.returncode == 0
            with socket.socket() as admin_socket, socket.socket() as route_socket:
                admin_socket.bind(("127.0.0.1", 0))
                route_socket.bind(("127.0.0.1", 0))
                admin_port = admin_socket.getsockname()[1]
                route_port = route_socket.getsockname()[1]
            database_file = directory / "database-url"
            database_file.write_text(
                f"postgres://hermes_test@127.0.0.1:{host_port}/{database}"
            )
            database_file.chmod(0o600)
            encryption_key = directory / "session-key"
            encryption_key.write_bytes(bytes(range(32)))
            encryption_key.chmod(0o600)
            keyring = directory / "keyring.json"
            keyring.write_text(
                json.dumps(
                    {
                        "activeKeyId": "fixture",
                        "keyFiles": {"fixture": str(encryption_key)},
                    }
                )
            )
            session_config = {
                "databaseUrlFile": str(database_file),
                "publicOrigin": "https://admin.example.org",
                "otherOrigin": "https://hermes.example.org",
                "clientId": "manager-admin",
                "scopes": ["openid"],
                "encryptionKeyringFile": str(keyring),
            }
            installation = str(uuid.uuid4())
            config = {
                "installation": {
                    "id": installation,
                    "epoch": 1,
                    "issuer": "https://sso.example.org/realms/agents",
                },
                "databaseUrlFile": str(database_file),
                "oidc": {
                    "jwksUrl": "https://sso.example.org/realms/agents/protocol/openid-connect/certs"
                },
                "admin": {
                    "listen": f"127.0.0.1:{admin_port}",
                    "tls": {"certificateFile": str(ca), "privateKeyFile": str(key)},
                    "audience": "manager-api",
                    "allowedClients": ["manager-admin"],
                    "managementClient": "manager-admin",
                    "administratorRole": "platform-administrator",
                    "browser": session_config,
                },
                "resolver": {
                    "listen": f"127.0.0.1:{route_port}",
                    "tls": {"certificateFile": str(ca), "privateKeyFile": str(key)},
                    "clientCaFile": str(ca),
                    "serviceIdentity": "spiffe://agents.example.org/router-plugin",
                    "audience": "route-api",
                    "authorizedClient": "hermes-router",
                },
            }
            config_file = directory / "manager.json"
            config_file.write_text(json.dumps(config))
            base_args = [str(BINARY), "check", "--config", str(config_file)]
            assert command(base_args).returncode != 0
            assert (
                command(
                    [str(BINARY), "migrate", "--config", str(config_file)]
                ).returncode
                == 0
            )
            assert command(base_args).returncode != 0, (
                "Missing registry self-bootstrapped"
            )
            sql(
                "INSERT INTO manager.installations "
                "(id,issuer,schema_version,template_schema_version,epoch,mode,"
                "cluster_identity,fresh_inventory_digest) VALUES "
                f"('{installation}','https://sso.example.org/realms/agents',"
                f"1,1,1,'active','process-fixture','{'a' * 64}');",
                database,
            )
            assert command(base_args).returncode == 0
            original_audience = config["admin"]["audience"]
            config["admin"]["audience"] = config["resolver"]["audience"]
            config_file.write_text(json.dumps(config))
            assert (
                command([str(BINARY), "api", "--config", str(config_file)]).returncode
                != 0
            ), "API accepted overlapping administration/route audiences"
            config["admin"]["audience"] = original_audience
            config_file.write_text(json.dumps(config))
            process = subprocess.Popen(
                [str(BINARY), "api", "--config", str(config_file)],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            try:
                tls = ssl.create_default_context(cafile=str(ca))

                def get(path, port=admin_port):
                    connection = http.client.HTTPSConnection(
                        "127.0.0.1", port, context=tls, timeout=1
                    )
                    try:
                        connection.request("GET", path)
                        response = connection.getresponse()
                        return (
                            response.status,
                            response.read(),
                            response.getheader("Cache-Control"),
                        )
                    finally:
                        connection.close()

                deadline = time.monotonic() + 10
                while time.monotonic() < deadline:
                    assert process.poll() is None, "API process failed startup"
                    try:
                        if get("/healthz")[0] == 200:
                            break
                    except OSError, http.client.HTTPException:
                        pass
                    time.sleep(0.05)
                else:
                    raise AssertionError("API process never became healthy")
                status, body, cache = get("/api/v1/assignments")
                assert status == 401 and cache == "no-store"
                assert json.loads(body) == {"error": "invalid_identity_evidence"}
                assert get("/internal/v1/routes/resolve")[0] == 404
                connection = http.client.HTTPSConnection(
                    "127.0.0.1", route_port, context=tls, timeout=1
                )
                try:
                    try:
                        connection.request("GET", "/healthz")
                        connection.getresponse()
                    except OSError, http.client.HTTPException:
                        pass
                    else:
                        raise AssertionError(
                            "Resolver accepted a connection without mTLS"
                        )
                finally:
                    connection.close()
            finally:
                process.kill()
                process.communicate(timeout=10)
            # No graceful shutdown hook was invoked. The registry still opens
            # under the same identity and a mismatched identity remains closed.
            assert command(base_args).returncode == 0
            config["installation"]["id"] = str(uuid.uuid4())
            config_file.write_text(json.dumps(config))
            assert command(base_args).returncode != 0
            assert (
                command(
                    [str(BINARY), "migrate", "--config", str(config_file)]
                ).returncode
                != 0
            )
            router_config = {
                "installation": dict(config["installation"], id=installation),
                "oidc": dict(config["oidc"], audience="route-api"),
                "session": dict(
                    session_config,
                    publicOrigin="https://hermes.example.org",
                    otherOrigin="https://admin.example.org",
                    clientId="hermes-router",
                ),
                "public": {
                    "listen": f"127.0.0.1:{admin_port}",
                    "certificateFile": str(ca),
                    "privateKeyFile": str(key),
                },
                "internal": {
                    "listen": f"127.0.0.1:{route_port}",
                    "certificateFile": str(ca),
                    "privateKeyFile": str(key),
                    "clientCaFile": str(ca),
                    "forwardAuthIdentity": "spiffe://agents.example.org/router-forward-auth",
                    "sessionPluginIdentity": "spiffe://agents.example.org/router-plugin",
                },
            }
            router_file = directory / "router.json"
            router_file.write_text(json.dumps(router_config))
            process = subprocess.Popen(
                [str(ROUTER_BINARY), "--config", str(router_file)],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            try:
                deadline = time.monotonic() + 10
                while time.monotonic() < deadline:
                    assert process.poll() is None, "Router auth process failed startup"
                    try:
                        if get("/healthz")[0] == 200:
                            break
                    except OSError, http.client.HTTPException:
                        pass
                    time.sleep(0.05)
                else:
                    raise AssertionError("Router auth process never became healthy")
                assert get("/_platform/me")[0] == 401
                assert get("/api/v1/session")[0] == 404
                assert get("/internal/v1/forward-auth")[0] == 404
                connection = http.client.HTTPSConnection(
                    "127.0.0.1", route_port, context=tls, timeout=1
                )
                try:
                    try:
                        connection.request("GET", "/healthz")
                        connection.getresponse()
                    except OSError, http.client.HTTPException:
                        pass
                    else:
                        raise AssertionError(
                            "Router internal listener accepted missing mTLS"
                        )
                finally:
                    connection.close()
            finally:
                process.kill()
                process.communicate(timeout=10)
            router_config["installation"]["epoch"] = 2
            router_file.write_text(json.dumps(router_config))
            assert (
                command([str(ROUTER_BINARY), "--config", str(router_file)]).returncode
                != 0
            )
            print(
                "PASS: real API and router auth startup/TLS/session boundaries, "
                "abrupt kill, registry reopening and identity/epoch mismatch denial"
            )
    finally:
        sql(f"DROP DATABASE {database} WITH (FORCE);")


if __name__ == "__main__":
    main()
