"""Acceptance scenarios of the installed platform, each an independent case.

The assertions are those of the standalone end-to-end program's scenario
helpers, kept as they are; what changes is that each scenario is collected,
reported and runnable on its own against the same installed platform, in any
order (set `HERMES_TEST_ORDER=reverse` to run them back to front).
"""

import pytest

from tests.personal_hermes import native_e2e as scenarios

pytestmark = [pytest.mark.e2e, pytest.mark.cluster, pytest.mark.slow]


def test_two_owners_reach_their_own_native_instances_through_the_gateway(platform):
    scenarios.two_owners_reach_their_own_native_instances_through_the_gateway(
        platform.identifier,
        platform.admin,
        platform.ca,
        platform.passwords,
        platform.assignments,
    )


@pytest.mark.security
def test_foreign_cookies_tickets_and_forged_headers_reach_nothing(platform):
    scenarios.foreign_cookies_tickets_and_forged_headers_reach_nothing(
        platform.ca, platform.passwords
    )


@pytest.mark.security
def test_chat_settings_and_the_admin_secret_reach_only_the_referencing_instance(
    platform,
):
    scenarios.chat_settings_and_the_admin_secret_reach_only_the_referencing_instance(
        platform.identifier,
        platform.admin,
        platform.ca,
        platform.passwords,
        platform.assignments,
    )


@pytest.mark.security
def test_uploads_reach_only_the_owners_private_home(platform):
    scenarios.uploads_reach_only_the_owners_private_home(
        platform.identifier,
        platform.admin,
        platform.ca,
        platform.passwords,
        platform.assignments,
    )


@pytest.mark.security
def test_an_email_change_keeps_ownership_and_the_old_email_gets_nothing(platform):
    scenarios.an_email_change_keeps_ownership_and_the_old_email_gets_nothing(
        platform.ca, platform.passwords, platform.assignments
    )


@pytest.mark.security
def test_owners_share_a_static_folder(platform):
    scenarios.owners_share_a_static_folder(
        platform.identifier,
        platform.admin,
        platform.assignments,
        platform.state.template,
        platform.state.terminal,
    )


def test_a_missing_static_directory_blocks_without_a_substitute(platform):
    scenarios.a_missing_static_directory_blocks_without_a_substitute(
        platform.identifier,
        platform.admin,
        platform.state.template,
        platform.state.terminal,
    )


def test_a_hermes_without_browser_traffic_acquires_a_terminal_backend(platform):
    scenarios.a_hermes_without_browser_traffic_acquires_a_terminal_backend(
        platform.identifier, platform.admin, platform.assignments
    )


@pytest.mark.security
def test_only_the_controller_holds_kubernetes_credentials_and_logs_hold_no_secrets(
    platform,
):
    scenarios.only_the_controller_holds_kubernetes_credentials_and_logs_hold_no_secrets(
        platform.identifier,
        platform.admin,
        platform.ca,
        platform.passwords,
        platform.assignments,
    )


def test_published_versions_commission_ready_instances_from_the_api(platform):
    namespaces, instances = set(), set()
    for name, receipt in platform.assignments.items():
        phase = platform.admin.phase(receipt["instanceId"])
        assert phase["phase"] == "ready", (name, phase["phase"])
        namespaces.add(phase["namespace"])
        instances.add(receipt["instanceId"])
    assert len(platform.assignments) == len(namespaces) == len(instances) == 2


def test_a_cron_job_fires_without_browser_traffic_and_acquires_a_terminal(platform):
    scenarios.a_cron_job_fires_without_browser_traffic_and_acquires_a_terminal(
        platform.identifier,
        platform.admin,
        platform.ca,
        platform.passwords,
        platform.assignments,
    )


def placement(platform, instance_id):
    """Where the instance's Pods and volumes are, and what the scheduler said:
    bounded, from the cluster, and free of credentials."""
    namespace = platform.admin.phase(instance_id)["namespace"]
    cluster = platform.cluster
    shown = [
        cluster.kubectl("get", "pods", "-n", namespace, "-o", "wide", check=False),
        cluster.kubectl(
            "get",
            "pvc,pv",
            "-A",
            "-o",
            "custom-columns=NAME:.metadata.name,NODE:.metadata.annotations.volume\\.kubernetes\\.io/selected-node,STATUS:.status.phase",
            check=False,
        ),
        cluster.kubectl(
            "get",
            "events",
            "-n",
            namespace,
            "--sort-by=.lastTimestamp",
            check=False,
        ),
    ]
    return "\n".join(result.stdout.decode(errors="replace")[-2500:] for result in shown)


def test_bobs_backends_share_the_workspace_and_idle_workers_retire(platform):
    try:
        scenarios.bobs_backends_share_the_workspace_and_idle_workers_retire(
            platform.identifier, platform.admin, platform.assignments
        )
    except AssertionError as error:
        raise AssertionError(
            f"{str(error)[:600]}\n--- placement ---\n"
            + placement(platform, platform.assignments["bob"]["instanceId"])
        ) from None
