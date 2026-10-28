"""Exercise personal native home validation and runtime adapter refresh."""

import importlib.util
from pathlib import Path

import pytest

from agent_platform.build import ROOT

pytestmark = pytest.mark.unit


@pytest.fixture
def initializer(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.syspath_prepend(str(ROOT / "runtimes/hermes"))
    spec = importlib.util.spec_from_file_location(
        "runtime_init", ROOT / "runtimes/hermes/runtime-init.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _adapter_home(tmp_path: Path) -> Path:
    plugin_home = tmp_path / "plugin-home"
    (plugin_home / "plugins/hindsight").mkdir(parents=True)
    (plugin_home / "plugins/hindsight/plugin.yaml").write_text("name: hindsight\n")
    return plugin_home


def test_managed_backend_covers_profiles_saved_without_terminal_backend(
    tmp_path: Path, initializer, monkeypatch: pytest.MonkeyPatch
):
    home = tmp_path / "home"
    profile = home / "profiles/research"
    profile.mkdir(parents=True)
    # personal-init.py writes the root config from manager-supplied input.
    (home / "config.yaml").write_text("terminal: {backend: managed_execution}\n")
    # Native dashboard saves drop keys that the managed layer already supplies.
    (profile / "config.yaml").write_text("terminal: {cwd: /workspace}\nmodel: chosen\n")
    managed = tmp_path / "managed"
    managed.mkdir()
    (managed / "config.yaml").write_text("terminal: {backend: managed_execution}\n")
    monkeypatch.setenv("HERMES_MANAGED_DIR", str(managed))
    initializer.initialize(root=home, plugin_home=_adapter_home(tmp_path))
    assert (profile / "plugins/hindsight/plugin.yaml").is_file()
    assert "model: chosen" in (profile / "config.yaml").read_text()


def test_profile_without_managed_backend_policy_is_refused(
    tmp_path: Path, initializer, monkeypatch: pytest.MonkeyPatch
):
    home = tmp_path / "home"
    profile = home / "profiles/research"
    profile.mkdir(parents=True)
    # personal-init.py writes the root config from manager-supplied input.
    (home / "config.yaml").write_text("terminal: {backend: managed_execution}\n")
    (profile / "config.yaml").write_text("terminal: {backend: local}\n")
    managed = tmp_path / "managed"
    managed.mkdir()
    (managed / "config.yaml").write_text("model: managed\n")
    monkeypatch.setenv("HERMES_MANAGED_DIR", str(managed))
    with pytest.raises(RuntimeError, match="Unsafe native terminal backend"):
        initializer.initialize(root=home, plugin_home=_adapter_home(tmp_path))
    assert not (profile / "plugins").exists()


def test_missing_managed_policy_is_refused(
    tmp_path: Path, initializer, monkeypatch: pytest.MonkeyPatch
):
    home = tmp_path / "home"
    home.mkdir()
    (home / "config.yaml").write_text("terminal: {backend: managed_execution}\n")
    monkeypatch.delenv("HERMES_MANAGED_DIR", raising=False)
    with pytest.raises(RuntimeError, match="Managed Hermes configuration is required"):
        initializer.initialize(root=home, plugin_home=_adapter_home(tmp_path))
    assert not (home / "plugins").exists()
