"""Destructive harness steps need proof of ownership and always clean up."""

import json

import pytest

from tests.support.ownership import (
    FIXTURE,
    KUBERNETES,
    POSTGRES,
    RUN,
    Cleanup,
    CommandFailed,
    ForeignResource,
    database_comment,
    remove_fixture,
    require_fixture,
    run_databases,
)

CANARY = "canary-label-value"


class FakeDocker:
    """Containers by name with their labels; records every command."""

    def __init__(self, containers, databases=()):
        self.containers = dict(containers)
        self.databases = list(databases)
        self.calls = []

    def __call__(self, args, timeout=30):
        self.calls.append(list(args))
        name = args[-1]
        if args[0] == "inspect":
            if name not in self.containers:
                raise CommandFailed("docker inspect failed")
            return json.dumps(self.containers[name]) + "\n"
        if args[0] == "rm":
            self.containers.pop(name)
            return ""
        if args[0] == "exec":
            return "\n".join(self.databases)
        raise AssertionError(args)

    def removed(self):
        return [call[-1] for call in self.calls if call[0] == "rm"]


def fixture(kind, run="a" * 12):
    return {FIXTURE: kind, RUN: run, "unrelated": CANARY}


@pytest.mark.unit
@pytest.mark.parametrize(
    ("labels", "message"),
    [
        (None, "not a personal-hermes-kubernetes fixture"),
        ({}, "not a personal-hermes-kubernetes fixture"),
        (
            {FIXTURE: POSTGRES, "unrelated": CANARY},
            "not a personal-hermes-kubernetes fixture",
        ),
        (
            {FIXTURE: KUBERNETES, RUN: "b" * 12, "unrelated": CANARY},
            "belongs to another run",
        ),
        ({RUN: "a" * 12, "unrelated": CANARY}, "not a personal-hermes-kubernetes"),
    ],
    ids=[
        "no-labels",
        "empty-labels",
        "other-kind",
        "other-run",
        "run-without-fixture-label",
    ],
)
def test_only_a_labelled_fixture_of_the_right_kind_and_run_is_accepted(labels, message):
    docker = FakeDocker({"candidate": labels})
    with pytest.raises(ForeignResource, match=message) as refused:
        require_fixture("candidate", KUBERNETES, "a" * 12, run_docker=docker)
    assert CANARY not in str(refused.value)
    assert docker.removed() == []


@pytest.mark.unit
def test_a_labelled_fixture_reports_its_run():
    docker = FakeDocker({"mine": fixture(KUBERNETES)})
    assert require_fixture("mine", KUBERNETES, run_docker=docker) == "a" * 12
    assert require_fixture("mine", KUBERNETES, "a" * 12, run_docker=docker)


@pytest.mark.unit
def test_a_failed_inspection_is_an_error_not_an_answer():
    docker = FakeDocker({})
    with pytest.raises(CommandFailed):
        require_fixture("missing", KUBERNETES, run_docker=docker)
    with pytest.raises(CommandFailed):
        remove_fixture("missing", KUBERNETES, run_docker=docker)
    assert docker.removed() == []


@pytest.mark.unit
def test_removal_touches_only_the_verified_container_never_a_similar_name():
    docker = FakeDocker(
        {
            "personal-hermes-kube-test-owned": fixture(KUBERNETES),
            "personal-hermes-kube-test-foreign": {},
            "personal-hermes-kube-test-other-run": fixture(KUBERNETES, "b" * 12),
            "personal-hermes-manager-tests": fixture(POSTGRES),
        }
    )
    for name in (
        "personal-hermes-kube-test-foreign",
        "personal-hermes-kube-test-other-run",
        "personal-hermes-manager-tests",
    ):
        with pytest.raises(ForeignResource):
            remove_fixture(name, KUBERNETES, "a" * 12, run_docker=docker)
    assert docker.removed() == []
    remove_fixture(
        "personal-hermes-kube-test-owned", KUBERNETES, "a" * 12, run_docker=docker
    )
    assert docker.removed() == ["personal-hermes-kube-test-owned"]
    assert len(docker.containers) == 3


