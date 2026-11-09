"""Opt-in real Hindsight, pgvector and repository-built Hermes qualification.

Uses synthetic credentials, a mock LLM and an isolated disposable Docker network.
A plain pytest run excludes this module via the image marker.
"""

import json
import os
import re
import shlex
import shutil
import subprocess
import time
import urllib.error
import urllib.request
from contextlib import contextmanager
from uuid import uuid4

import pytest
import yaml

from agent_platform.build import ROOT
from agent_platform.privacy import stage_contexts
from tests.integration.test_hindsight_chart import HINDSIGHT_IMAGE
from tests.support.images import require_image_prerequisite
from tests.test_chart import render

pytestmark = [pytest.mark.e2e, pytest.mark.image]
POSTGRES_IMAGE = (
    "pgvector/pgvector:pg17@sha256:"
    "7d0a41464797aeb9862dfdc0bbf1526b82cfd324cdd8036df19e3545f4a9ccb8"
)
HERMES_IMAGE = "agent-platform-hermes:hindsight-image-test"
SYNTHETIC_PASSWORD = "hindsight-image-owner-fixture"
SYNTHETIC_LLM_KEY = "hindsight-image-llm-fixture"


def run(*args, timeout=30, check=True, **kwargs):
    __tracebackhide__ = True
    result = subprocess.run(
        args, capture_output=True, text=True, timeout=timeout, **kwargs
    )
    if check and result.returncode:
        # Environment credentials never enter argv or failure diagnostics.
        output = result.stdout + result.stderr
        for secret in (
            SYNTHETIC_PASSWORD,
            SYNTHETIC_LLM_KEY,
            os.environ.get("HINDSIGHT_TEST_LLM_API_KEY", ""),
        ):
            if secret:
                output = output.replace(secret, "[redacted]")
        pytest.fail(output[-6000:])
    return result


def eventually(check, *, timeout=90):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            value = check()
            if value:
                return value
        except OSError, ValueError, urllib.error.URLError:
            pass
        time.sleep(0.25)
    pytest.fail(f"Condition did not converge within {timeout} seconds")


