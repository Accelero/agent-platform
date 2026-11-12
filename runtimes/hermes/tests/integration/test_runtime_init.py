"""Native initialization from real chart renders, and task scripts run as processes."""

import json
from pathlib import Path

import yaml

from agent_platform.build import ROOT
from tests.support.chart import render


def test_helm_profiles_seed_independent_images_without_overwriting_live_settings(
    tmp_path: Path, initializer
):
    values = {
        "workloads": {"hermes": {"enabled": True}},
        "hermes": {
            "profiles": {
                "research": {
                    "plugins": {
                        "entries": {
                            "managed_execution": {
                                "settings": {"image": "example.org/research:1"}
                            }
                        }
                    }
                },
                "coding": {
                    "plugins": {
                        "entries": {
                            "managed_execution": {
                                "settings": {"image": "example.org/coding:1"}
                            }
                        }
                    }
                },
            }
        },
    }
    objects = render(tmp_path, values)
    defaults = json.loads(
        objects["ConfigMap", "hermes-defaults"]["data"]["defaults.json"]
    )
    home = tmp_path / "home"
    initializer.seed_defaults(home, defaults)
    root = yaml.safe_load((home / "config.yaml").read_text())
    assert root["plugins"]["entries"]["managed_execution"]["settings"] == {
        "profile": "default"
    }
    for name in ("research", "coding"):
        path = home / "profiles" / name / "config.yaml"
        config = yaml.safe_load(path.read_text())
        assert config["terminal"]["backend"] == "managed_execution"
        settings = config["plugins"]["entries"]["managed_execution"]["settings"]
        assert settings == {"profile": name, "image": f"example.org/{name}:1"}
    path.write_text("model: user-choice\nterminal: {backend: managed_execution}\n")
    initializer.seed_defaults(home, defaults)
    assert path.read_text() == (
        "model: user-choice\nterminal: {backend: managed_execution}\n"
    )
    pod = objects["Deployment", "hermes"]["spec"]["template"]["spec"]
    mount = next(
        item
        for item in pod["containers"][0]["volumeMounts"]
        if item["name"] == "hermes-defaults"
    )
    assert mount["readOnly"] is True


def test_default_chart_has_no_named_persona_or_native_home_worker_mount(tmp_path: Path):
    objects = render(
        tmp_path,
        {
            "workloads": {
                "hermes": {"enabled": True},
                "runtime-manager": {"enabled": True},
            }
        },
    )
    defaults = json.loads(
        objects["ConfigMap", "hermes-defaults"]["data"]["defaults.json"]
    )
    assert defaults["profiles"] == {}
    manager = yaml.safe_load(
        objects["ConfigMap", "runtime-manager-config"]["data"]["config.yaml"]
    )
    template = manager["templates"]["hermes"]
    mounts = template["kubernetes"]["volume_mounts"]
    assert {item["mountPath"] for item in mounts} == {
        "/workspace",
        "/etc/ssh/authorized_keys/worker",
    }
    assert "identity_kubernetes_claim" not in template["workdir_scope"]


def test_h7_multica_run_removes_hindsight_environment(tmp_path):
    import os
    import subprocess

    home = tmp_path / "home"
    (home / ".multica").mkdir(parents=True)
    (home / ".multica/config.json").write_text("{}")
    bin_path = tmp_path / "bin"
    bin_path.mkdir()
    (bin_path / "s6-setuidgid").write_text('#!/bin/sh\nshift\nexec "$@"\n')
    (bin_path / "multica").write_text("#!/bin/sh\nenv\n")
    for path in bin_path.iterdir():
        path.chmod(0o755)
    script = (
        (ROOT / "runtimes/hermes/multica-run")
        .read_text()
        .replace("/opt/data", str(home))
    )
    result = subprocess.run(
        ["/bin/sh", "-c", script],
        text=True,
        capture_output=True,
        timeout=10,
        check=True,
        env={
            "PATH": str(bin_path) + os.pathsep + "/usr/bin:/bin",
            "HINDSIGHT_MODE": "local_external",
            "HINDSIGHT_BANK_ID": "default",
            "HINDSIGHT_API_URL": "http://hindsight:8888",
        },
    )
    assert not any(line.startswith("HINDSIGHT_") for line in result.stdout.splitlines())
    assert "HOME=" + str(home) in result.stdout
