"""Admin secret delivery, rotation, removal and refusal on the shared platform
(R10, R11, R18).

Each case writes its own secrets and creates its own people, templates and
assignments, so it neither depends on nor disturbs the commissioned instances.
The secret values are canaries generated for the case. They are compared only
by SHA-256 and are never printed: a failing assertion names the place that held
or lacked a value, not the value.

A value an owner's own agent can read is delivered on purpose (SPEC.md,
"Configuration and admin secrets"); what these cases forbid is any other
recipient, any API, log or object that should not hold it, and a stale value
after rotation or removal.
"""

import base64
import hashlib
import json
import secrets

import pytest

from tests.personal_hermes import native_e2e as scenarios
from tests.support.people import person, ready, template

pytestmark = [
    pytest.mark.e2e,
    pytest.mark.cluster,
    pytest.mark.slow,
    pytest.mark.security,
]

DIRECTORY = "/run/hermes/admin-secrets"
CONTROL_PLANE = ("hermes-manager-api", "hermes-manager-controller", "hermes-gateway")


def digest(value):
    return hashlib.sha256(value.encode()).hexdigest()


def canary(prefix):
    return f"{prefix}-{secrets.token_hex(16)}"


def holders(platform, value):
    """Where in the cluster a Kubernetes Secret holds `value`, as
    (namespace, secret, key); the values themselves are only hashed."""
    items = json.loads(
        scenarios.kubectl(
            platform.identifier,
            "get",
            "secrets",
            "--all-namespaces",
            "-o",
            "json",
            quiet=True,
            timeout=180,
        )
    )["items"]
    wanted = digest(value)
    return sorted(
        (item["metadata"]["namespace"], item["metadata"]["name"], key)
        for item in items
        for key, stored in (item.get("data") or {}).items()
        if hashlib.sha256(base64.b64decode(stored)).hexdigest() == wanted
    )


def listing(platform):
    """The secret metadata by name; values are never part of it."""
    items = platform.admin.call("GET", "/api/v1/secrets?limit=100").json()["items"]
    return {item["name"]: item for item in items}


def exec_digest(platform, namespace, script):
    """SHA-256 of what a command prints inside the instance's Hermes."""
    return hashlib.sha256(
        scenarios.instance_pod_exec(platform.identifier, namespace, script).encode()
    ).hexdigest()


def exit_status(platform, namespace, script):
    return scenarios.instance_pod_exec(
        platform.identifier, namespace, f"{script} >/dev/null 2>&1; echo $?"
    ).strip()


def restarted(platform, receipt, namespace, before):
    def replaced():
        try:
            return scenarios.hermes_pod(platform.identifier, namespace) != before
        except AssertionError:
            return False

    scenarios.wait(replaced, 600, "the instance never restarted", interval=5)
    return ready(platform, receipt)


def surfaces(platform, namespaces):
    """Everything the platform says about itself in text: API bodies and the
    logs of the control plane and of the named instances."""
    admin = platform.admin
    bodies = [
        admin.call("GET", "/api/v1/secrets?limit=100").text,
        admin.call("GET", "/api/v1/audit-events?limit=100").text,
        admin.call("GET", "/api/v1/assignments?limit=100").text,
        admin.call("GET", "/api/v1/templates?limit=100").text,
        admin.call("GET", "/api/v1/terminal-backends?limit=100").text,
    ]
    sources = [(scenarios.NAMESPACE, f"deployment/{name}") for name in CONTROL_PLANE]
    sources += [(namespace, "deployment/hermes") for namespace in namespaces]
    for namespace, target in sources:
        bodies.append(
            scenarios.kubectl(
                platform.identifier,
                "logs",
                "--namespace",
                namespace,
                target,
                "--all-containers",
                "--tail=-1",
                quiet=True,
            )
        )
    return "\n".join(bodies)


