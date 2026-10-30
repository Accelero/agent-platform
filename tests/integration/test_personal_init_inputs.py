"""Every malformed mounted input is refused by the real entry point with one
fixed message, echoes nothing, and writes nothing to the owner's home."""

import copy
import json

import pytest

from tests.support.initializer import (
    CONFIG,
    CONTEXT,
    KEY,
    REFUSAL,
    load,
    write_inputs,
)

pytestmark = pytest.mark.integration

CANARY = "generated-canary-secret"


def mutated(path, value):
    """CONTEXT with the value at `path` replaced; `...` deletes it."""
    result = copy.deepcopy(CONTEXT)
    target = result
    for part in path[:-1]:
        target = target[part]
    if value is ...:
        del target[path[-1]]
    else:
        target[path[-1]] = value
    return result


def with_extra(path, name):
    result = copy.deepcopy(CONTEXT)
    target = result
    for part in path:
        target = target[part]
    target[name] = CANARY
    return result


CONTEXTS = {
    "empty-file": b"",
    "not-json": b"owner: " + CANARY.encode(),
    "json-array": json.dumps([CANARY]).encode(),
    "json-null": b"null",
    "invalid-utf8": b'{"schemaVersion": 1, "x": "\xff"}',
    "oversized": json.dumps({"padding": "x" * 20000}).encode(),
    "deeply-nested-arrays": b"[" * 8000 + b"]" * 8000,
    "deeply-nested-objects": b'{"a":' * 2700 + b"1" + b"}" * 2700,
    "schema-version-two": mutated(["schemaVersion"], 2),
    "schema-version-boolean": mutated(["schemaVersion"], True),
    "schema-version-text": mutated(["schemaVersion"], "1"),
    "missing-owner": mutated(["owner"], ...),
    "unknown-top-level-field": with_extra([], "extra"),
    "unknown-owner-field": with_extra(["owner"], "email"),
    "installation-not-canonical": mutated(
        ["installationId"], "00000000-0000-4000-8000-00000000000A"
    ),
    "instance-nil": mutated(["instanceId"], "00000000-0000-0000-0000-000000000000"),
    "material-braced": mutated(
        ["materialId"], "{00000000-0000-4000-8000-000000000003}"
    ),
    "instance-number": mutated(["instanceId"], 7),
    "issuer-http": mutated(["owner", "issuer"], "http://sso.example.org/realms/a"),
    "issuer-userinfo": mutated(
        ["owner", "issuer"], "https://user:pass@sso.example.org/realms/a"
    ),
    "issuer-query": mutated(["owner", "issuer"], "https://sso.example.org/r?x=1"),
    "issuer-fragment": mutated(["owner", "issuer"], "https://sso.example.org/r#x"),
    "issuer-padded": mutated(["owner", "issuer"], " https://sso.example.org/r"),
    "issuer-without-host": mutated(["owner", "issuer"], "https:///realms/a"),
    "issuer-too-long": mutated(
        ["owner", "issuer"], "https://sso.example.org/" + "a" * 2100
    ),
    "issuer-number": mutated(["owner", "issuer"], 5),
    "subject-empty": mutated(["owner", "subject"], ""),
    "subject-too-long": mutated(["owner", "subject"], "a" * 256),
    "subject-space": mutated(["owner", "subject"], "owner a"),
    "subject-non-ascii": mutated(["owner", "subject"], "owner-é"),
    "subject-control": mutated(["owner", "subject"], "owner\n"),
    "subject-number": mutated(["owner", "subject"], 5),
    "template-uppercase": mutated(["template", "id"], "Personal"),
    "template-leading-digit": mutated(["template", "id"], "1personal"),
    "template-too-long": mutated(["template", "id"], "a" * 64),
    "template-version-zero": mutated(["template", "version"], 0),
    "template-version-boolean": mutated(["template", "version"], True),
    "template-version-text": mutated(["template", "version"], "1"),
    "template-version-float": mutated(["template", "version"], 1.0),
}
CONFIGS = {
    "empty-file": b"",
    "not-json": b"terminal: " + CANARY.encode(),
    "json-array": json.dumps([CANARY]).encode(),
    "oversized": json.dumps(
        {"terminal": {"backend": "managed_execution"}, "padding": "x" * 70000}
    ).encode(),
    "no-terminal": {"model": CANARY},
    "terminal-is-a-list": {"terminal": [CANARY]},
    "terminal-is-text": {"terminal": CANARY},
    "local-backend": {"terminal": {"backend": "local"}},
    "backend-in-another-case": {"terminal": {"backend": "Managed_Execution"}},
    "backend-missing": {"terminal": {"runtime_id": CANARY}},
}
KEYS = {
    "empty-file": b"",
    "too-short": "2a" * 31,
    "too-long": "2a" * 33,
    "not-hex": "zz" * 32,
    "uppercase-hex": "2A" * 32,
    "oversized": "2a" * 200,
    "invalid-utf8": b"\xff" * 64,
}


