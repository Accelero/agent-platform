"""Native home initialization survives real abrupt death and preserves owner state."""

import importlib.util
import json
import multiprocessing
import os
import signal
from pathlib import Path

import pytest

SOURCE = Path(__file__).resolve().parents[1] / "runtimes/hermes/personal-init.py"


def module():
    spec = importlib.util.spec_from_file_location("personal_init", SOURCE)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


@pytest.fixture
def fixture(tmp_path):
    root = tmp_path / "home"
    root.mkdir()
    context = tmp_path / "context.json"
    context.write_text(
        json.dumps(
            {
                "schemaVersion": 1,
                "installationId": "00000000-0000-4000-8000-000000000001",
                "instanceId": "00000000-0000-4000-8000-000000000002",
                "materialId": "00000000-0000-4000-8000-000000000003",
                "owner": {
                    "issuer": "https://sso.example.org/realms/agents",
                    "subject": "owner-a",
                },
                "template": {"id": "personal-default", "version": 1},
            }
        )
    )
    config = tmp_path / "native-config.json"
    config.write_text(json.dumps({"terminal": {"backend": "managed_execution"}}))
    key = tmp_path / "initialization-key"
    key.write_text("2a" * 32)
    key.chmod(0o600)
    return module(), root, context, config, key


def invoke(fixture, **kwargs):
    initializer, root, context, config, key = fixture
    return initializer.initialize(root, context, config, key, **kwargs)


def test_termination_message_replaces_an_untrusted_symlink_without_touching_owner_file(
    fixture, monkeypatch, tmp_path
):
    initializer, root, context, config, key = fixture
    victim = tmp_path / "owner-file"
    victim.write_text("owner state")
    result = tmp_path / "result.json"
    result.symlink_to(victim)
    monkeypatch.setattr(
        "sys.argv",
        [
            "personal-init",
            "--root",
            str(root),
            "--context",
            str(context),
            "--config",
            str(config),
            "--key",
            str(key),
            "--result",
            str(result),
        ],
    )
    initializer.main()
    assert victim.read_text() == "owner state"
    assert not result.is_symlink()
    receipt = json.loads(result.read_bytes())
    assert receipt["record"]["complete"] is True


@pytest.mark.parametrize("stage", ["intent", "config"])
def test_abrupt_death_resumes_committed_intent_without_reseeding(fixture, stage):
    def child():
        def interrupt(current):
            if current == stage:
                os.kill(os.getpid(), signal.SIGKILL)

        invoke(fixture, interrupt=interrupt)

    process = multiprocessing.get_context("fork").Process(target=child)
    process.start()
    process.join(timeout=5)
    if process.is_alive():
        process.kill()
        process.join(timeout=5)
        pytest.fail("Native initializer exceeded its bound")
    assert process.exitcode == -signal.SIGKILL
    initializer, root, _, config, _ = fixture
    assert (root / initializer.META / initializer.INTENT).is_file()
    assert not (root / initializer.META / initializer.RECEIPT).exists()
    invoke(fixture)
    assert (root / "config.yaml").read_bytes() == config.read_bytes()
    assert (root / initializer.META / initializer.RECEIPT).is_file()


def test_completed_home_preserves_settings_profiles_memory_and_original_seed(fixture):
    invoke(fixture)
    initializer, root, context, config, _ = fixture
    receipt = (root / initializer.META / initializer.RECEIPT).read_bytes()
    files = {
        "config.yaml": (
            b"terminal:\n  backend: managed_execution\ncustom_setting: owner\n"
        ),
        "profiles/custom/config.yaml": b"owner setting fixture",
        "memories/owner.md": b"owner memory fixture",
        "agents/custom.yaml": b"owner agent fixture",
        ".multica/config.json": b"owner enrollment fixture",
    }
    for path, value in files.items():
        target = root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(value)
    value = json.loads(context.read_text())
    value["template"]["version"] = 2
    context.write_text(json.dumps(value))
    config.write_text(
        json.dumps({"terminal": {"backend": "managed_execution"}, "new_default": True})
    )
    invoke(fixture)
    for path, value in files.items():
        assert (root / path).read_bytes() == value
    assert (root / initializer.META / initializer.RECEIPT).read_bytes() == receipt