@pytest.mark.unit
def test_databases_belong_to_a_run_only_by_the_comment_it_stored():
    docker = FakeDocker(
        {"postgres": fixture(POSTGRES)}, databases=["hermes_e2e_0123abcd"]
    )
    assert run_databases("postgres", "a" * 12, run_docker=docker) == [
        "hermes_e2e_0123abcd"
    ]
    query = docker.calls[-1][-1]
    assert database_comment("a" * 12) in query
    assert "LIKE" not in query.upper()
    assert "hermes_e2e_" not in query


@pytest.mark.unit
def test_databases_of_a_foreign_server_are_never_listed_or_dropped():
    for labels in (None, fixture(KUBERNETES)):
        docker = FakeDocker({"postgres": labels}, databases=["hermes_e2e_0123abcd"])
        with pytest.raises(ForeignResource):
            run_databases("postgres", "a" * 12, run_docker=docker)
        assert [call[0] for call in docker.calls] == ["inspect"]


@pytest.mark.unit
@pytest.mark.parametrize(
    "name",
    [
        "x; DROP DATABASE hermes_test",
        "hermes_e2e_",
        "hermes_test",
        "HERMES_E2E_0123ABCD",
    ],
    ids=["injection", "empty-suffix", "other-database", "uppercase"],
)
def test_an_unexpected_database_name_is_refused_not_passed_on(name):
    docker = FakeDocker({"postgres": fixture(POSTGRES)}, databases=[name])
    with pytest.raises(AssertionError, match="unexpected database name"):
        run_databases("postgres", "a" * 12, run_docker=docker)


@pytest.mark.unit
def test_a_malformed_run_identifier_cannot_reach_a_query():
    for run in ("", "A" * 12, "a" * 11, "a' OR '1'='1"):
        with pytest.raises(AssertionError, match="malformed run identifier"):
            database_comment(run)


@pytest.mark.unit
def test_every_finalizer_runs_in_reverse_order_when_one_fails():
    order = []

    def fail():
        order.append("second")
        raise RuntimeError("second failed")

    with pytest.raises(ExceptionGroup) as group, Cleanup() as cleanup:
        cleanup.add("first", lambda: order.append("first"))
        cleanup.add("second", fail)
        cleanup.add("third", lambda: order.append("third"))
    assert order == ["third", "second", "first"]
    assert [str(error) for error in group.value.exceptions] == ["second failed"]


@pytest.mark.unit
def test_the_original_error_survives_and_cleanup_failures_are_notes():
    ran = []

    def fail():
        raise OSError(f"secret detail {CANARY}")

    with pytest.raises(ValueError, match="the test failed") as raised:
        with Cleanup() as cleanup:
            cleanup.add("earlier allocation", lambda: ran.append("earlier"))
            cleanup.add("broken allocation", fail)
            raise ValueError("the test failed")
    assert ran == ["earlier"]
    assert raised.value.__notes__ == ["cleanup failed: broken allocation: OSError"]
    assert CANARY not in "".join(raised.value.__notes__)


@pytest.mark.unit
def test_an_interrupt_still_releases_everything_allocated_so_far():
    released = []
    with pytest.raises(KeyboardInterrupt), Cleanup() as cleanup:
        cleanup.add("a", lambda: released.append("a"))
        cleanup.add("b", lambda: released.append("b"))
        raise KeyboardInterrupt
    assert released == ["b", "a"]


@pytest.mark.unit
def test_a_failed_second_allocation_still_cleans_the_first():
    released = []

    def allocate(name, fail=False):
        if fail:
            raise RuntimeError("allocation failed")
        released.append(f"allocated {name}")

    with pytest.raises(RuntimeError, match="allocation failed"):
        with Cleanup() as cleanup:
            allocate("first")
            cleanup.add("first", lambda: released.append("released first"))
            allocate("second", fail=True)
            cleanup.add("second", lambda: released.append("released second"))
    assert released == ["allocated first", "released first"]
