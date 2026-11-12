"""Real worker SSH identity, host pinning and shutdown (X13, X14).

Builds the agent-runtime release target; no cluster or deployment checkout is
required. The release builder reruns the `artifact` cases on the pushed image.
"""

import json
import os
import subprocess
import time
from contextlib import contextmanager

import pytest

from tests.support.images import require_docker
from tests.support.plugin import load_plugin

pytestmark = pytest.mark.image


def run(*args, timeout=30, check=True, **kwargs):
    return subprocess.run(
        args, capture_output=True, text=True, timeout=timeout, check=check, **kwargs
    )


@pytest.fixture(scope="module")
def worker_image(platform_image):
    require_docker("ssh", "sftp", "ssh-keygen")
    return platform_image("agent-runtime")


@pytest.mark.artifact
def test_worker_image_ships_no_host_keys(platform_image):
    """Each Pod receives its own host key; the image must not carry one."""
    require_docker()
    run(
        "docker",
        "run",
        "--rm",
        "--network",
        "none",
        "--read-only",
        "--user",
        "0:0",
        "--entrypoint",
        "python3",
        platform_image("agent-runtime"),
        "-c",
        "from pathlib import Path; "
        "assert not list(Path('/etc/ssh').glob('ssh_host_*_key')); "
        "assert not Path('/etc/ssl/private/ssl-cert-snakeoil.key').exists()",
        timeout=120,
    )


@pytest.fixture
def keys(tmp_path, worker_image):
    for name in ("client", "host", "other-host", "untrusted-client"):
        run("ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(tmp_path / name))
    (tmp_path / "client.pub").chmod(0o444)
    # Kubernetes Secret projections are root-owned, including for arbitrary UIDs.
    run(
        "docker",
        "run",
        "--rm",
        "--user",
        "0:0",
        "--entrypoint",
        "chown",
        "-v",
        f"{tmp_path}:/keys",
        worker_image,
        "0:0",
        "/keys/client.pub",
    )
    # Secret projections are readable through the Pod's group; copy into mode 0600.
    (tmp_path / "host").chmod(0o444)
    return tmp_path


@contextmanager
def worker(image, keys, uid, gid, *, pinned=False):
    command = [
        "docker",
        "run",
        "--rm",
        "-d",
        "--read-only",
        "--user",
        f"{uid}:{gid}",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--tmpfs",
        "/tmp:rw,mode=1777",
        "--tmpfs",
        f"/home/worker:rw,mode=0700,uid={uid},gid={gid}",
        "--tmpfs",
        f"/workspace:rw,mode=0700,uid={uid},gid={gid}",
        "-p",
        "127.0.0.1::2222",
        "--mount",
        f"type=bind,src={keys / 'client.pub'},"
        "dst=/etc/ssh/authorized_keys/worker,readonly",
    ]
    if pinned:
        command += [
            "--mount",
            f"type=bind,src={keys / 'host'},"
            "dst=/etc/ssh/hostkeys/ssh_host_ed25519_key,readonly",
        ]
    container = run(*command, image).stdout.strip()
    try:
        bindings = json.loads(run("docker", "inspect", container).stdout)[0][
            "NetworkSettings"
        ]["Ports"]["2222/tcp"]
        port = bindings[0]["HostPort"]
        deadline = time.monotonic() + 10
        while True:
            result = ssh(keys, port, "true")
            if result.returncode == 0:
                break
            if time.monotonic() >= deadline:
                pytest.fail(result.stderr + run("docker", "logs", container).stderr)
            time.sleep(0.05)
        yield container, port
    finally:
        run("docker", "rm", "-f", container, check=False)


def ssh(keys, port, command, *, flags=(), key="client", env=None):
    return run(
        "ssh",
        "-F",
        "/dev/null",
        "-i",
        str(keys / key),
        "-p",
        str(port),
        "-o",
        "BatchMode=yes",
        "-o",
        "IdentitiesOnly=yes",
        "-o",
        "ConnectTimeout=1",
        *flags,
        "-o",
        "StrictHostKeyChecking=accept-new",
        "-o",
        "UserKnownHostsFile=" + str(keys / "known_hosts"),
        "worker@127.0.0.1",
        command,
        check=False,
        env=env,
    )


@pytest.mark.parametrize(
    "uid,gid",
    [
        (1000, 1000),
        (2000, 3000),
        (4000, 4000),
        (65534, 65533),
        (2147483647, 2147483647),
    ],
)
def test_worker_ssh_binds_the_account_to_the_pod_identity(worker_image, keys, uid, gid):
    """X13: authentication and session account lookup agree at arbitrary IDs."""
    with worker(worker_image, keys, uid, gid) as (container, port):
        result = ssh(keys, port, "id -u; id -g; id -un; pwd")
        assert result.returncode == 0, result.stderr
        assert result.stdout.splitlines() == [
            str(uid),
            str(gid),
            "worker",
            "/home/worker",
        ]
        assert ssh(keys, port, "true", key="untrusted-client").returncode != 0
        forwarded = ssh(
            keys,
            port,
            'printf "%s" "$EXECUTION_SMOKE_VALUE"',
            flags=("-o", "SendEnv=EXECUTION_SMOKE_VALUE"),
            env={**os.environ, "EXECUTION_SMOKE_VALUE": "forwarded-fixture"},
        )
        assert forwarded.returncode == 0 and forwarded.stdout == "forwarded-fixture"
        source, downloaded = keys / "source", keys / "downloaded"
        source.write_text("sftp fixture\n")
        transfer = run(
            "sftp",
            "-F",
            "/dev/null",
            "-i",
            str(keys / "client"),
            "-P",
            str(port),
            "-o",
            "BatchMode=yes",
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "UserKnownHostsFile=" + str(keys / "known_hosts"),
            "-b",
            "-",
            "worker@127.0.0.1",
            input=f"put {source} /workspace/probe\nget /workspace/probe {downloaded}\n",
            check=False,
        )
        assert transfer.returncode == 0, transfer.stderr
        assert downloaded.read_text() == source.read_text()
        # Retain v1 supervision and graceful signal handling.
        run(
            "docker",
            "kill",
            "--signal=" + ("INT" if uid == 2000 else "TERM"),
            container,
        )
        result = run("docker", "wait", container, check=False)
        assert result.returncode == 0 and result.stdout.strip() == "0", result.stderr


def test_worker_presents_the_provisioned_host_key_and_plugin_refuses_another(
    worker_image, keys, monkeypatch
):
    """X14: a real SSH exchange uses exactly the chart-provisioned identity."""
    plugin = load_plugin(monkeypatch)
    env = object.__new__(plugin.ManagedSSHEnvironment)
    env.settings = {"host_key_file": str(keys / "host.pub")}
    env.managed_environment, env.managed_generation = "a" * 24, "b" * 12
    env._pin_host_key()
    try:
        with worker(worker_image, keys, 2000, 3000, pinned=True) as (container, port):
            flags = env._target_flags("-p")
            result = ssh(keys, port, "id -u", flags=flags)
            assert result.returncode == 0, result.stderr
            assert result.stdout.strip() == "2000"
            mode = run(
                "docker",
                "exec",
                container,
                "stat",
                "-c",
                "%a",
                "/home/worker/hostkeys/ssh_host_ed25519_key",
            )
            assert mode.stdout.strip() == "600"
            env.settings["host_key_file"] = str(keys / "other-host.pub")
            env._pin_host_key()
            result = ssh(keys, port, "true", flags=env._target_flags("-p"))
            assert result.returncode != 0
            assert "Host key verification failed" in result.stderr
    finally:
        env._host_identity_dir.cleanup()