def request(base, route, body=None, *, timeout=10):
    req = urllib.request.Request(
        base + route,
        data=json.dumps(body).encode() if body is not None else None,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(
        req, timeout=timeout
    ) as response:
        return json.load(response)


def endpoint(name):
    data = json.loads(run("docker", "inspect", name).stdout)[0]
    bindings = data["NetworkSettings"]["Ports"].get("8888/tcp")
    if bindings:
        return "http://127.0.0.1:" + bindings[0]["HostPort"]
    # An internal Docker bridge does not publish ports. Its address is reachable
    # from the Linux Docker host while containers have no route to the internet.
    address = next(iter(data["NetworkSettings"]["Networks"].values()))["IPAddress"]
    return "http://" + address + ":8888"


@pytest.fixture(scope="module")
def hermes_image(tmp_path_factory):
    for executable in ("docker", "openssl", "helm"):
        require_image_prerequisite(
            bool(shutil.which(executable)),
            f"{executable} is required for Hindsight image tests",
        )
    require_image_prerequisite(
        run("docker", "info", check=False).returncode == 0,
        "a running Docker daemon is required",
    )
    stage = tmp_path_factory.mktemp("hindsight-build")
    contexts = stage_contexts(ROOT, stage / "inputs")
    run(
        "docker",
        "build",
        "-f",
        "runtimes/hermes/Dockerfile",
        "-t",
        HERMES_IMAGE,
        str(contexts["hermes"]),
        timeout=1200,
    )
    return HERMES_IMAGE


@pytest.fixture(scope="module")
def rendered_chart(tmp_path_factory):
    path = tmp_path_factory.mktemp("hindsight-render")
    base = yaml.safe_load((ROOT / "examples/lightweight/values.yaml").read_text())
    overlay = yaml.safe_load((ROOT / "examples/hindsight/values.yaml").read_text())
    base["workloads"].update(overlay["workloads"])
    base["hindsight"] = overlay["hindsight"]
    return render(path, base)


class Stack:
    def __init__(self, network, database, certs, image, chart):
        self.network, self.database, self.certs = network, database, certs
        self.image, self.chart = image, chart
        self.defaults = json.loads(
            chart["ConfigMap", "hermes-defaults"]["data"]["defaults.json"]
        )
        self.api = ""

    def sql(self, sql, database="hindsight"):
        __tracebackhide__ = True
        return run(
            "docker",
            "exec",
            "-i",
            self.database,
            "psql",
            "-U",
            "postgres",
            "-d",
            database,
            "-v",
            "ON_ERROR_STOP=1",
            "-At",
            input=sql,
        ).stdout

    def container(self, workload):
        return self.chart["Deployment", workload]["spec"]["template"]["spec"][
            "containers"
        ][0]

    def chart_env(self, workload, *, prefix=""):
        # Resolve only the synthetic Secrets supplied by this fixture. Unexpected
        # reference types or Secrets fail rather than silently dropping settings.
        secrets = {
            ("hindsight-database", "url"): (
                f"postgresql://hindsight:{SYNTHETIC_PASSWORD}@postgres:5432/hindsight"
                "?sslmode=verify-full&sslrootcert=/etc/postgres/ca.crt"
            ),
            ("hindsight-llm", "api-key"): SYNTHETIC_LLM_KEY,
        }
        env = {}
        for entry in self.container(workload)["env"]:
            if not entry["name"].startswith(prefix):
                continue
            if "value" in entry:
                value = entry["value"]
            else:
                ref = entry["valueFrom"]["secretKeyRef"]
                value = secrets[ref["name"], ref["key"]]
            env[entry["name"]] = value
        return env

    def api_env(self):
        return {
            **self.chart_env("hindsight"),
            "HINDSIGHT_API_LLM_PROVIDER": "mock",
            "HINDSIGHT_API_LLM_MODEL": "mock-model",
        }

    @contextmanager
    def api_container(self, overrides=None, *, ready=True, alias=None, egress=False):
        __tracebackhide__ = True
        name = "hindsight-image-api-" + uuid4().hex[:10]
        env = {**self.api_env(), **(overrides or {})}
        args = [
            "docker",
            "run",
            "-d",
            "--name",
            name,
            "--network",
            self.network,
            "-p",
            "127.0.0.1::8888",
            "--user",
            "1000:1000",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--mount",
            f"type=bind,src={self.certs / 'ca.crt'},dst=/etc/postgres/ca.crt,readonly",
        ]
        if alias:
            args += ["--network-alias", alias]
        for key in env:
            args += ["--env", key]
        container = self.container("hindsight")
        command = container["command"]
        args += ["--entrypoint", command[0], container["image"], *command[1:]]
        run(*args, env={**os.environ, **env})
        try:
            if egress:
                # Explicit real-model inputs opt this API into external access;
                # the default mock suite stays on its internal Docker network.
                run("docker", "network", "connect", "bridge", name)
            if ready:

                def healthy():
                    state = json.loads(run("docker", "inspect", name).stdout)[0][
                        "State"
                    ]
                    if not state["Running"]:
                        pytest.fail(
                            f"Hindsight exited {state['ExitCode']} "
                            "before /health became healthy"
                        )
                    return request(endpoint(name), "/health")["status"] == "healthy"

                eventually(
                    healthy,
                    timeout=310,
                )
            yield name
        finally:
            run("docker", "rm", "-fv", name, check=False)

    def probe(self, action, **data):
        env = {
            **self.chart_env("hermes", prefix="HINDSIGHT_"),
            "HERMES_HOME": "/opt/data",
            "HOME": "/opt/data",
            "HERMES_DISABLE_LAZY_INSTALL": "1",
        }
        args = [
            "docker",
            "run",
            "--rm",
            "--network",
            self.network,
            "--user",
            "1000:1000",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--tmpfs",
            "/opt/data:rw,mode=0700,uid=1000,gid=1000",
            "--mount",
            f"type=bind,src={ROOT / 'tests/support/hindsight_probe.py'},"
            "dst=/tmp/probe.py,readonly",
            "--entrypoint",
            "/opt/hermes/.venv/bin/python",
        ]
        args.insert(2, "-i")
        for key in env:
            args += ["--env", key]
        args += [self.image, "/tmp/probe.py", action]
        result = run(
            *args,
            input=json.dumps({"defaults": self.defaults, **data}),
            env={**os.environ, **env},
            timeout=160,
        )
        line = next(
            line
            for line in result.stdout.splitlines()
            if line.startswith("HINDSIGHT_RESULT=")
        )
        return json.loads(line.split("=", 1)[1])

    def restart(self):
        run("docker", "kill", self.api)
        run("docker", "start", self.api)
        eventually(
            lambda: request(endpoint(self.api), "/health")["status"] == "healthy",
            timeout=310,
        )


@pytest.fixture(scope="module")
def stack(tmp_path_factory, hermes_image, rendered_chart):
    suffix = uuid4().hex[:10]
    network, database = "hindsight-image-" + suffix, "hindsight-image-db-" + suffix
    certs = tmp_path_factory.mktemp("hindsight-tls")
    run(
        "openssl",
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        str(certs / "ca.key"),
        "-out",
        str(certs / "ca.crt"),
        "-subj",
        "/CN=hindsight-image-ca",
        "-days",
        "2",
    )
    run(
        "openssl",
        "req",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        str(certs / "server.key"),
        "-out",
        str(certs / "server.csr"),
        "-subj",
        "/CN=postgres",
    )
    (certs / "extensions").write_text("subjectAltName=DNS:postgres\n")
    run(
        "openssl",
        "x509",
        "-req",
        "-in",
        str(certs / "server.csr"),
        "-CA",
        str(certs / "ca.crt"),
        "-CAkey",
        str(certs / "ca.key"),
        "-CAcreateserial",
        "-out",
        str(certs / "server.crt"),
        "-days",
        "2",
        "-extfile",
        str(certs / "extensions"),
    )
    (certs / "ca.crt").chmod(0o644)
    run("docker", "pull", POSTGRES_IMAGE, timeout=600)
    run("docker", "pull", HINDSIGHT_IMAGE, timeout=1200)
    run("docker", "network", "create", "--internal", network)
    try:
        run(
            "docker",
            "run",
            "-d",
            "--name",
            database,
            "--network",
            network,
            "--network-alias",
            "postgres",
            "--env",
            "POSTGRES_PASSWORD",
            POSTGRES_IMAGE,
            env={**os.environ, "POSTGRES_PASSWORD": "hindsight-image-admin-fixture"},
        )
        eventually(
            lambda: (
                run(
                    "docker",
                    "exec",
                    database,
                    "pg_isready",
                    "-h",
                    "127.0.0.1",
                    "-U",
                    "postgres",
                    check=False,
                ).returncode
                == 0
            )
        )
        current = Stack(network, database, certs, hermes_image, rendered_chart)
        current.sql(
            f"CREATE ROLE hindsight LOGIN PASSWORD '{SYNTHETIC_PASSWORD}' "
            "NOSUPERUSER NOCREATEDB NOCREATEROLE;\n"
            "CREATE DATABASE hindsight OWNER hindsight;",
            database="postgres",
        )
        current.sql(
            "CREATE EXTENSION vector; CREATE EXTENSION pg_trgm; "
            "CREATE EXTENSION btree_gin;"
        )
        for name in ("server.key", "server.crt"):
            run(
                "docker",
                "cp",
                str(certs / name),
                database + ":/var/lib/postgresql/" + name,
            )
        run(
            "docker",
            "exec",
            "--user",
            "root",
            database,
            "sh",
            "-ceu",
            "chown postgres:postgres /var/lib/postgresql/server.*; "
            "chmod 600 /var/lib/postgresql/server.key",
        )
        current.sql(
            "ALTER SYSTEM SET ssl = 'on';\n"
            "ALTER SYSTEM SET ssl_cert_file = '/var/lib/postgresql/server.crt';\n"
            "ALTER SYSTEM SET ssl_key_file = '/var/lib/postgresql/server.key';\n"
            "SELECT pg_reload_conf();"
        )
        with current.api_container(alias="hindsight") as current.api:
            yield current
    finally:
        run("docker", "rm", "-fv", database, check=False)
        run("docker", "network", "rm", network, check=False)


def test_h11_rendered_api_becomes_healthy_without_egress(stack):
    # The stack starts the rendered API with only fixture Secrets and mock LLM
    # substitutions. Inspect its actual networks before requiring real readiness.
    networks = json.loads(run("docker", "inspect", stack.api).stdout)[0][
        "NetworkSettings"
    ]["Networks"]
    assert set(networks) == {stack.network}
    network = json.loads(run("docker", "network", "inspect", stack.network).stdout)[0]
    assert network["Internal"] is True
    assert request(endpoint(stack.api), "/health")["status"] == "healthy"


@pytest.mark.parametrize("url", ["", "https://synthetic.invalid/database", "pg0"])
def test_h2_empty_database_url_stops_the_container(stack, url):
    with stack.api_container({"HINDSIGHT_API_DATABASE_URL": url}, ready=False) as name:
        result = run("docker", "wait", name)
        assert result.stdout.strip() == "1"
        logs = run("docker", "logs", name)
        assert "HINDSIGHT_API_DATABASE_URL is not a PostgreSQL URL" in logs.stderr
        assert not url or url not in logs.stdout + logs.stderr


def test_h3_seeded_home_initializes_the_real_provider(stack):
    assert stack.probe("seeded") == {
        "mode": "local_external",
        "url": "http://hindsight:8888",
        "bank": "default",
    }
    assert "t" in stack.sql(
        "SELECT DISTINCT ssl FROM pg_stat_ssl s "
        "JOIN pg_stat_activity a ON s.pid=a.pid WHERE a.usename='hindsight';"
    )
    assert (
        stack.sql("SELECT rolsuper FROM pg_roles WHERE rolname='hindsight';").strip()
        == "f"
    )
    assert request(endpoint(stack.api), "/version")["api_version"] == "0.10.3"
    assert request(endpoint(stack.api), "/health/live")["status"] == "alive"
    assert (
        stack.sql(
            "SELECT count(*) FROM pg_tables WHERE schemaname='public' "
            "AND tableowner <> 'hindsight';"
        ).strip()
        == "0"
    )
    assert request(endpoint(stack.api), "/v1/default/banks/default/memories/list")[
        "items"
    ]


def test_h4_routed_profile_uses_its_native_settings_without_a_key(stack):
    result = stack.probe(
        "routed",
        bank="routed-" + uuid4().hex[:12],
        content="Synthetic routed bank retains the violet telescope fixture.",
    )
    assert result["credential"] is False


def test_h6_retain_creates_bank_and_survives_api_restart(stack):
    bank = "retain-" + uuid4().hex[:12]
    content = "The synthetic observatory uses the violet telescope on Tuesdays."
    banks = request(endpoint(stack.api), "/v1/default/banks")["banks"]
    assert not any(item["bank_id"] == bank for item in banks)
    assert stack.probe("retain", bank=bank, content=content)["count"] > 0
    stack.restart()
    assert stack.probe("recall", bank=bank, content=content)["count"] > 0


@pytest.mark.parametrize("status,delay", [(404, 0), (401, 0), (503, 0), (503, 120)])
def test_h6_unavailable_api_does_not_break_a_turn(stack, status, delay):
    result = stack.probe("unavailable", status=status, delay=delay)
    assert result["elapsed"] < 10
    assert result["calls"] > 0


def test_h7_derived_home_makes_no_provider_calls(stack):
    assert stack.probe("derived") == {"calls": 0, "builtin": True}


def test_h8_synthetic_credentials_are_absent_from_api_logs(stack):
    # Exercise log-producing work within this test rather than relying on order.
    stack.probe(
        "retain",
        bank="logs-" + uuid4().hex[:12],
        content="Synthetic log fixture uses a telescope.",
    )
    logs = run("docker", "logs", stack.api)
    for secret in (SYNTHETIC_PASSWORD, SYNTHETIC_LLM_KEY):
        assert secret not in logs.stdout + logs.stderr, (
            "Credential appeared in API logs"
        )


def test_h10_failed_migration_exits_without_serving(stack):
    stack.sql("CREATE DATABASE missing_vector OWNER hindsight;", database="postgres")
    url = stack.api_env()["HINDSIGHT_API_DATABASE_URL"].replace(
        "/hindsight?", "/missing_vector?"
    )
    with stack.api_container({"HINDSIGHT_API_DATABASE_URL": url}, ready=False) as name:
        assert run("docker", "wait", name, timeout=310).stdout.strip() != "0"
        with pytest.raises((OSError, urllib.error.URLError)):
            request(endpoint(name), "/health", timeout=2)
        logs = run("docker", "logs", name)
        assert "pgvector" in logs.stdout + logs.stderr


def test_h10_queued_retain_completes_after_restart(stack):
    bank = "queue-" + uuid4().hex[:12]
    route = f"/v1/default/banks/{bank}"
    result = request(
        endpoint(stack.api),
        route + "/memories",
        {
            "async": True,
            "items": [
                {
                    "content": f"Synthetic observatory {i} uses violet telescope {i}.",
                    "document_id": f"queue-{i}",
                }
                for i in range(24)
            ],
        },
    )
    operation = result["operation_id"]
    before = request(endpoint(stack.api), route + "/operations/" + operation)
    assert before["status"] in {"pending", "processing"}
    stack.restart()
    eventually(
        lambda: (
            request(endpoint(stack.api), route + "/operations/" + operation)["status"]
            == "completed"
        ),
        timeout=180,
    )


def test_h10_database_outage_fails_readiness_and_recovers(stack):
    run("docker", "stop", "--time", "5", stack.database)
    try:
        assert request(endpoint(stack.api), "/health/live")["status"] == "alive"
        with pytest.raises(urllib.error.HTTPError) as error:
            request(endpoint(stack.api), "/health", timeout=40)
        assert error.value.code == 503
    finally:
        run("docker", "start", stack.database)
        eventually(
            lambda: request(endpoint(stack.api), "/health")["status"] == "healthy"
        )


@pytest.mark.parametrize("settings", [None, {}, {"bank_id": "default"}])
def test_h13_missing_json_never_selects_cloud(stack, settings):
    result = stack.probe("missing-json", settings=settings)
    assert result["url"] == "http://hindsight:8888"


def test_h13_routed_profile_without_settings_makes_no_calls(stack):
    assert stack.probe("routed-missing") == {"calls": 0, "builtin": True}


def test_h14_dashboard_settings_reach_the_real_provider(stack):
    assert (
        stack.probe(
            "dashboard",
            bank="dashboard-" + uuid4().hex[:12],
            content="Synthetic dashboard fixture uses a violet telescope.",
        )["disabled"]
        is True
    )


def documented_command(name):
    readme = (ROOT / "charts/agent-platform/README.md").read_text()
    block = re.search(
        r"<!-- hindsight-" + name + r" -->\s*```sh\n(.*?)\n```", readme, re.S
    )
    assert block is not None
    args = shlex.split(block[1].replace("\\\n", ""))
    return args[args.index("--") + 1 :]


def test_h14_documented_commands_match_the_guide():
    activate, deactivate = (
        documented_command("activate"),
        documented_command("deactivate"),
    )
    assert (
        activate[:4]
        == deactivate[:4]
        == ["/command/s6-setuidgid", "hermes", "sh", "-ceu"]
    )
    assert "HERMES_HOME=/opt/data" in activate[4]
    assert '"bank_id":"default"' in activate[4].replace('\\"', '"')
    assert 'config set memory.provider ""' in deactivate[4]


def test_h14_documented_activation_changes_only_memory_settings(stack):
    result = stack.probe(
        "documented",
        activate=documented_command("activate")[4],
        deactivate=documented_command("deactivate")[4],
    )
    assert result["disabled"] is True


def test_optional_real_model_retain_and_recall(stack):
    names = (
        "HINDSIGHT_TEST_LLM_PROVIDER",
        "HINDSIGHT_TEST_LLM_MODEL",
        "HINDSIGHT_TEST_LLM_API_KEY",
    )
    if not all(os.environ.get(name) for name in names):
        pytest.skip(
            "optional real-model LLM inputs are absent; mock is compatibility only"
        )
    overrides = {
        "HINDSIGHT_API_LLM_" + key: os.environ["HINDSIGHT_TEST_LLM_" + key]
        for key in ("PROVIDER", "MODEL", "API_KEY")
    }
    if os.environ.get("HINDSIGHT_TEST_LLM_BASE_URL"):
        overrides["HINDSIGHT_API_LLM_BASE_URL"] = os.environ[
            "HINDSIGHT_TEST_LLM_BASE_URL"
        ]
    # Stop the mock worker before running another provider against its database.
    run("docker", "stop", "--time", "10", stack.api)
    try:
        with stack.api_container(overrides, alias="hindsight", egress=True):
            result = stack.probe(
                "real-retain",
                bank="optional-real-model",
                content="The synthetic observatory uses a violet telescope.",
            )
            assert result["count"] > 0
    finally:
        run("docker", "start", stack.api)
        eventually(
            lambda: request(endpoint(stack.api), "/health")["status"] == "healthy"
        )
