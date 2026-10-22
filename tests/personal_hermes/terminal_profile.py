"""Executed inside an isolated native Pod with synthetic identity and storage.

Exercise the actual native dashboard configuration handlers and the bundled
plugin against the real manager HTTPS API and SSH worker, without model access.
"""

import asyncio
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from types import SimpleNamespace


def main():
    owner, runtime_id, denied = sys.argv[1:]
    os.environ["HERMES_PERSONAL_INSTANCE_ID"] = json.loads(
        Path("/run/hermes/owner/context.json").read_text()
    )["instanceId"]
    os.environ["HERMES_HOME"] = "/opt/data"
    from hermes_cli.config import load_config
    from hermes_cli.profiles import create_profile
    from hermes_cli.web_models import ConfigUpdate
    from hermes_cli.web_routers.config_env import get_schema, update_config
    from hermes_cli.web_server_profiles import _profile_scope

    profile = "research"
    if not Path("/opt/data/profiles/research/config.yaml").exists():
        create_profile(profile, no_alias=True, no_skills=True)
    schema = asyncio.run(get_schema(profile=profile))
    assert "terminal.runtime_id" in schema["fields"]
    asyncio.run(
        update_config(
            ConfigUpdate(config={"terminal": {"runtime_id": runtime_id}}),
            profile=profile,
        )
    )
    spec = importlib.util.spec_from_file_location(
        "fixture_managed_execution", "/opt/hermes/plugins/managed_execution/__init__.py"
    )
    plugin = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(plugin)
    workspace = Path("/workspace")
    project = workspace / "projects/example"
    project.mkdir(parents=True, exist_ok=True)
    (project / "owner.txt").write_text(owner)
    (workspace / "not-mounted.txt").write_text("outside the permitted subpath")

    with _profile_scope(profile):
        assert load_config()["terminal"]["runtime_id"] == runtime_id
        settings = load_config()["plugins"]["entries"]["managed_execution"]["settings"]
        provider = plugin.Provider(
            SimpleNamespace(
                get_config=lambda key, default: settings.get(key, default),
                profile_name=profile,
            )
        )
        assert provider.settings()["runtime_id"] == runtime_id
        if denied == "1":
            try:
                provider.create_environment(
                    cwd="/workspace", timeout=30, task_id="denied-session"
                )
            except RuntimeError as error:
                assert "(403)" in str(error), str(error)
            else:
                raise AssertionError("Manager allowed a forbidden terminal backend")
        else:
            environment = provider.create_environment(
                cwd="/workspace", timeout=30, task_id="profile-" + runtime_id
            )
            try:
                assert environment._reply["ssh"]["host_key"].startswith("ssh-ed25519 ")
                with tempfile.TemporaryDirectory(
                    prefix="native-ssh-fixture-"
                ) as directory:
                    upload = Path(directory) / "upload.txt"
                    upload.write_text("profile transfer fixture")
                    environment._scp_upload(str(upload), "/workspace/uploaded.txt")
                    # A fresh connection must reject a different valid key,
                    # even while the ordinary transport has a ControlMaster.
                    client_public = subprocess.run(
                        ["ssh-keygen", "-y", "-f", settings["ssh_key"]],
                        capture_output=True,
                        text=True,
                        timeout=5,
                        check=True,
                    ).stdout.strip()
                    alias = (
                        "hermes-managed-"
                        + environment.managed_environment
                        + "-"
                        + environment.managed_generation
                    )
                    wrong = Path(directory) / "wrong-host"
                    wrong.write_text(alias + " " + client_public + "\n")
                    denied_ssh = subprocess.run(
                        [
                            "ssh",
                            "-F",
                            "/dev/null",
                            "-o",
                            "BatchMode=yes",
                            "-o",
                            "ConnectTimeout=3",
                            "-o",
                            "UserKnownHostsFile=" + str(wrong),
                            *environment._target_flags("-p"),
                            "worker@" + environment.host,
                            "true",
                        ],
                        capture_output=True,
                        timeout=8,
                    )
                    assert denied_ssh.returncode != 0, (
                        "SSH accepted a foreign host identity"
                    )
                    assert b"Host key verification failed" in denied_ssh.stderr, (
                        "Wrong-key connection failed for an unexpected reason"
                    )
                result = environment.execute(
                    "test ! -e /workspace/not-mounted.txt && "
                    "cat /workspace/owner.txt /workspace/uploaded.txt"
                )
                assert result["returncode"] == 0, result
                assert owner in result["output"], result
                assert "profile transfer fixture" in result["output"], result
                assert environment._known_hosts.is_file()
            finally:
                environment.cleanup()
    print("PASS: native profile selection and manager-enforced terminal access")


if __name__ == "__main__":
    main()
