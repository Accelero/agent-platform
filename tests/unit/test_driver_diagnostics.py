"""Failures of the cluster driver name what failed, never the output that could
hold a credential, and stay bounded where they do show an error."""

from types import SimpleNamespace
from unittest.mock import patch

import pytest

from tests.personal_hermes import kubernetes as driver
from tests.personal_hermes import native_e2e as e2e

pytestmark = pytest.mark.unit

CANARY = "generated-canary-secret"


def failed(stdout=b"", stderr=b""):
    return SimpleNamespace(returncode=1, stdout=stdout, stderr=stderr)


def test_kubectl_failures_show_a_bounded_error_for_non_secret_commands():
    with patch.object(e2e.subprocess, "run", return_value=failed(stderr=b"e" * 5000)):
        with pytest.raises(AssertionError) as error:
            e2e.kubectl("cluster", "get", "pods")
    assert "kubectl get failed" in str(error.value)
    assert len(str(error.value)) < 900


def test_quiet_kubectl_failures_carry_no_output_at_all():
    with patch.object(
        e2e.subprocess, "run", return_value=failed(stderr=CANARY.encode())
    ):
        with pytest.raises(AssertionError) as error:
            e2e.kubectl("cluster", "get", "secret", quiet=True)
    assert str(error.value) == "kubectl get failed"


def test_applying_a_secret_never_echoes_the_server_error():
    document = {"kind": "Secret", "metadata": {"name": "x"}, "data": {"k": "dg=="}}
    with patch.object(
        e2e.subprocess, "run", return_value=failed(stderr=CANARY.encode())
    ):
        with pytest.raises(AssertionError) as error:
            e2e.apply("cluster", [{"kind": "ConfigMap"}, document])
    assert CANARY not in str(error.value)


def test_a_failed_fixture_command_names_no_output():
    with patch.object(
        driver.subprocess, "run", return_value=failed(stdout=CANARY, stderr=CANARY)
    ):
        with pytest.raises(AssertionError) as error:
            driver.command(["docker", "inspect", "anything"])
    assert CANARY not in str(error.value)
    assert str(error.value) == "Disposable Kubernetes fixture command failed"
