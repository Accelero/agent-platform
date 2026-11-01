"""Runs inside the repository-built Hermes image; never uses a real model."""

import copy
import http.server
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path
from types import SimpleNamespace

import yaml

sys.path.insert(0, "/opt/hermes")
sys.path.insert(0, "/usr/local/lib")
from plugins.memory import load_memory_provider  # noqa: E402

HOME = Path("/opt/data")
SETTINGS = {
    "mode": "local_external",
    "api_url": "http://hindsight:8888",
    "bank_id": "default",
}


def initializer():
    spec = importlib.util.spec_from_file_location(
        "runtime_init", "/usr/local/lib/runtime-init.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def seed(data):
    os.environ["HERMES_HOME"] = str(HOME)
    initializer().seed_defaults(HOME, data["defaults"])
    initializer().initialize(root=HOME)


def provider(session="hindsight-image"):
    p = load_memory_provider("hindsight")
    assert p is not None and p.is_available()
    p.initialize(session)
    return p


def settings(home=HOME, **extra):
    path = home / "hindsight/config.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps({**SETTINGS, **extra}))


def connection(p):
    return {"mode": p._mode, "url": p._api_url, "bank": p._bank_id}


def retain(p, content):
    p.sync_turn(content, "Recorded for this synthetic fixture.")
    assert p._wait_for_retains_drained(90), "Retain did not finish"
    text, count = p._do_recall(content)
    assert count > 0 and content in text
    return count


def agent_memory(config):
    from agent.agent_init import _GATEWAY_IDENTITY_PARAMS, _init_memory

    agent = SimpleNamespace(
        enabled_toolsets=["memory"],
        disabled_toolsets=[],
        tools=[],
        valid_tool_names=set(),
        session_id="image-memory",
        session_cwd="/workspace",
        _session_db=None,
        _emit_warning=lambda *a: None,
        _emit_status=lambda *a: None,
        **{"_" + name: "" for name in _GATEWAY_IDENTITY_PARAMS},
    )
    _init_memory(agent, config, False, "cli")
    assert agent._memory_store is not None
    return agent


class CountingHandler(http.server.BaseHTTPRequestHandler):
    calls = 0
    status = 503
    delay = 0

    def do_GET(self):
        type(self).calls += 1
        time.sleep(self.delay)
        self.send_response(self.status)
        self.end_headers()
        self.wfile.write(b"{}")

    do_POST = do_GET

    def log_message(self, *args):
        pass


def counting_server():
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), CountingHandler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def no_provider(data, *, routed=False):
    seed(data)
    server = counting_server()
    os.environ["HINDSIGHT_API_URL"] = f"http://127.0.0.1:{server.server_port}"
    os.environ["HINDSIGHT_API_KEY"] = "synthetic-parent-key"
    if routed:
        from agent.secret_scope import set_secret_scope
        from hermes_constants import set_hermes_home_override

        home = HOME / "profiles/research"
        home.mkdir(parents=True, exist_ok=True)
        shutil.copytree(HOME / "plugins", home / "plugins", dirs_exist_ok=True)
        (home / "config.yaml").write_text("memory: {provider: hindsight}\n")
        set_hermes_home_override(home)
        set_secret_scope({}, profile_home=str(home))
        config = {"memory": {"provider": "hindsight"}}
    else:
        # This is the derived config whose writer the pinned Go test exercises.
        config = {"memory": {"provider": ""}}
        (HOME / "config.yaml").write_text(yaml.safe_dump(config))
    agent = agent_memory(config)
    assert agent._memory_manager is None
    # Exercise the real built-in store's persistent file path.
    from tools.memory_tool import memory_tool

    result = memory_tool(
        action="add",
        target="memory",
        content="Synthetic local memory fixture",
        store=agent._memory_store,
    )
    from hermes_constants import get_hermes_home

    assert (
        "Synthetic local memory fixture"
        in (get_hermes_home() / "memories/MEMORY.md").read_text()
    )
    assert "error" not in json.loads(result)
    assert CountingHandler.calls == 0
    return {"calls": CountingHandler.calls, "builtin": True}


