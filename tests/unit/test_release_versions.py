"""The release version rule: placeholders on ordinary commits, the tag's exact
version on a release commit, and nothing publishable before it is verified."""

import re
import subprocess

import pytest
import yaml

from agent_platform import release

STABLE = ["0.6.9", "0.7.0", "1.0.0", "10.20.30", "0.7.0-dev.0", "0.7.0-dev.12"]
MALFORMED = [
    "",
    "0.1",
    "1.2.3.4",
    "01.2.3",
    "1.02.3",
    "1.2.03",
    "1.2.3-dev",
    "1.2.3-dev.",
    "1.2.3-dev.01",
    "1.2.3-dev.1.2",
    "1.2.3-rc.1",
    "1.2.3-beta",
    "1.2.3+build.5",
    "v1.2.3",
    "1.2.3 ",
    " 1.2.3",
    "1.2.3\n",
    "latest",
    "../1.2.3",
]


def tree(tmp_path, *, chart="0.0.0", app="0.0.0", project="0.0.0"):
    (tmp_path / "charts/agent-platform").mkdir(parents=True)
    (tmp_path / "charts/agent-platform/Chart.yaml").write_text(
        yaml.safe_dump({"name": "agent-platform", "version": chart, "appVersion": app})
    )
    (tmp_path / "pyproject.toml").write_text(
        f'[project]\nname = "agent-platform"\nversion = "{project}"\n'
    )
    return tmp_path


def trigger():
    workflow = yaml.safe_load(
        (release.ROOT / ".github/workflows/release.yml").read_text()
    )
    (pattern,) = workflow["on"]["push"]["tags"]
    # GitHub filter patterns: `*` is any run of characters except `/`.
    expression = (
        re.escape(pattern).replace(r"\[0\-9\]", "[0-9]").replace(r"\*", "[^/]*")
    )
    return re.compile(expression)


@pytest.mark.unit
@pytest.mark.parametrize("version", STABLE)
def test_stable_and_development_versions_are_releasable_and_start_the_workflow(
    version, tmp_path
):
    assert release.VERSION.fullmatch(version)
    assert trigger().fullmatch(f"v{version}")
    release.verify_release_source(
        version, tree(tmp_path, chart=version, project=version)
    )


@pytest.mark.unit
@pytest.mark.parametrize("version", [item for item in MALFORMED if item])
def test_a_malformed_version_is_never_releasable(version, tmp_path):
    assert not release.VERSION.fullmatch(version)
    root = tree(tmp_path, chart=version, project=version)
    with pytest.raises(ValueError, match="Release tag must be"):
        release.verify_release_source(version, root)


@pytest.mark.unit
@pytest.mark.parametrize(
    "tag", ["v1.2.3-rc.1", "v01.2.3", "v1.2.3.4", "v1.2.3+5", "v1.2.3-dev"]
)
def test_a_tag_the_trigger_accepts_but_the_version_rule_refuses_stops_before_publishing(
    tag, tmp_path
):
    """The workflow glob is deliberately loose; validation is the real gate."""
    assert trigger().fullmatch(tag)
    version = tag.removeprefix("v")
    with pytest.raises(ValueError):
        release.verify_release_source(
            version, tree(tmp_path, chart=version, project=version)
        )


@pytest.mark.unit
@pytest.mark.parametrize("tag", ["1.2.3", "v1.2", "release-1.2.3", "v1", "latest"])
def test_tags_outside_the_release_pattern_do_not_start_the_workflow(tag):
    assert not trigger().fullmatch(tag)


@pytest.mark.unit
@pytest.mark.parametrize(
    ("chart", "project", "stale"),
    [
        ("0.0.0", "1.2.3", "chart"),
        ("1.2.3", "0.0.0", "project"),
        ("1.2.2", "1.2.3", "chart"),
        ("1.2.3", "1.2.4", "project"),
        ("1.2.3-dev.1", "1.2.3", "chart"),
    ],
    ids=[
        "chart-placeholder",
        "project-placeholder",
        "chart-older",
        "project-newer",
        "chart-prerelease",
    ],
)
def test_a_release_commit_must_carry_the_tags_version_in_chart_and_project(
    chart, project, stale, tmp_path
):
    root = tree(tmp_path, chart=chart, project=project)
    with pytest.raises(ValueError, match=f"The {stale} version"):
        release.verify_release_source("1.2.3", root)


@pytest.mark.unit
def test_the_placeholder_is_not_a_release(tmp_path):
    root = tree(tmp_path)
    with pytest.raises(ValueError, match="Release tag must be"):
        release.verify_release_source("0.0.0", root)
    with pytest.raises(ValueError, match="semantic version"):
        release.build("0.0.0", "a" * 40, publish=True)


@pytest.mark.unit
@pytest.mark.parametrize(
    ("field", "name"),
    [("chart", "chart"), ("app", "chart appVersion"), ("project", "project")],
)
def test_ordinary_commits_keep_the_placeholder_everywhere(field, name, tmp_path):
    release.verify_placeholder_source(tree(tmp_path))
    other = tmp_path / "other"
    other.mkdir()
    with pytest.raises(ValueError, match=f"The {name} version must stay 0.0.0"):
        release.verify_placeholder_source(tree(other, **{field: "0.6.10"}))


@pytest.mark.unit
def test_this_source_tree_is_a_placeholder_tree_or_the_exact_release_commit():
    tags = subprocess.run(
        ["git", "tag", "--points-at", "HEAD"],
        cwd=release.ROOT,
        capture_output=True,
        text=True,
        timeout=10,
        check=True,
    ).stdout.split()
    releases = [tag for tag in tags if re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+.*", tag)]
    assert len(releases) <= 1, "one release per commit"
    if releases:
        release.verify_release_source(releases[0].removeprefix("v"))
    else:
        release.verify_placeholder_source()


@pytest.mark.unit
def test_nothing_is_authenticated_or_published_before_the_version_is_verified():
    steps = yaml.safe_load(
        (release.ROOT / ".github/workflows/release.yml").read_text()
    )["jobs"]["publish"]["steps"]
    verified = next(
        index
        for index, step in enumerate(steps)
        if "verify_release_source" in step.get("run", "")
    )
    risky = [
        index
        for index, step in enumerate(steps)
        if "login" in step.get("uses", "")
        or "registry login" in step.get("run", "")
        or "--publish" in step.get("run", "")
        or "gh release create" in step.get("run", "")
    ]
    assert risky, "the workflow must contain publishing steps"
    assert verified < min(risky)