def test_a_file_secret_reaches_only_its_referencing_instance_and_follows_its_reference(
    platform,
):
    admin, tag = platform.admin, secrets.token_hex(3)
    name, filename = f"file-{tag}", "token"
    first, second = canary("file-one"), canary("file-two")

    admin.call("PUT", f"/api/v1/secrets/{name}", {"kind": "file", "value": first})
    using = admin.publish(
        "templates",
        f"with-file-{tag}",
        template(platform, secrets=[{"name": name, "file": filename}]),
    )
    holder = person(
        platform, "filed", {"template": {"id": f"with-file-{tag}", "version": using}}
    )
    bystander = person(
        platform,
        "plain",
        {"template": {"id": "personal", "version": platform.state.template}},
    )
    holder_space, other_space = ready(platform, holder), ready(platform, bystander)

    # Delivered: a read-only file in the referencing instance, nowhere else.
    path = f"{DIRECTORY}/{filename}"
    assert exec_digest(platform, holder_space, f"cat {path}") == digest(first)
    assert exit_status(platform, holder_space, f"echo x >> {path}") != "0"
    assert exit_status(platform, holder_space, f"touch {DIRECTORY}/other") != "0"
    mode = scenarios.instance_pod_exec(
        platform.identifier, holder_space, f"stat -L -c %a {path}"
    )
    assert mode.strip() == "440", mode
    environment = scenarios.instance_pod_exec(platform.identifier, holder_space, "env")
    assert first not in environment, "a file secret was also delivered as a variable"
    assert exit_status(platform, other_space, f"test -e {DIRECTORY}") != "0", (
        "an instance that does not reference the secret has the directory"
    )
    assert holders(platform, first) == [(holder_space, "hermes-admin-secrets", name)]

    # Rotation restarts the referencing instance only and replaces the value.
    holder_pod = scenarios.hermes_pod(platform.identifier, holder_space)
    other_pod = scenarios.hermes_pod(platform.identifier, other_space)
    admin.call("PUT", f"/api/v1/secrets/{name}", {"kind": "file", "value": second})
    restarted(platform, holder, holder_space, holder_pod)
    assert exec_digest(platform, holder_space, f"cat {path}") == digest(second)
    assert scenarios.hermes_pod(platform.identifier, other_space) == other_pod, (
        "an instance that does not reference the secret was restarted"
    )
    assert holders(platform, first) == [], "the replaced value is still stored"
    assert holders(platform, second) == [(holder_space, "hermes-admin-secrets", name)]
    assert listing(platform)[name]["version"] == 2
    assert listing(platform)[name]["references"] == 1

    # Nothing the platform shows holds either value.
    shown = surfaces(platform, [holder_space, other_space])
    assert name in shown, "the scan of the platform's output read nothing"
    assert first not in shown and second not in shown, "a value was shown or logged"

    # Dropping the reference restarts the instance without the file; only then
    # can the secret be deleted, and nothing keeps its value.
    admin.call(
        "DELETE", f"/api/v1/secrets/{name}", etag='"2"', expected=(409,)
    )  # still referenced by the template version
    plain = admin.publish("templates", f"without-file-{tag}", template(platform))
    current = admin.call("GET", f"/api/v1/assignments/{holder['assignmentId']}")
    spec = current.json()["spec"]
    spec["template"] = {"id": f"without-file-{tag}", "version": plain}
    holder_pod = scenarios.hermes_pod(platform.identifier, holder_space)
    admin.call(
        "PATCH",
        f"/api/v1/assignments/{holder['assignmentId']}",
        spec,
        etag=current.headers["ETag"],
    )
    restarted(platform, holder, holder_space, holder_pod)
    assert exit_status(platform, holder_space, f"test -e {path}") != "0", (
        "the file outlived its reference"
    )
    assert holders(platform, second) == [], "the value outlived its reference"


def test_references_of_the_wrong_kind_or_to_nothing_are_refused_and_store_nothing(
    platform,
):
    admin, tag = platform.admin, secrets.token_hex(3)
    docker = json.dumps({"auths": {"registry.invalid": {"auth": canary("auth")}}})
    kinds = {"env": canary("env"), "file": canary("file"), "registry": docker}
    for kind, value in kinds.items():
        admin.call(
            "PUT", f"/api/v1/secrets/{kind}-{tag}", {"kind": kind, "value": value}
        )

    # A secret keeps the kind it was created with, and a value must fit it.
    for name, kind, value in (
        (f"file-{tag}", "env", canary("wrong-kind")),
        (f"registry-{tag}", "file", docker),
        (f"bad-{tag}", "registry", canary("not-a-docker-config")),
    ):
        refused = admin.call(
            "PUT",
            f"/api/v1/secrets/{name}",
            {"kind": kind, "value": value},
            expected=(422,),
        )
        assert refused.json() == {"error": "invalid_secret_value"}
    stored = listing(platform)
    assert f"bad-{tag}" not in stored, "a refused secret was stored"
    assert [stored[f"{kind}-{tag}"]["version"] for kind in kinds] == [1, 1, 1]
    assert [stored[f"{kind}-{tag}"]["kind"] for kind in kinds] == list(kinds)

    def problems(collection, definition):
        reply = admin.call(
            "POST", f"/api/v1/{collection}/validate", {"definition": definition}
        ).json()
        return [(item["pointer"], item["code"]) for item in reply["problems"]]

    base = template(platform)
    wrong = "wrong_secret_kind"
    for reference, secret in (
        ({"env": "X_KEY"}, f"file-{tag}"),
        ({"env": "X_KEY"}, f"registry-{tag}"),
        ({"file": "x"}, f"env-{tag}"),
        ({"file": "x"}, f"registry-{tag}"),
    ):
        found = problems(
            "templates", {**base, "secrets": [{"name": secret, **reference}]}
        )
        assert found == [("/secrets/0/name", wrong)], (reference, secret, found)
    assert (
        problems(
            "templates", {**base, "secrets": [{"name": f"env-{tag}", "env": "X_KEY"}]}
        )
        == []
    )
    assert problems(
        "templates", {**base, "secrets": [{"name": f"nope-{tag}", "env": "X_KEY"}]}
    ) == [("/secrets/0/name", "unknown_secret")]

    backend = {
        **platform.state.worker,
        "limits": {"idleSeconds": 300, "maxLeaseSeconds": 900, "maxEnvironments": 2},
        "mounts": {"workspace": {"mountPath": "/workspace"}},
    }
    assert (
        problems("terminal-backends", {**backend, "registrySecret": f"registry-{tag}"})
        == []
    )
    assert problems(
        "terminal-backends", {**backend, "registrySecret": f"env-{tag}"}
    ) == [("/registrySecret", wrong)]
    assert problems(
        "terminal-backends", {**backend, "registrySecret": f"nope-{tag}"}
    ) == [("/registrySecret", "unknown_secret")]

    # A draft naming a missing secret can be saved but never published.
    draft = admin.call(
        "POST",
        f"/api/v1/templates/dangling-{tag}/versions",
        {"definition": {**base, "secrets": [{"name": f"nope-{tag}", "env": "X_KEY"}]}},
    ).json()
    published = admin.call(
        "POST",
        f"/api/v1/templates/dangling-{tag}/versions/{draft['version']}/publish",
        etag=f'"{draft["revision"]}"',
        expected=(422,),
    )
    assert published.json()["error"] == "invalid_configuration"

    # Deletion needs the current version, and a referenced secret stays.
    admin.call("DELETE", f"/api/v1/secrets/env-{tag}", expected=(422,))
    admin.call("DELETE", f"/api/v1/secrets/env-{tag}", etag='"7"', expected=(412,))
    admin.call("DELETE", "/api/v1/secrets/llm-key", etag='"1"', expected=(409,))
    assert "llm-key" in listing(platform)
    admin.call("DELETE", f"/api/v1/secrets/env-{tag}", etag='"1"')
    assert f"env-{tag}" not in listing(platform)
    assert problems(
        "templates", {**base, "secrets": [{"name": f"env-{tag}", "env": "X_KEY"}]}
    ) == [("/secrets/0/name", "unknown_secret")]
    shown = surfaces(platform, [])
    assert f"file-{tag}" in shown, "the scan of the platform's output read nothing"
    assert all(value not in shown for value in kinds.values()), "a value was shown"