def routed(data):
    seed(data)
    from agent.secret_scope import get_secret, reset_secret_scope, set_secret_scope
    from hermes_constants import reset_hermes_home_override, set_hermes_home_override

    home = HOME / "profiles/research"
    home.mkdir(parents=True, exist_ok=True)
    shutil.copytree(HOME / "plugins", home / "plugins", dirs_exist_ok=True)
    (home / "config.yaml").write_text(
        "memory: {provider: hindsight}\nmodel: native-choice\n"
    )
    settings(home, bank_id=data["bank"], recall_types="observation,world,experience")
    before = (home / "hindsight/config.json").read_bytes()
    token = set_hermes_home_override(home)
    scope = set_secret_scope({}, profile_home=str(home))
    try:
        assert get_secret("HINDSIGHT_API_URL", "") == ""
        p = provider()
        assert p._bank_id == data["bank"] and not p._api_key
        retain(p, data["content"])
        p.shutdown()
    finally:
        reset_secret_scope(scope)
        reset_hermes_home_override(token)
    initializer().initialize(root=HOME)
    assert (home / "hindsight/config.json").read_bytes() == before
    assert (
        yaml.safe_load((home / "config.yaml").read_text())["memory"]["provider"]
        == "hindsight"
    )
    return {"bank": data["bank"], "credential": False}


class ModelHandler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length", "0")))
        body = json.dumps(
            {
                "id": "synthetic",
                "object": "chat.completion",
                "created": 1,
                "model": "synthetic-model",
                "choices": [
                    {
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": "Synthetic fixture answer.",
                        },
                        "finish_reason": "stop",
                    }
                ],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 5,
                    "total_tokens": 15,
                },
            }
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


def unavailable(data):
    seed(data)
    server = counting_server()
    CountingHandler.status = data.get("status", 503)
    CountingHandler.delay = data.get("delay", 0)
    settings(api_url=f"http://127.0.0.1:{server.server_port}")
    config = yaml.safe_load((HOME / "config.yaml").read_text())
    config["model"] = {"streaming": False}
    config["agent"] = {"environment_probe": False}
    (HOME / "config.yaml").write_text(yaml.safe_dump(config))
    p = provider()
    from agent.memory_manager import MemoryManager
    from run_agent import AIAgent

    manager = MemoryManager()
    manager.add_provider(p)
    if not CountingHandler.delay:
        p._recall_sync = True
    p.queue_prefetch("Synthetic ordinary conversation")
    model = http.server.ThreadingHTTPServer(("127.0.0.1", 0), ModelHandler)
    threading.Thread(target=model.serve_forever, daemon=True).start()
    agent = AIAgent(
        base_url=f"http://127.0.0.1:{model.server_port}/v1",
        api_key="synthetic-model-key",
        provider="openai",
        api_mode="chat_completions",
        model="synthetic-model",
        enabled_toolsets=["memory"],
        quiet_mode=True,
        skip_context_files=True,
        skip_background_review=True,
        max_iterations=2,
        memory_manager=manager,
    )
    start = time.monotonic()
    result = agent.run_conversation("Say a synthetic fixture answer.")
    elapsed = time.monotonic() - start
    assert result["final_response"] == "Synthetic fixture answer."
    assert result["completed"] and not result["failed"]
    assert elapsed < 10
    assert CountingHandler.calls > 0
    return {"elapsed": elapsed, "calls": CountingHandler.calls, "context": ""}


def existing_home(data):
    defaults = copy.deepcopy(data["defaults"])
    defaults["config"]["memory"]["provider"] = ""
    defaults.pop("hindsight", None)
    seed({**data, "defaults": defaults})


