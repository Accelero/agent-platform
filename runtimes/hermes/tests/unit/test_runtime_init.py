"""Exercise generic native configuration initialization and state preservation."""

import json
from pathlib import Path

import pytest
import yaml


@pytest.mark.parametrize("name", ["../escape", ".", "default", "custom", "a/b"])
def test_profile_defaults_cannot_write_outside_native_home(
    tmp_path: Path, initializer, name: str
):
    with pytest.raises(ValueError, match="profile"):
        initializer.seed_defaults(tmp_path, {"config": {}, "profiles": {name: {}}})
    assert not list(tmp_path.iterdir())


def test_config_seed_refuses_symlinked_parent(tmp_path: Path, initializer):
    outside = tmp_path / "outside"
    outside.mkdir()
    home = tmp_path / "home"
    home.mkdir()
    (home / "profiles").symlink_to(outside, target_is_directory=True)
    with pytest.raises(RuntimeError, match="initialize"):
        initializer.seed_defaults(home, {"config": {}, "profiles": {"research": {}}})
    assert not list(outside.iterdir())


def _adapter_home(tmp_path: Path) -> Path:
    plugin_home = tmp_path / "plugin-home"
    (plugin_home / "plugins/hindsight").mkdir(parents=True)
    (plugin_home / "plugins/hindsight/plugin.yaml").write_text("name: hindsight\n")
    return plugin_home


def test_managed_backend_covers_profiles_saved_without_terminal_backend(
    tmp_path: Path, initializer
):
    home = tmp_path / "home"
    profile = home / "profiles/research"
    legacy = profile / "plugins/managed_execution"
    legacy.mkdir(parents=True)
    (legacy / "plugin.yaml").write_text("name: managed_execution\n")
    # Native dashboard saves drop keys that the managed layer already supplies.
    (profile / "config.yaml").write_text("terminal: {cwd: /workspace}\nmodel: chosen\n")
    managed = tmp_path / "managed.yaml"
    managed.write_text("terminal: {backend: managed_execution}\n")
    initializer.initialize(
        root=home, plugin_home=_adapter_home(tmp_path), managed=managed
    )
    assert not legacy.exists()
    assert (profile / "plugins/hindsight/plugin.yaml").is_file()
    assert (home / "workspaces/research").is_dir()
    assert "model: chosen" in (profile / "config.yaml").read_text()


def test_profile_without_managed_backend_policy_is_refused(tmp_path: Path, initializer):
    home = tmp_path / "home"
    profile = home / "profiles/research"
    profile.mkdir(parents=True)
    (profile / "config.yaml").write_text("terminal: {backend: local}\n")
    with pytest.raises(RuntimeError, match="Unsafe native terminal backend"):
        initializer.initialize(
            root=home,
            plugin_home=_adapter_home(tmp_path),
            managed=tmp_path / "absent.yaml",
        )
    assert not (profile / "plugins").exists()


def hindsight_seed():
    return {
        "config": {"memory": {"provider": "hindsight"}},
        "profiles": {"research": {"memory": {"provider": ""}}},
        "hindsight": {
            "mode": "local_external",
            "api_url": "http://hindsight:8888",
            "bank_id": "default",
        },
    }


def test_h3_fresh_root_seed_writes_provider_and_connection(tmp_path, initializer):
    seed = hindsight_seed()
    initializer.seed_defaults(tmp_path, seed)
    assert (
        yaml.safe_load((tmp_path / "config.yaml").read_text())["memory"]["provider"]
        == "hindsight"
    )
    path = tmp_path / "hindsight/config.json"
    assert json.loads(path.read_text()) == seed["hindsight"]
    assert path.stat().st_mode & 0o777 == 0o600
    assert not (tmp_path / "profiles/research/hindsight").exists()
    config = yaml.safe_load((tmp_path / "profiles/research/config.yaml").read_text())
    assert config["memory"]["provider"] == ""


@pytest.mark.parametrize(
    "settings",
    [
        None,
        {},
        {"mode": "cloud"},
        {
            "mode": "local_external",
            "api_url": "http://hindsight:8888",
            "bank_id": "default",
            "api_key": "synthetic-private-value",
        },
        {
            "mode": "local_external",
            "api_url": "https://user:synthetic-private-value@example.org",
            "bank_id": "default",
        },
    ],
)
def test_h8_seed_rejects_credentials_in_hindsight_settings(
    tmp_path, initializer, settings
):
    seed = hindsight_seed()
    seed["hindsight"] = settings
    with pytest.raises(ValueError, match="Invalid Hindsight seed settings") as error:
        initializer.seed_defaults(tmp_path, seed)
    assert "synthetic-private-value" not in str(error.value)
    assert not list(tmp_path.iterdir())