def test_a_registry_secret_reaches_only_the_instance_whose_backend_names_it(platform):
    admin, tag = platform.admin, secrets.token_hex(3)
    name = f"registry-{tag}"
    password = canary("registry-one")
    auth = base64.b64encode(f"user:{password}".encode()).decode()
    first = json.dumps({"auths": {"registry.invalid": {"auth": auth}}})
    admin.call("PUT", f"/api/v1/secrets/{name}", {"kind": "registry", "value": first})
    backend = admin.publish(
        "terminal-backends",
        f"private-{tag}",
        {
            **platform.state.worker,
            "limits": {
                "idleSeconds": 300,
                "maxLeaseSeconds": 900,
                "maxEnvironments": 2,
            },
            "mounts": {"workspace": {"mountPath": "/workspace"}},
            "registrySecret": name,
        },
    )
    holder = person(
        platform,
        "private",
        {
            "template": {"id": "personal", "version": platform.state.template},
            "terminal": {
                "default": f"private-{tag}",
                "allowed": {f"private-{tag}": backend},
            },
        },
    )
    bystander = person(
        platform,
        "public",
        {"template": {"id": "personal", "version": platform.state.template}},
    )
    holder_space, other_space = ready(platform, holder), ready(platform, bystander)

    # The pull credential is an image pull Secret (Docker config) in the
    # instance's namespace only, and Hermes itself is not given it. The object's
    # name is not part of the specification, so it is found by its content.
    [(space, pull, key)] = holders(platform, first)
    assert (space, key) == (holder_space, ".dockerconfigjson")
    kind = scenarios.kubectl(
        platform.identifier,
        "get",
        "secret",
        pull,
        "--namespace",
        holder_space,
        "-o",
        "jsonpath={.type}",
        quiet=True,
    )
    assert kind == "kubernetes.io/dockerconfigjson", kind
    for space in (holder_space, other_space):
        environment = scenarios.instance_pod_exec(platform.identifier, space, "env")
        assert password not in environment and first not in environment
    assert listing(platform)[name]["references"] == 1

    # Rotation replaces it where it was and nowhere else.
    rotated = json.dumps(
        {
            "auths": {
                "registry.invalid": {
                    "auth": base64.b64encode(
                        f"user:{canary('registry-two')}".encode()
                    ).decode()
                }
            }
        }
    )
    admin.call("PUT", f"/api/v1/secrets/{name}", {"kind": "registry", "value": rotated})
    scenarios.wait(
        lambda: (
            [(a, c) for a, _, c in holders(platform, rotated)]
            == [(holder_space, ".dockerconfigjson")]
        ),
        300,
        "the rotated pull credential never replaced the old one",
        interval=5,
    )
    assert holders(platform, first) == [], "the replaced credential is still stored"
    shown = surfaces(platform, [holder_space, other_space])
    assert name in shown, "the scan of the platform's output read nothing"
    assert password not in shown and first not in shown and rotated not in shown