def dashboard(data):
    existing_home(data)
    from fastapi import FastAPI
    from fastapi.testclient import TestClient
    from hermes_cli.config import load_config
    from hermes_cli.web_routers import memory_providers, ops

    # The real dashboard routers and persistence, isolated from transport auth.
    app = FastAPI()
    app.include_router(memory_providers.router)
    app.include_router(ops.router)
    client = TestClient(app)
    before = copy.deepcopy(load_config())
    with client:
        r = client.get("/api/memory/providers/hindsight/config")
        assert r.status_code == 200, r.text
        fields = {item["key"]: item for item in r.json()["fields"]}
        assert "mode" in fields and "api_url" in fields and "bank_id" in fields
        # Use exactly the panel values documented in the operator guide.
        r = client.put(
            "/api/memory/providers/hindsight/config",
            json={
                "values": {
                    "mode": "local_external",
                    "api_url": "http://hindsight:8888",
                    "bank_id": data["bank"],
                    "api_key": "",
                }
            },
        )
        assert r.status_code == 200, r.text
        assert load_config()["memory"]["provider"] == "hindsight"
        p = provider("new-session-after-dashboard-save")
        assert connection(p) == {
            "mode": "local_external",
            "url": "http://hindsight:8888",
            "bank": data["bank"],
        }
        assert not p._api_key
        # Stub consolidation is asynchronous. Widen only this probe's recall
        # types so dashboard persistence remains exactly the documented fields.
        p._apply_recall_settings({"recall_types": "observation,world,experience"})
        retain(p, data["content"])
        p.shutdown()
        r = client.put("/api/memory/provider", json={"provider": ""})
        assert r.status_code == 200, r.text
    after = load_config()
    before.setdefault("memory", {})["provider"] = ""
    assert after == before
    assert agent_memory(after)._memory_manager is None
    return {
        "bank": data["bank"],
        "disabled": True,
        "takes_effect": "new session or gateway restart",
    }


def documented(data):
    existing_home(data)
    from hermes_cli.config import load_config

    (HOME / "hindsight/config.json").unlink(missing_ok=True)
    original = copy.deepcopy(load_config())
    secret = HOME / ".env"
    secret.write_text("EXAMPLE_API_KEY=synthetic-existing-credential\n")
    before_secret = secret.read_bytes()
    for name in ("activate", "deactivate"):
        # Exact inner shell from the README, extracted by the host test.
        result = subprocess.run(
            ["sh", "-ceu", data[name]], capture_output=True, text=True, timeout=30
        )
        assert result.returncode == 0, result.stderr
        expected = copy.deepcopy(original)
        expected.setdefault("memory", {})["provider"] = (
            "hindsight" if name == "activate" else ""
        )
        assert load_config() == expected
        assert secret.read_bytes() == before_secret
        if name == "activate":
            p = provider("new-session-after-native-command")
            assert connection(p) == {
                "mode": "local_external",
                "url": "http://hindsight:8888",
                "bank": "default",
            }
            p.shutdown()
        else:
            assert agent_memory(load_config())._memory_manager is None
    return {"disabled": True, "takes_effect": "new session or gateway restart"}


def main(action, data):
    if action == "derived":
        return no_provider(data)
    if action == "routed-missing":
        return no_provider(data, routed=True)
    if action == "routed":
        return routed(data)
    if action == "unavailable":
        return unavailable(data)
    if action == "dashboard":
        return dashboard(data)
    if action == "documented":
        return documented(data)
    seed(data)
    if action == "missing-json":
        path = HOME / "hindsight/config.json"
        if data["settings"] is None:
            path.unlink()
        else:
            path.write_text(json.dumps(data["settings"]))
    elif action in ("retain", "recall", "real-retain"):
        settings(
            bank_id=data["bank"],
            recall_types=data.get("recall_types", "observation,world,experience"),
        )
    p = provider()
    result = connection(p)
    if action == "seeded":
        p.sync_turn("Synthetic fresh root uses bank default.", "Fixture recorded.")
        assert p._wait_for_retains_drained(90)
    elif action == "real-retain":
        # Ordinary plugin defaults, including observation-only recall. This optional
        # test waits for real LLM consolidation rather than widening recall types.
        p._apply_recall_settings({})
        p.sync_turn(data["content"], "Fixture recorded.")
        assert p._wait_for_retains_drained(90)
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            _, count = p._do_recall("Which telescope does the observatory use?")
            if count:
                result["count"] = count
                break
            time.sleep(1)
        else:
            raise AssertionError("No observations recalled after real model retain")
    elif action == "retain":
        result["count"] = retain(p, data["content"])
    elif action == "recall":
        text, count = p._do_recall(data["content"])
        assert count > 0 and data["content"] in text
        result["count"] = count
    p.shutdown()
    return result


if __name__ == "__main__":
    data = json.load(sys.stdin)
    result = main(sys.argv[1], data)
    print("HINDSIGHT_RESULT=" + json.dumps(result), flush=True)
    # A black-holed HTTP request runs on a daemon thread; model turns must not wait
    # for Python's interpreter shutdown or provider background-drain cleanup.
    os._exit(0)
