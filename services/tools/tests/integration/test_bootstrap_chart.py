"""The chart's bootstrap Jobs and RBAC, rendered by real Helm."""

import json

from tests.support.chart import render


def test_chart_ships_automatic_job_and_suspended_manual_template(tmp_path):
    values = {
        "multica": {"ownerEmail": "owner@example.org"},
        "bootstrap": {
            "enabled": True,
            "installation": "fixture",
            "lifecycle": {
                key: ["fixture", key] for key in ("before", "after", "checkpoint")
            },
        },
        "images": {"tools": "fixture-tools"},
    }
    objects = render(tmp_path, values)
    job = objects["Job", "agents-bootstrap-1"]
    assert job["spec"]["activeDeadlineSeconds"] == 1200
    cron = objects["CronJob", "agents-bootstrap"]
    assert cron["spec"]["suspend"] is True
    pod = job["spec"]["template"]["spec"]
    assert pod["containers"][0]["command"] == ["python", "-m", "platform_bootstrap"]
    assert pod["containers"][0]["args"] == []
    assert pod["securityContext"]["runAsUser"] == 1000
    assert not any(kind == "Secret" for kind, _ in objects)
    configuration = json.loads(
        objects["ConfigMap", "agents-bootstrap"]["data"]["config.json"]
    )
    assert configuration["createAgent"] is True
    assert configuration["runtimeVisibility"] == "private"
    values["bootstrap"]["createAgent"] = False
    runtime_only = render(tmp_path, values)
    configuration = json.loads(
        runtime_only["ConfigMap", "agents-bootstrap"]["data"]["config.json"]
    )
    assert configuration["createAgent"] is False


def test_default_bootstrap_account_can_read_budget_and_deletion_identities(tmp_path):
    from tests.support.chart import render

    values = {
        "bootstrap": {
            "enabled": True,
            "installation": "fixture",
            "workspace": {"name": "Fixture", "slug": "fixture", "issuePrefix": "FIX"},
            "lifecycle": {
                name: ["fixture"] for name in ("before", "after", "checkpoint")
            },
        },
        "multica": {"ownerEmail": "owner@example.org"},
        "images": {"tools": "fixture-image"},
    }
    objects = render(tmp_path, values, include_namespaces=True)
    rules = objects["Role", "agents", "agents-bootstrap"]["rules"]
    assert any(
        "batch" in rule["apiGroups"]
        and "jobs" in rule["resources"]
        and "get" in rule["verbs"]
        for rule in rules
    )
    assert any(
        "configmaps" in rule["resources"]
        and "agent-state-deleting" in rule.get("resourceNames", [])
        for rule in rules
    )
