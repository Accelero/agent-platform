"""Exercise the packaged dashboard chart in disposable local Docker/k3d only.

The HTTPS provider, PostgreSQL registry, identities and keys are synthetic. This
checks chart installation, auth/directory boundaries and restart persistence;
controller/native lifecycle acceptance remains in the separate native fixture.
"""

import argparse
import base64
import json
import ssl
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
K3S = (
    "rancher/k3s@sha256:"
    "ca7f37d993d82ef0dcdcfecb2e0e2618ea541dbaffc620c8cedebe01a82acd0d"
)
NODE = "node@sha256:86f07bc9c5dce4578cf37e5a418b7bfc7f817cda25cde66e2b66e95ed86c4567"
POSTGRES = (
    "postgres@sha256:2a0d0fe14825b0939f78a8cad5cd4e6aa68bf94d0e5dd96e24b6d23af4315545"
)


def command(args, *, data=None, timeout=60):
    result = subprocess.run(args, input=data, capture_output=True, timeout=timeout)
    if result.returncode:
        (ROOT / ".local/chart-fixture-error.log").write_bytes(result.stderr[-8192:])
        raise AssertionError(f"Local chart fixture step failed: {args[0]}")
    return result.stdout


def encoded(value):
    return base64.urlsafe_b64encode(value).rstrip(b"=").decode()