def run(initializer, monkeypatch, root, paths, result):
    monkeypatch.setattr(
        "sys.argv",
        [
            "personal-init",
            "--root",
            str(root),
            "--context",
            str(paths["context"]),
            "--config",
            str(paths["config"]),
            "--key",
            str(paths["key"]),
            "--result",
            str(result),
        ],
    )
    initializer.main()


@pytest.fixture
def home(tmp_path):
    root = tmp_path / "home"
    root.mkdir()
    return root


def refused(initializer, monkeypatch, tmp_path, home, **inputs):
    paths = write_inputs(tmp_path, **inputs)
    result = tmp_path / "result.json"
    with pytest.raises(SystemExit) as refusal:
        run(initializer, monkeypatch, home, paths, result)
    assert str(refusal.value) == REFUSAL
    assert not result.exists(), "no receipt for a refused initialization"
    assert list(home.iterdir()) == [], "the home was not touched"


@pytest.mark.parametrize("name", CONTEXTS)
def test_a_malformed_context_is_refused_before_the_home_is_touched(
    name, tmp_path, home, monkeypatch, capsys
):
    refused(load(), monkeypatch, tmp_path, home, context=CONTEXTS[name])
    captured = capsys.readouterr()
    assert CANARY not in captured.out + captured.err


@pytest.mark.parametrize("name", CONFIGS)
def test_a_malformed_or_unmanaged_config_is_refused_before_the_home_is_touched(
    name, tmp_path, home, monkeypatch, capsys
):
    refused(load(), monkeypatch, tmp_path, home, config=CONFIGS[name])
    captured = capsys.readouterr()
    assert CANARY not in captured.out + captured.err


@pytest.mark.parametrize("name", KEYS)
def test_a_malformed_initialization_key_is_refused_before_the_home_is_touched(
    name, tmp_path, home, monkeypatch, capsys
):
    refused(load(), monkeypatch, tmp_path, home, key=KEYS[name])
    captured = capsys.readouterr()
    assert KEY not in captured.out + captured.err


@pytest.mark.parametrize("missing", ["context", "config", "key"])
def test_an_absent_mounted_input_is_refused_before_the_home_is_touched(
    missing, tmp_path, home, monkeypatch
):
    refused(load(), monkeypatch, tmp_path, home, **{missing: None})


@pytest.mark.parametrize("name", ["context", "config", "key"])
def test_a_mounted_input_that_is_a_directory_or_special_file_is_refused(
    name, tmp_path, home, monkeypatch
):
    paths = write_inputs(tmp_path)
    paths[name].unlink()
    paths[name].mkdir()
    with pytest.raises(SystemExit) as refusal:
        run(load(), monkeypatch, home, paths, tmp_path / "result.json")
    assert str(refusal.value) == REFUSAL
    assert list(home.iterdir()) == []


@pytest.mark.parametrize(
    "root",
    ["relative/home", "/does/not/exist", "{home}/../escape", "{home}/missing"],
    ids=["relative", "absent", "parent-traversal", "missing-child"],
)
def test_an_unusable_home_root_is_refused_and_nothing_is_created_elsewhere(
    root, tmp_path, home, monkeypatch
):
    paths = write_inputs(tmp_path)
    before = sorted(path.name for path in tmp_path.iterdir())
    with pytest.raises(SystemExit) as refusal:
        run(
            load(),
            monkeypatch,
            root.format(home=home),
            paths,
            tmp_path / "result.json",
        )
    assert str(refusal.value) == REFUSAL
    assert sorted(path.name for path in tmp_path.iterdir()) == before
    assert list(home.iterdir()) == []


def test_a_symlinked_home_root_is_not_followed(tmp_path, home, monkeypatch):
    link = tmp_path / "link"
    link.symlink_to(home)
    with pytest.raises(SystemExit) as refusal:
        run(
            load(),
            monkeypatch,
            link,
            write_inputs(tmp_path),
            tmp_path / "result.json",
        )
    assert str(refusal.value) == REFUSAL
    assert list(home.iterdir()) == []


def test_the_valid_inputs_of_this_matrix_do_initialize(tmp_path, home, monkeypatch):
    """The control: every refusal above differs from this by one input."""
    result = tmp_path / "result.json"
    run(load(), monkeypatch, home, write_inputs(tmp_path), result)
    assert json.loads(result.read_bytes())["record"]["complete"] is True
    assert (home / "config.yaml").read_bytes() == json.dumps(CONFIG).encode()
