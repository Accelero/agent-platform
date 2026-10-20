"""Native Kubernetes acceptance inside the disposable fixture cluster.

Called by kubernetes.py --native after building the reviewed native runtime.
Only isolated Docker/Kubernetes fixtures and synthetic owner state are used.
"""

import json
import shutil
import subprocess
import time
import uuid
from pathlib import Path

from tests.personal_hermes.kubernetes import ROOT, command


def run_fixture(identifier, postgres, temporary):
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
            "activeDeadlineSeconds": 600,
            "containers": [
                {
                    "name": "tests",
                    "image": fixture_image,
                    "imagePullPolicy": "Never",
                    "args": ["--ignored", "--nocapture"],
                    "env": [
                        {"name": key, "value": value}
                        for key, value in {
                            "DATABASE_URL": f"postgres://hermes_test@{address}/hermes_test",
                            "HERMES_NATIVE_COMMISSIONING_FIXTURE": "isolated",
                            "FIXTURE_CONTROL_NAMESPACE": control,
                            "HERMES_NATIVE_FIXTURE_IMAGE": image,
                            "HERMES_NATIVE_FIXTURE_UPDATED_IMAGE": image_updated,
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
    deadline = time.monotonic() + 600

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
            assert checked == {"ready", "updated", "restarted", "deleted", "readded"}
            print(
                "PASS: two real native instances; image/admin updates, Pod restart, "
                "removal/re-add and persistent owner state"
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
            stage in {"ready", "updated", "restarted", "deleted", "readded"}
            and stage not in checked
        ):
            owner_a, owner_b = progress["namespaceA"], progress["namespaceB"]
            if stage == "ready":
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