@pytest.mark.parametrize("field", ["subject", "instanceId", "materialId"])
def test_foreign_owner_instance_or_material_cannot_adopt_an_existing_home(
    fixture, field
):
    invoke(fixture)
    _, root, context, _, _ = fixture
    original = (root / "config.yaml").read_bytes()
    value = json.loads(context.read_text())
    if field == "subject":
        value["owner"]["subject"] = "owner-b"
    else:
        value[field] = "00000000-0000-4000-8000-000000000004"
    context.write_text(json.dumps(value))
    with pytest.raises(ValueError, match="native_home_owner_conflict"):
        invoke(fixture)
    assert (root / "config.yaml").read_bytes() == original


def test_unexplained_existing_home_is_not_bootstrapped(fixture):
    initializer, root, _, _, _ = fixture
    (root / "config.yaml").write_bytes(b"preexisting owner settings")
    with pytest.raises(ValueError, match="unexplained_native_home"):
        invoke(fixture)
    assert (root / "config.yaml").read_bytes() == b"preexisting owner settings"
    assert not (root / initializer.META).exists()


def test_changed_key_or_tampered_receipt_does_not_replace_state(fixture):
    invoke(fixture)
    initializer, root, _, _, key = fixture
    receipt = root / initializer.META / initializer.RECEIPT
    original = (root / "config.yaml").read_bytes()
    key.write_text("2b" * 32)
    with pytest.raises(ValueError, match="invalid_initialization_receipt"):
        invoke(fixture)
    key.write_text("2a" * 32)
    value = json.loads(receipt.read_text())
    value["record"]["complete"] = False
    receipt.write_text(json.dumps(value))
    with pytest.raises(ValueError, match="invalid_initialization_receipt"):
        invoke(fixture)
    assert (root / "config.yaml").read_bytes() == original


def test_partial_home_cannot_mix_new_template_inputs(fixture):
    def interrupt(stage):
        if stage == "intent":
            raise InterruptedError("fixture interruption")

    with pytest.raises(InterruptedError):
        invoke(fixture, interrupt=interrupt)
    _, root, _, config, _ = fixture
    config.write_text(
        json.dumps({"terminal": {"backend": "managed_execution"}, "new_default": True})
    )
    with pytest.raises(ValueError, match="initialization_input_changed"):
        invoke(fixture)
    assert not (root / "config.yaml").exists()


@pytest.mark.parametrize("target", ["metadata", "config", "root", "fifo"])
def test_initialization_cannot_follow_owner_links_or_special_files(
    fixture, tmp_path, target
):
    initializer, root, context, config, key = fixture
    outside = tmp_path / "outside"
    outside.mkdir()
    marker = outside / "marker"
    marker.write_bytes(b"foreign fixture data")
    if target == "metadata":
        (root / initializer.META).symlink_to(outside, target_is_directory=True)
    elif target == "root":
        alias = tmp_path / "alias"
        alias.symlink_to(root, target_is_directory=True)
        fixture = (initializer, alias, context, config, key)
    else:

        def interrupt(stage):
            if stage == "intent":
                raise InterruptedError("fixture interruption")

        with pytest.raises(InterruptedError):
            invoke(fixture, interrupt=interrupt)
        if target == "config":
            (root / "config.yaml").symlink_to(marker)
        else:
            os.mkfifo(root / "config.yaml")
    with pytest.raises((OSError, ValueError)):
        invoke(fixture)
    assert marker.read_bytes() == b"foreign fixture data"


def test_concurrent_initializers_produce_one_receipt(fixture):
    processes = [
        multiprocessing.get_context("fork").Process(target=invoke, args=(fixture,))
        for _ in range(4)
    ]
    for process in processes:
        process.start()
    for process in processes:
        process.join(timeout=5)
        if process.is_alive():
            process.kill()
            process.join(timeout=5)
        assert process.exitcode == 0
    initializer, root, _, config, _ = fixture
    assert (root / "config.yaml").read_bytes() == config.read_bytes()
    invoke(fixture)
    assert (root / initializer.META / initializer.RECEIPT).is_file()
