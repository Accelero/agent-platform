"""The ownership checks against a real Docker daemon, with containers that are
only created, never started, from an image imported out of an empty archive."""

import io
import subprocess
import tarfile

import pytest

from tests.support.ownership import (
    FIXTURE,
    KUBERNETES,
    POSTGRES,
    RUN,
    Cleanup,
    CommandFailed,
    ForeignResource,
    docker,
    labels,
    new_run,
    remove_fixture,
    require_fixture,
)


def exists(name):
    return bool(
        docker(["ps", "--all", "--quiet", "--filter", f"name=^{name}$"]).strip()
    )


def import_empty_image(tag):
    archive = io.BytesIO()
    tarfile.open(fileobj=archive, mode="w").close()
    result = subprocess.run(
        ["docker", "import", "-", tag],
        input=archive.getvalue(),
        capture_output=True,
        timeout=30,
    )
    assert result.returncode == 0, "importing the empty image failed"


def create(name, image, container_labels):
    arguments = ["create", "--name", name]
    for key, value in container_labels.items():
        arguments += ["--label", f"{key}={value}"]
    docker([*arguments, image, "/none"])


@pytest.fixture
def containers():
    """Containers of every ownership shape, removed by exact name."""
    run = new_run()
    prefix = f"ownership-test-{run}"
    image = f"agent-platform-ownership-test:{run}"
    shapes = {
        "owned": {FIXTURE: KUBERNETES, RUN: run},
        "foreign": {},
        "other_run": {FIXTURE: KUBERNETES, RUN: new_run()},
        "postgres": {FIXTURE: POSTGRES, RUN: run},
    }
    names = {key: f"{prefix}-{key.replace('_', '-')}" for key in shapes}
    with Cleanup() as cleanup:
        import_empty_image(image)
        cleanup.add("image", lambda: docker(["rmi", "--force", image]))
        for key, name in names.items():
            cleanup.add(
                name, lambda name=name: docker(["rm", "--force", "--volumes", name])
            )
            create(name, image, shapes[key])
        yield run, names


@pytest.mark.integration
def test_labels_are_read_from_the_daemon_and_unlabelled_containers_are_foreign(
    containers,
):
    run, names = containers
    assert labels(names["owned"])[RUN] == run
    assert labels(names["foreign"]) == {}
    with pytest.raises(ForeignResource):
        require_fixture(names["foreign"], KUBERNETES)
    assert require_fixture(names["owned"], KUBERNETES, run) == run


@pytest.mark.integration
def test_removal_leaves_similar_unlabelled_other_run_and_other_kind_containers(
    containers,
):
    run, names = containers
    for key in ("foreign", "other_run", "postgres"):
        with pytest.raises(ForeignResource):
            remove_fixture(names[key], KUBERNETES, run)
        assert exists(names[key]), key
    remove_fixture(names["owned"], KUBERNETES, run)
    assert not exists(names["owned"])
    for key in ("foreign", "other_run", "postgres"):
        assert exists(names[key]), key


@pytest.mark.integration
def test_a_container_the_daemon_does_not_know_is_an_error_not_permission(containers):
    run, names = containers
    missing = f"{names['owned']}-missing"
    with pytest.raises(CommandFailed):
        require_fixture(missing, KUBERNETES, run)
    with pytest.raises(CommandFailed):
        remove_fixture(missing, KUBERNETES, run)
    assert exists(names["owned"])
