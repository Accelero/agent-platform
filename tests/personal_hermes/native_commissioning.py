"""Native Kubernetes acceptance inside the disposable fixture cluster.

Called by kubernetes.py --native after building the reviewed native runtime.
Only isolated Docker/Kubernetes fixtures and synthetic owner state are used.
"""

import base64
import json
import shutil
import subprocess
import tempfile
import time
import uuid
from pathlib import Path

from agent_platform.privacy import stage_contexts
from tests.personal_hermes.kubernetes import ROOT, command


def run_fixture(identifier, postgres, temporary):
    with tempfile.TemporaryDirectory(prefix="terminal-worker-build-") as directory:
        worker_context = stage_contexts(ROOT, Path(directory) / "inputs")[
            "agent-runtime"
        ]
        command(
            [
                "docker",
                "build",
                "--file",
                str(worker_context / "Dockerfile"),
                "--tag",
                "personal-hermes-worker-test:local",
                str(worker_context),
            ],
            timeout=1200,
        )
    context = Path(temporary) / "native-context"
    context.mkdir()
    build = subprocess.run(
        [
            "cargo",
            "test",
            "--test",
            "native_commissioning",
            "--locked",
            "--no-run",
            "--message-format=json",
        ],
        cwd=ROOT / "services/hermes-manager",
        capture_output=True,
        text=True,
        timeout=300,
    )
    assert build.returncode == 0, "Native commissioning fixture build failed"
    artifacts = [json.loads(line) for line in build.stdout.splitlines()]
    binary = next(
        item["executable"]
        for item in artifacts
        if item.get("reason") == "compiler-artifact"
        and item.get("executable")
        and item["target"]["name"] == "native_commissioning"
    )
    shutil.copyfile(binary, context / "native-commissioning")
    (context / "native-commissioning").chmod(0o755)
    (context / "Dockerfile").write_text(
        "FROM personal-hermes-native-test:local\n"
        "COPY native-commissioning /fixture/native-commissioning\n"
        'ENTRYPOINT ["/fixture/native-commissioning"]\n'
    )
    fixture_image = "personal-hermes-commissioning-tests:local"
    command(["docker", "build", "--tag", fixture_image, str(context)], timeout=120)
    command(["docker", "run", "--rm", fixture_image, "--list"], timeout=20)
    (context / "Dockerfile").write_text(
        "FROM personal-hermes-native-test:local\nLABEL native-fixture-revision=2\n"
    )
    updated_image = "personal-hermes-native-update:local"
    command(["docker", "build", "--tag", updated_image, str(context)], timeout=120)
    (context / "Dockerfile").write_text(
        "FROM personal-hermes-worker-test:local\nLABEL terminal-runtime=alternate\n"
    )
    command(
        [
            "docker",
            "build",
            "--tag",
            "personal-hermes-worker-alternate:local",
            str(context),
        ],
        timeout=120,
    )
    archive = Path(temporary) / "native-images.tar"
    command(
        [
            "docker",
            "save",
            "--output",
            str(archive),
            fixture_image,
            updated_image,
            "personal-hermes-native-test:local",
            "personal-hermes-worker-test:local",
            "personal-hermes-worker-alternate:local",
        ],
        timeout=180,
    )
    command(
        ["docker", "cp", str(archive), f"{identifier}:/tmp/native-images.tar"],
        timeout=180,
    )
    command(
        [
            "docker",
            "exec",
            identifier,
            "ctr",
            "images",
            "import",
            "/tmp/native-images.tar",
        ],
        timeout=240,
    )
    listing = command(["docker", "exec", identifier, "ctr", "images", "list"])

    def pinned(tag):
        reference = "docker.io/library/" + tag
        digest = next(
            line.split()[2]
            for line in listing.splitlines()
            if line.split()[0] == reference
        )
        assert digest.startswith("sha256:")
        pinned_reference = reference.rsplit(":", 1)[0] + "@" + digest
        command(
            [
                "docker",
                "exec",
                identifier,
                "ctr",
                "images",
                "tag",
                reference,
                pinned_reference,
            ]
        )
        return pinned_reference

    image = pinned("personal-hermes-native-test:local")
    image_updated = pinned(updated_image)
    worker_image = pinned("personal-hermes-worker-test:local")
    worker_alternate = pinned("personal-hermes-worker-alternate:local")
    postgres_state = json.loads(command(["docker", "inspect", postgres]))[0]
    server_state = json.loads(command(["docker", "inspect", identifier]))[0]
    shared = (
        postgres_state["NetworkSettings"]["Networks"].keys()
        & server_state["NetworkSettings"]["Networks"].keys()
    )
    assert len(shared) == 1, "Native fixture needs one shared PostgreSQL network"
    address = postgres_state["NetworkSettings"]["Networks"][shared.pop()]["IPAddress"]
    assert address
    control = "native-commissioning-" + uuid.uuid4().hex[:12]
    execution_url = f"https://execution.{control}.svc:8443/execution"
    tls = Path(temporary) / "fixture-tls"
    tls.mkdir()
    command(
        [
            "openssl",
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            str(tls / "server.key"),
            "-out",
            str(tls / "server.crt"),
            "-days",
            "1",
            "-subj",
            "/CN=execution-fixture",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
            "-addext",
            f"subjectAltName=DNS:execution.{control}.svc",
        ],
        timeout=30,
    )
    kubectl = ["docker", "exec", identifier, "kubectl", "--request-timeout=10s"]

    def apply(value):
        result = subprocess.run(
            [
                "docker",
                "exec",
                "-i",
                identifier,
                "kubectl",
                "--request-timeout=10s",
                "apply",
                "-f",
                "-",
            ],
            input=json.dumps(value),
            text=True,
            capture_output=True,
            timeout=30,
        )
        assert result.returncode == 0, "Native fixture manifest failed"

    def obj(kind, name, **fields):
        return {
            "apiVersion": "v1",
            "kind": kind,
            "metadata": {"name": name, "namespace": control},
            **fields,
        }

    apply({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": control}})
    apply(obj("ServiceAccount", "fixture-tests"))
    apply(
        obj(
            "Secret",
            "fixture-tls",
            data={
                name: base64.b64encode((tls / name).read_bytes()).decode()
                for name in ("server.key", "server.crt")
            },
        )
    )
    apply(
        obj(
            "Service",
            "execution",
            spec={
                "selector": {"app.kubernetes.io/component": "hermes-controller"},
                "ports": [{"port": 8443, "targetPort": 8443}],
            },
        )
    )
    apply(
        {
            "apiVersion": "rbac.authorization.k8s.io/v1",
            "kind": "ClusterRoleBinding",
            "metadata": {"name": control},
            "roleRef": {
                "apiGroup": "rbac.authorization.k8s.io",
                "kind": "ClusterRole",
                "name": "cluster-admin",
            },
            "subjects": [
                {
                    "kind": "ServiceAccount",
                    "name": "fixture-tests",
                    "namespace": control,
                }
            ],
        }
    )
    apply(obj("ConfigMap", "fixture-progress", data={"stage": "starting"}))
    pod = obj(
        "Pod",
        "native-tests",
        spec={
            "serviceAccountName": "fixture-tests",
            "restartPolicy": "Never",
            "activeDeadlineSeconds": 900,
            "volumes": [{"name": "tls", "secret": {"secretName": "fixture-tls"}}],
            "containers": [
                {
                    "name": "tests",
                    "image": fixture_image,
                    "imagePullPolicy": "Never",
                    "volumeMounts": [
                        {"name": "tls", "mountPath": "/fixture-tls", "readOnly": True}
                    ],
                    "args": ["--ignored", "--nocapture"],
                    "env": [
                        {"name": key, "value": value}
                        for key, value in {
                            "DATABASE_URL": f"postgres://hermes_test@{address}/hermes_test",
                            "HERMES_NATIVE_COMMISSIONING_FIXTURE": "isolated",
                            "FIXTURE_CONTROL_NAMESPACE": control,
                            "HERMES_NATIVE_FIXTURE_IMAGE": image,
                            "HERMES_NATIVE_FIXTURE_UPDATED_IMAGE": image_updated,
                            "HERMES_WORKER_FIXTURE_IMAGE": worker_image,
                            "HERMES_WORKER_FIXTURE_ALTERNATE_IMAGE": worker_alternate,
                            "HERMES_FIXTURE_EXECUTION_URL": execution_url,
                        }.items()
                    ],
                    "resources": {"requests": {"cpu": "500m", "memory": "1Gi"}},
                }
            ],
        },
    )
    pod["metadata"]["labels"] = {"app.kubernetes.io/component": "hermes-controller"}
    apply(pod)
    checked = set()
    deadline = time.monotonic() + 900

    def native_pod(namespace):
        data = json.loads(
            command(
                [
                    *kubectl,
                    "get",
                    "pods",
                    "-n",
                    namespace,
                    "-l",
                    "app.kubernetes.io/component=hermes",
                    "-o",
                    "json",
                ]
            )
        )
        assert len(data["items"]) == 1
        return data["items"][0]["metadata"]["name"]

    def execute(namespace, script):
        return command(
            [
                *kubectl,
                "exec",
                "-n",
                namespace,
                native_pod(namespace),
                "--",
                "/opt/hermes/.venv/bin/python",
                "-c",
                script,
            ],
            timeout=30,
        )

    while time.monotonic() < deadline:
        status = json.loads(
            command(
                [*kubectl, "get", "pod", "native-tests", "-n", control, "-o", "json"]
            )
        )
        phase = status.get("status", {}).get("phase")
        if phase in {"Succeeded", "Failed"}:
            logs = command(
                [*kubectl, "logs", "native-tests", "-n", control], timeout=30
            )
            if phase != "Succeeded":
                print(logs[-12000:])
            assert phase == "Succeeded", "Native-on-Kubernetes commissioning failed"
            assert checked == {
                "ready",
                "updated",
                "restarted",
                "deleted",
                "readded",
                "terminal",
                "terminal-retired",
            }
            print(
                "PASS: two real native instances; image/admin updates, Pod restart, "
                "removal/re-add and persistent owner state; dashboard/profile "
                "backend selection, scoped real SSH/subpath execution and retirement"
            )
            return
        progress = json.loads(
            command(
                [
                    *kubectl,
                    "get",
                    "configmap",
                    "fixture-progress",
                    "-n",
                    control,
                    "-o",
                    "json",
                ]
            )
        )["data"]
        stage = progress["stage"]
        if (
            stage
            in {
                "ready",
                "updated",
                "restarted",
                "deleted",
                "readded",
                "terminal",
                "terminal-retired",
            }
            and stage not in checked
        ):
            owner_a, owner_b = progress["namespaceA"], progress["namespaceB"]
            print("Native acceptance stage:", stage, flush=True)
            if stage == "terminal":
                script = (Path(__file__).with_name("terminal_profile.py")).read_text()
                for namespace, owner, runtime_id, denied in [
                    (owner_a, "owner-a", "python-tools", False),
                    (owner_a, "owner-a", "shell-tools", False),
                    (owner_b, "owner-b", "python-tools", False),
                    (owner_b, "owner-b", "shell-tools", True),
                ]:
                    print(
                        "Terminal acceptance:",
                        owner,
                        runtime_id,
                        "denied" if denied else "allowed",
                        flush=True,
                    )
                    result = subprocess.run(
                        [
                            "docker",
                            "exec",
                            "-i",
                            identifier,
                            "kubectl",
                            "--request-timeout=10s",
                            "exec",
                            "-i",
                            "-n",
                            namespace,
                            native_pod(namespace),
                            "--",
                            "/opt/hermes/.venv/bin/python",
                            "-",
                            owner,
                            runtime_id,
                            str(int(denied)),
                        ],
                        input=script,
                        text=True,
                        capture_output=True,
                        timeout=210,
                    )
                    if result.returncode:
                        print(result.stderr[-4000:])
                        print(
                            command(
                                [
                                    *kubectl,
                                    "logs",
                                    "native-tests",
                                    "-n",
                                    control,
                                    "--tail=60",
                                    "--limit-bytes=12000",
                                ]
                            )
                        )
                    assert result.returncode == 0, (
                        "Native profile/terminal execution failed"
                    )
                    assert "PASS: native profile selection" in result.stdout, (
                        "Native terminal acceptance script did not execute"
                    )
            elif stage == "terminal-retired":
                for namespace in (owner_a, owner_b):
                    workers = json.loads(
                        command(
                            [
                                *kubectl,
                                "get",
                                "pods",
                                "-n",
                                namespace,
                                "-l",
                                "app.kubernetes.io/component=worker",
                                "-o",
                                "json",
                            ]
                        )
                    )
                    assert not workers["items"], "Retired terminal Pod still exists"
            elif stage == "ready":
                for namespace, owner in [(owner_a, "owner-a"), (owner_b, "owner-b")]:
                    execute(
                        namespace,
                        "import pathlib,json; p=pathlib.Path('/opt/data'); "
                        f"(p/'owner-fixture.txt').write_text('{owner}'); "
                        "s=json.loads((p/'config.yaml').read_text()); "
                        "s['agent']={'max_turns':77,"
                        "'fixture_preference':'owner choice'}; "
                        "(p/'config.yaml').write_text(json.dumps(s))",
                    )
            elif stage != "deleted":
                for namespace, owner in [(owner_a, "owner-a"), (owner_b, "owner-b")]:
                    execute(
                        namespace,
                        "import pathlib,json,os; "
                        "os.environ['HERMES_HOME']='/opt/data'; "
                        "p=pathlib.Path('/opt/data/owner-fixture.txt'); "
                        f"assert p.read_text()=='{owner}'; "
                        "from hermes_cli.config import load_config; c=load_config(); "
                        "assert c['agent']['max_turns']==24; "
                        "assert c['agent']['fixture_preference']=='owner choice'; "
                        "p=pathlib.Path('/opt/data/config.yaml'); "
                        "raw=json.loads(p.read_text()); "
                        "assert raw['agent']['max_turns']==77",
                    )
                if stage == "updated":
                    command(
                        [
                            *kubectl,
                            "delete",
                            "pod",
                            "-n",
                            owner_a,
                            native_pod(owner_a),
                            "--wait=true",
                        ],
                        timeout=60,
                    )
            else:
                data = json.loads(
                    command(
                        [*kubectl, "get", "deployments", "-n", owner_a, "-o", "json"]
                    )
                )
                assert not data["items"]
                execute(
                    owner_b,
                    "import pathlib; p=pathlib.Path('/opt/data/owner-fixture.txt'); "
                    "assert p.read_text()=='owner-b'",
                )
            command(
                [
                    *kubectl,
                    "patch",
                    "configmap",
                    "fixture-progress",
                    "-n",
                    control,
                    "--type=merge",
                    "-p",
                    json.dumps({"data": {"stage": stage + "-checked"}}),
                ]
            )
            checked.add(stage)
        time.sleep(0.5)
    raise AssertionError("Native commissioning fixture exceeded its deadline")