def run_fixture(chart, image):
    name = "dashboard-fixture-" + uuid.uuid4().hex[:8]
    node = f"k3d-{name}-server-0"
    postgres = name + "-pg"
    forward = None
    provider = None
    try:
        existing = command(["k3d", "cluster", "list", "-o", "json"])
        if not any(x["name"] == name for x in json.loads(existing)):
            command(
                [
                    "k3d",
                    "cluster",
                    "create",
                    name,
                    "--image",
                    K3S,
                    "--kubeconfig-update-default=false",
                    "--kubeconfig-switch-context=false",
                    "--no-lb",
                    "--wait",
                    "--timeout",
                    "120s",
                ],
                timeout=180,
            )
        network = f"k3d-{name}"
        command(
            [
                "docker",
                "run",
                "-d",
                "--name",
                postgres,
                "--network",
                network,
                "-e",
                "POSTGRES_HOST_AUTH_METHOD=trust",
                "-e",
                "POSTGRES_USER=hermes_test",
                "-e",
                "POSTGRES_DB=hermes_test",
                POSTGRES,
            ]
        )
        for _ in range(60):
            ready = subprocess.run(
                ["docker", "exec", postgres, "pg_isready", "-U", "hermes_test"],
                capture_output=True,
                timeout=5,
            )
            if ready.returncode == 0:
                break
            time.sleep(0.5)
        else:
            raise AssertionError("Local PostgreSQL fixture did not become ready")
        address = json.loads(command(["docker", "inspect", postgres]))[0][
            "NetworkSettings"
        ]["Networks"][network]["IPAddress"]
        node_address = json.loads(command(["docker", "inspect", node]))[0][
            "NetworkSettings"
        ]["Networks"][network]["IPAddress"]
        with tempfile.TemporaryDirectory(prefix="dashboard-chart-") as temp:
            directory = Path(temp)
            directory.chmod(0o755)
            provider = name + "-identity"
            command(
                [
                    "docker",
                    "run",
                    "-d",
                    "--name",
                    provider,
                    "--network",
                    network,
                    "--read-only",
                    "--cap-drop",
                    "ALL",
                    "--user",
                    "10001:10001",
                    "-v",
                    f"{directory}:/fixture:ro",
                    "--entrypoint",
                    "sh",
                    NODE,
                    "-c",
                    "until test -f /fixture/provider.js; do sleep 0.1; done; "
                    "exec node /fixture/provider.js",
                ]
            )
            provider_address = json.loads(command(["docker", "inspect", provider]))[0][
                "NetworkSettings"
            ]["Networks"][network]["IPAddress"]
            ca, key = directory / "ca.crt", directory / "tls.key"
            cert, ca_key = directory / "tls.crt", directory / "ca.key"
            command(
                [
                    "openssl",
                    "req",
                    "-x509",
                    "-newkey",
                    "rsa:2048",
                    "-nodes",
                    "-keyout",
                    str(ca_key),
                    "-out",
                    str(ca),
                    "-days",
                    "1",
                    "-subj",
                    "/CN=local-dashboard-ca",
                    "-addext",
                    "basicConstraints=critical,CA:TRUE",
                    "-addext",
                    "keyUsage=critical,keyCertSign,cRLSign",
                ]
            )
            command(
                [
                    "openssl",
                    "req",
                    "-new",
                    "-newkey",
                    "rsa:2048",
                    "-nodes",
                    "-keyout",
                    str(key),
                    "-out",
                    str(directory / "server.csr"),
                    "-subj",
                    "/CN=local-dashboard-fixture",
                ]
            )
            extensions = directory / "server.ext"
            extensions.write_text(
                f"subjectAltName=IP:127.0.0.1,IP:{provider_address},IP:{node_address}\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nauthorityKeyIdentifier=keyid,issuer\nsubjectKeyIdentifier=hash\n"
            )
            command(
                [
                    "openssl",
                    "x509",
                    "-req",
                    "-in",
                    str(directory / "server.csr"),
                    "-CA",
                    str(ca),
                    "-CAkey",
                    str(ca_key),
                    "-set_serial",
                    "2",
                    "-out",
                    str(cert),
                    "-days",
                    "1",
                    "-extfile",
                    str(extensions),
                ]
            )
            modulus = (
                command(["openssl", "rsa", "-in", str(key), "-noout", "-modulus"])
                .decode()
                .strip()
                .split("=", 1)[1]
            )
            n = encoded(bytes.fromhex(modulus))
            jwks = {
                "keys": [
                    {
                        "kty": "RSA",
                        "kid": "fixture",
                        "use": "sig",
                        "alg": "RS256",
                        "n": n,
                        "e": "AQAB",
                    }
                ]
            }
            installation = str(uuid.uuid4())
            owner = {
                "id": "directory-owner",
                "username": "fixture-owner",
                "firstName": "Fixture",
                "lastName": "Owner",
                "email": "fixture@example.org",
                "enabled": True,
            }

            (directory / "provider.json").write_text(
                json.dumps({"jwks": jwks, "owner": owner})
            )
            key.chmod(0o644)
            (directory / "provider.js.tmp").write_text("""
const fs = require('fs');
const https = require('https');
const {jwks, owner} = JSON.parse(fs.readFileSync('/fixture/provider.json'));
https.createServer({
  cert: fs.readFileSync('/fixture/tls.crt'),
  key: fs.readFileSync('/fixture/tls.key')
}, (req, res) => {
  const send = data => {
    const body = JSON.stringify(data);
    res.writeHead(200, {
      'Content-Type': 'application/json',
      'Content-Length': Buffer.byteLength(body)
    });
    res.end(body);
  };
  const path = new URL(req.url, 'https://fixture.invalid').pathname;
  if (req.method === 'GET' && path.endsWith('/certs')) return send(jwks);
  if (req.method === 'GET' && path === '/admin/realms/agents/users' &&
      req.headers.authorization === 'Bearer synthetic-directory-service-token') {
    return send([owner]);
  }
  if (req.method === 'POST' && path.endsWith('/token')) {
    let body = '';
    req.on('data', chunk => {
      body += chunk;
      if (body.length > 4096) req.destroy();
    });
    req.on('end', () => {
      const data = new URLSearchParams(body);
      if (data.get('client_id') === 'directory-reader' &&
          data.get('client_secret') === 'synthetic-directory-secret') {
        send({access_token: 'synthetic-directory-service-token',
              expires_in: 300, token_type: 'Bearer'});
      } else {
        res.writeHead(403); res.end();
      }
    });
    return;
  }
  res.writeHead(404); res.end();
}).listen(8443, '0.0.0.0');
""")
            (directory / "provider.js.tmp").rename(directory / "provider.js")
            issuer = f"https://{provider_address}:8443/realms/agents"
            origin = f"https://{node_address}:18443"
            root = "/run/manager/secrets/"
            config = json.loads(
                (ROOT / "services/hermes-manager/api.example.json").read_text()
            )
            config.pop("installation")
            config.pop("administratorIdentityFile", None)
            config["databaseUrlFile"] = root + "database-url"
            config["oidc"] = {
                "jwksUrl": issuer + "/protocol/openid-connect/certs",
                "caFile": root + "ca.crt",
            }
            for listener in ("admin", "resolver", "execution"):
                config[listener]["tls"] = {
                    "certificateFile": root + "tls.crt",
                    "privateKeyFile": root + "tls.key",
                }
            config["resolver"]["clientCaFile"] = root + "ca.crt"
            config["admin"]["directory"] = {
                "clientId": "directory-reader",
                "clientSecretFile": root + "directory-secret",
            }
            config["admin"]["browser"] = {
                "databaseUrlFile": root + "database-url",
                "publicOrigin": origin,
                "otherOrigin": "https://native.example.org",
                "clientId": "manager-admin",
                "scopes": ["openid"],
                "encryptionKeyringFile": root + "keyring.json",
            }
            scope = {"id": installation, "epoch": 1, "issuer": issuer}
            database = f"postgres://hermes_test@{address}:5432/hermes_test"
            files = {
                "database-url": database.encode(),
                "tls.crt": cert.read_bytes(),
                "ca.crt": ca.read_bytes(),
                "tls.key": key.read_bytes(),
                "directory-secret": b"synthetic-directory-secret",
                "session-key": bytes(range(32)),
                "keyring.json": json.dumps(
                    {
                        "activeKeyId": "fixture",
                        "keyFiles": {"fixture": root + "session-key"},
                    }
                ).encode(),
            }
            for filename, data in files.items():
                (directory / filename).write_bytes(data)
                (directory / filename).chmod(0o644)
            operator = {
                **config,
                "installation": scope,
                "databaseUrlFile": "/fixture/database-url",
            }
            (directory / "operator.json").write_text(json.dumps(operator))
            command(
                [
                    "docker",
                    "run",
                    "--rm",
                    "--network",
                    network,
                    "--read-only",
                    "--cap-drop",
                    "ALL",
                    "-v",
                    f"{directory}:/fixture:ro",
                    image,
                    "migrate",
                    "--config",
                    "/fixture/operator.json",
                ]
            )

            def sql(statement):
                return command(
                    [
                        "docker",
                        "exec",
                        "-i",
                        postgres,
                        "psql",
                        "-U",
                        "hermes_test",
                        "-d",
                        "hermes_test",
                        "-v",
                        "ON_ERROR_STOP=1",
                        "-f",
                        "-",
                    ],
                    data=statement.encode(),
                )

            # Synthetic registry after real migrations; fixture-only bootstrap.
            sql(
                "INSERT INTO manager.installations(id,issuer,schema_version,"
                "template_schema_version,epoch,mode,cluster_identity,"
                "fresh_inventory_digest) VALUES("
                f"'{installation}','{issuer}',1,1,1,'active',"
                f"'k3d-chart-fixture','{'a' * 64}');"
            )
            sql(
                "INSERT INTO manager_admin_auth.administrator_grants(id,"
                "installation_id,issuer,subject,active,can_manage_administrators) "
                f"VALUES('{uuid.uuid4()}','{installation}',"
                f"'{issuer}','fixture-admin',true,true);"
            )
            kubeconfig = directory / "kubeconfig"
            kubeconfig.write_bytes(command(["k3d", "kubeconfig", "get", name]))
            command(["k3d", "image", "import", image, "-c", name], timeout=180)
            values = {
                "sandbox": {"enabled": False},
                "images": {"hermes-manager": image},
                "hermesManager": {
                    "enabled": True,
                    "installation": scope,
                    "api": {
                        "existingSecret": "hermes-manager-private",
                        "config": config,
                    },
                },
            }
            values_path = directory / "values.yaml"
            values_path.write_text(yaml.safe_dump(values))
            kubectl = [
                "docker",
                "exec",
                "-i",
                node,
                "/bin/kubectl",
                "--kubeconfig",
                "/etc/rancher/k3s/k3s.yaml",
            ]
            command([*kubectl, "create", "namespace", "dashboard-fixture"])
            secret = {
                "apiVersion": "v1",
                "kind": "Secret",
                "metadata": {
                    "name": "hermes-manager-private",
                    "namespace": "dashboard-fixture",
                },
                "data": {
                    name: base64.b64encode(value).decode()
                    for name, value in files.items()
                },
            }
            command([*kubectl, "apply", "-f", "-"], data=json.dumps(secret).encode())
            command(
                [
                    "helm",
                    "upgrade",
                    "--install",
                    "dashboard",
                    str(chart),
                    "--kubeconfig",
                    str(kubeconfig),
                    "-n",
                    "dashboard-fixture",
                    "-f",
                    str(values_path),
                    "--wait",
                    "--timeout",
                    "120s",
                ],
                timeout=150,
            )
            forward_log = (directory / "forward.log").open("wb")
            forward = subprocess.Popen(
                [
                    *kubectl,
                    "-n",
                    "dashboard-fixture",
                    "port-forward",
                    "--address=0.0.0.0",
                    "service/hermes-manager-api",
                    "18443:8443",
                ],
                stdout=forward_log,
                stderr=forward_log,
            )
            client = urllib.request.build_opener(
                urllib.request.ProxyHandler({}),
                urllib.request.HTTPSHandler(
                    context=ssl.create_default_context(cafile=str(ca))
                ),
            )

            def token(subject):
                now = int(time.time())
                claims = {
                    "iss": issuer,
                    "sub": subject,
                    "aud": "manager-api",
                    "azp": "manager-admin",
                    "typ": "Bearer",
                    "sid": "fixture-session",
                    "iat": now,
                    "exp": now + 300,
                }
                signing = (
                    encoded(
                        json.dumps(
                            {"alg": "RS256", "kid": "fixture", "typ": "JWT"}
                        ).encode()
                    )
                    + "."
                    + encoded(json.dumps(claims).encode())
                )
                signature = command(
                    ["openssl", "dgst", "-sha256", "-sign", str(key)],
                    data=signing.encode(),
                )
                return signing + "." + encoded(signature)

            admin_token = token("fixture-admin")

            def request(path, bearer=None):
                headers = {"Authorization": "Bearer " + bearer} if bearer else {}
                try:
                    with client.open(
                        urllib.request.Request(origin + path, headers=headers),
                        timeout=10,
                    ) as response:
                        return response.status, response.read()
                except urllib.error.HTTPError as error:
                    return error.code, error.read()

            last = "no response"
            for _ in range(30):
                try:
                    status, body = request("/api/v1/session", admin_token)
                    last = f"HTTP {status}"
                    if status == 200:
                        break
                except (OSError, urllib.error.URLError) as error:
                    last = str(error)
                time.sleep(0.5)
            else:
                forward_log.flush()
                (ROOT / ".local/chart-forward-error.log").write_bytes(
                    (directory / "forward.log").read_bytes()[-8192:]
                )
                raise AssertionError(
                    "Packaged manager chart did not serve its authenticated API: "
                    + last
                )
            assert request("/api/v1/session", token("ungranted-user"))[0] == 403
            status, data = request("/api/v1/directory-users?q=fixture", admin_token)
            assert status == 200 and json.loads(data)["items"][0]["identity"] == {
                "issuer": issuer,
                "subject": "directory-owner",
            }
            assert (
                b"synthetic-directory-secret" not in data
                and b"synthetic-directory-service-token" not in data
            )
            # A directory candidate is not signed-login evidence for grant management.
            candidates = json.loads(
                request("/api/v1/administrator-identities", admin_token)[1]
            )["items"]
            assert {item["subject"] for item in candidates} == {
                "fixture-admin",
                "ungranted-user",
            }
            assert all(item["subject"] != "directory-owner" for item in candidates)
            command(
                [
                    *kubectl,
                    "-n",
                    "dashboard-fixture",
                    "rollout",
                    "restart",
                    "deployment/hermes-manager-api",
                ]
            )
            command(
                [
                    *kubectl,
                    "-n",
                    "dashboard-fixture",
                    "rollout",
                    "status",
                    "deployment/hermes-manager-api",
                    "--timeout=120s",
                ],
                timeout=135,
            )
            print(
                "PASS: packaged Helm chart in local k3d; non-root API, "
                "PostgreSQL grants, real TLS service-token directory search, "
                "candidate isolation and Pod restart"
            )
    finally:
        if forward:
            forward.terminate()
            forward.wait(timeout=10)
        if provider:
            subprocess.run(
                ["docker", "rm", "-f", provider], capture_output=True, timeout=30
            )
        subprocess.run(
            ["docker", "rm", "-f", postgres], capture_output=True, timeout=30
        )
        subprocess.run(
            ["k3d", "cluster", "delete", name], capture_output=True, timeout=90
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--chart", type=Path, required=True)
    parser.add_argument("--image", default="hermes-manager:local")
    args = parser.parse_args()
    run_fixture(args.chart.resolve(), args.image)


if __name__ == "__main__":
    main()