def test_h5_existing_home_is_not_changed_by_seeding(tmp_path, initializer):
    (tmp_path / "config.yaml").write_text(
        "memory: {provider: chosen}\nmodel: selected\n"
    )
    (tmp_path / ".env").write_text("EXAMPLE_API_KEY=synthetic-credential\n")
    seed = hindsight_seed()
    seed["profiles"] = {}
    before = {path.name: path.read_bytes() for path in tmp_path.iterdir()}
    initializer.seed_defaults(tmp_path, seed)
    assert {path.name: path.read_bytes() for path in tmp_path.iterdir()} == before
    (tmp_path / "hindsight").mkdir()
    path = tmp_path / "hindsight/config.json"
    path.write_text('{"bank_id":"user-bank","api_key":"synthetic-credential"}')
    before_json = path.read_bytes()
    initializer.seed_defaults(tmp_path, seed)
    assert path.read_bytes() == before_json


def test_h5_explicit_empty_provider_is_preserved(tmp_path, initializer):
    seed = hindsight_seed()
    seed["config"]["memory"]["provider"] = ""
    del seed["hindsight"]
    initializer.seed_defaults(tmp_path, seed)
    assert (
        yaml.safe_load((tmp_path / "config.yaml").read_text())["memory"]["provider"]
        == ""
    )
    assert not (tmp_path / "hindsight").exists()


@pytest.mark.parametrize("existing_target", [False, True])
def test_h5_seed_refuses_symlinked_hindsight_parent(
    tmp_path, initializer, existing_target
):
    outside = tmp_path / "outside"
    outside.mkdir()
    if existing_target:
        (outside / "config.json").write_text('{"bank_id":"private-bank"}')
    before = {path.name: path.read_bytes() for path in outside.iterdir()}
    home = tmp_path / "home"
    home.mkdir()
    (home / "hindsight").symlink_to(outside, target_is_directory=True)
    with pytest.raises(RuntimeError, match="Hindsight"):
        initializer.seed_defaults(home, hindsight_seed())
    assert {path.name: path.read_bytes() for path in outside.iterdir()} == before
    assert not (home / "config.yaml").exists()


def test_h5_seed_refuses_symlinked_hindsight_file(tmp_path, initializer):
    (tmp_path / "hindsight").mkdir()
    outside = tmp_path / "user-settings.json"
    outside.write_text("{}")
    (tmp_path / "hindsight/config.json").symlink_to(outside)
    with pytest.raises(RuntimeError, match="Hindsight"):
        initializer.seed_defaults(tmp_path, hindsight_seed())
    assert outside.read_text() == "{}"


def test_h5_interrupted_seed_converges(tmp_path, initializer, monkeypatch):
    real_write = initializer.atomic_file

    def interrupted(path, *args, **kwargs):
        if path.name == "config.yaml":
            raise OSError("simulated interruption")
        return real_write(path, *args, **kwargs)

    monkeypatch.setattr(initializer, "atomic_file", interrupted)
    with pytest.raises(OSError, match="interruption"):
        initializer.seed_defaults(tmp_path, hindsight_seed())
    settings = tmp_path / "hindsight/config.json"
    before = settings.read_bytes()
    assert not (tmp_path / "config.yaml").exists()
    monkeypatch.setattr(initializer, "atomic_file", real_write)
    initializer.seed_defaults(tmp_path, hindsight_seed())
    assert settings.read_bytes() == before
    assert (
        yaml.safe_load((tmp_path / "config.yaml").read_text())["memory"]["provider"]
        == "hindsight"
    )


def test_h4_native_profile_settings_survive_restart(tmp_path, initializer):
    seed = hindsight_seed()
    initializer.seed_defaults(tmp_path, seed)
    profile = tmp_path / "profiles/research"
    (profile / "config.yaml").write_text(
        "memory: {provider: hindsight}\nmodel: chosen\n"
    )
    (profile / "hindsight").mkdir()
    (profile / "hindsight/config.json").write_text(
        '{"mode":"local_external","api_url":"http://hindsight:8888","bank_id":"chosen-bank"}'
    )
    before = {
        path.relative_to(profile): path.read_bytes()
        for path in profile.rglob("*")
        if path.is_file()
    }
    for provider in ("hindsight", "other", ""):
        seed["config"]["memory"]["provider"] = provider
        initializer.seed_defaults(tmp_path, seed)
    assert {
        path.relative_to(profile): path.read_bytes()
        for path in profile.rglob("*")
        if path.is_file()
    } == before
