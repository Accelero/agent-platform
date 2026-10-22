"""Render manager API/controller boundaries using only synthetic configuration."""

import json
import subprocess
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[1]


def render(tmp_path, values):
    path = tmp_path / "values.yaml"
    path.write_text(yaml.safe_dump(values))
    result = subprocess.run(
        [
            "helm",
            "template",
            "fixture",
            str(ROOT / "charts/agent-platform"),
            "-n",
            "fixture",
            "-f",
            str(path),
        ],
        capture_output=True,
        text=True,
        check=True,
        timeout=30,
    )
    return [value for value in yaml.safe_load_all(result.stdout) if value]


def test_manager_api_chart_has_native_config_private_mounts_and_no_kubernetes_rights(
    tmp_path,
):
    values = yaml.safe_load((ROOT / "examples/manager/values.yaml").read_text())
    documents = render(tmp_path, values)
    deployment = next(x for x in documents if x["kind"] == "Deployment")
    pod = deployment["spec"]["template"]["spec"]
    assert pod["automountServiceAccountToken"] is False
    assert pod["securityContext"]["runAsUser"] == 10001
    container = pod["containers"][0]
    assert container["securityContext"]["readOnlyRootFilesystem"] is True
    assert container["securityContext"]["capabilities"]["drop"] == ["ALL"]
    assert container["command"] == ["hermes-manager"]
    assert container["args"][0] == "api"
    assert {x["containerPort"] for x in container["ports"]} == {8443, 8444, 8445}
    config = json.loads(
        next(
            x
            for x in documents
            if x["kind"] == "ConfigMap"
            and x["metadata"]["name"].startswith("hermes-manager-")
        )["data"]["config.json"]
    )
    assert config["installation"] == values["hermesManager"]["installation"]
    assert config["admin"]["uiDirectory"] == "/usr/share/hermes-manager/ui"
    assert config["admin"]["directory"]["clientSecretFile"].startswith(
        "/run/manager/secrets/"
    )
    assert (
        next(x for x in documents if x["kind"] == "NetworkPolicy")["spec"]["ingress"]
        == []
    )
    assert not any(
        x["kind"] in {"Role", "ClusterRole", "Secret"}
        and x["metadata"]["name"].startswith("hermes-manager-")
        for x in documents
    )


def test_manager_api_without_browser_sessions_stays_api_only(tmp_path):
    values = yaml.safe_load((ROOT / "examples/manager/values.yaml").read_text())
    del values["hermesManager"]["api"]["config"]["admin"]["browser"]
    documents = render(tmp_path, values)
    config = json.loads(
        next(
            x
            for x in documents
            if x["kind"] == "ConfigMap"
            and x["metadata"]["name"].startswith("hermes-manager-")
        )["data"]["config.json"]
    )
    # The process rejects dashboard assets without browser authentication.
    assert "uiDirectory" not in config["admin"]


def test_controller_requires_explicit_account_and_separate_secret(tmp_path):
    values = yaml.safe_load((ROOT / "examples/manager/values.yaml").read_text())
    manager = values["hermesManager"]
    manager["api"]["enabled"] = False
    manager["controller"] = {
        "enabled": True,
        "existingSecret": "controller-private",
        "serviceAccountName": "reviewed-controller",
        "config": {},
    }
    documents = render(tmp_path, values)
    pod = next(x for x in documents if x["kind"] == "Deployment")["spec"]["template"][
        "spec"
    ]
    assert pod["automountServiceAccountToken"] is True
    assert pod["serviceAccountName"] == "reviewed-controller"
    assert (
        next(x for x in pod["volumes"] if x["name"] == "secrets")["secret"][
            "secretName"
        ]
        == "controller-private"
    )
    assert not any(
        x["kind"] == "Service" and x["metadata"]["name"].startswith("hermes-manager-")
        for x in documents
    )
    config = json.loads(
        next(
            x
            for x in documents
            if x["kind"] == "ConfigMap"
            and x["metadata"]["name"].startswith("hermes-manager-")
        )["data"]["config.json"]
    )
    assert config["kubernetes"] == {"source": "inCluster"}


def test_router_auth_listeners_have_separate_ingress_and_no_kubernetes_token(tmp_path):
    values = yaml.safe_load((ROOT / "examples/manager/values.yaml").read_text())
    manager = values["hermesManager"]
    manager["api"]["enabled"] = False
    manager["routerAuth"] = {
        "enabled": True,
        "existingSecret": "router-private",
        "config": {"public": {}, "internal": {}},
    }
    manager["ingress"] = {
        "routerPublic": [{"podSelector": {"matchLabels": {"role": "public-proxy"}}}]
    }
    documents = render(tmp_path, values)
    pod = next(x for x in documents if x["kind"] == "Deployment")["spec"]["template"][
        "spec"
    ]
    assert pod["automountServiceAccountToken"] is False
    container = pod["containers"][0]
    assert container["command"] == ["hermes-router-auth"]
    assert container["args"] == ["--config", "/run/manager/config/config.json"]
    assert {x["containerPort"] for x in container["ports"]} == {8446, 8447}
    policy = next(
        x
        for x in documents
        if x["kind"] == "NetworkPolicy"
        and x["metadata"]["name"] == "hermes-manager-router-auth"
    )
    assert [
        port["port"] for rule in policy["spec"]["ingress"] for port in rule["ports"]
    ] == [8446]
