"""Create a private native home once, with a signed, restart-safe receipt.

The init container receives a selected initialization key; the running Hermes
container does not. Live owner files are never overwritten by reconciliation.
"""

import argparse
import errno
import fcntl
import hashlib
import hmac
import json
import os
import re
import stat
import tempfile
import time
import uuid
from pathlib import Path
from urllib.parse import urlsplit

SCHEMA = 1
META = ".agent-platform"
INTENT = "native-init-intent.json"
RECEIPT = "native-init-receipt.json"


def canonical(value):
    return json.dumps(
        value, sort_keys=True, separators=(",", ":"), ensure_ascii=False
    ).encode()


def checked_context(value):
    if (
        not isinstance(value, dict)
        or set(value)
        != {
            "schemaVersion",
            "installationId",
            "instanceId",
            "materialId",
            "owner",
            "template",
        }
        or type(value["schemaVersion"]) is not int
        or value["schemaVersion"] != SCHEMA
    ):
        raise ValueError("invalid_initialization_context")
    for key in ("installationId", "instanceId", "materialId"):
        parsed = uuid.UUID(value[key])
        if parsed.int == 0 or str(parsed) != value[key]:
            raise ValueError("invalid_initialization_context")
    owner = value["owner"]
    if not isinstance(owner, dict) or set(owner) != {"issuer", "subject"}:
        raise ValueError("invalid_initialization_context")
    issuer, subject = owner["issuer"], owner["subject"]
    parsed = urlsplit(issuer)
    if (
        parsed.scheme != "https"
        or not parsed.hostname
        or parsed.username is not None
        or parsed.password is not None
        or parsed.query
        or parsed.fragment
        or len(issuer) > 2048
        or issuer != issuer.strip()
        or any(ord(c) < 33 for c in issuer)
        or not 1 <= len(subject) <= 255
        or not all(33 <= ord(c) <= 126 for c in subject)
    ):
        raise ValueError("invalid_initialization_context")
    template = value["template"]
    if (
        not isinstance(template, dict)
        or set(template) != {"id", "version"}
        or not re.fullmatch(r"[a-z][a-z0-9-]{0,62}", template["id"])
        or type(template["version"]) is not int
        or template["version"] < 1
    ):
        raise ValueError("invalid_initialization_context")
    return {
        "schemaVersion": SCHEMA,
        "installationId": value["installationId"],
        "instanceId": value["instanceId"],
        "materialId": value["materialId"],
        "ownerDigest": hashlib.sha256(canonical(owner)).hexdigest(),
    }


def external(path, limit):
    # Kubernetes projected ConfigMap/Secret files are deliberate symlinks. This
    # trusted, read-only input mount is separate from no-follow owner-home I/O.
    fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as source:
        if not stat.S_ISREG(os.fstat(source.fileno()).st_mode):
            raise ValueError("invalid_initialization_input")
        value = source.read(limit + 1)
    if not value or len(value) > limit:
        raise ValueError("invalid_initialization_input")
    return value


def read_at(directory, name, limit):
    try:
        fd = os.open(
            name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory
        )
    except FileNotFoundError:
        return None
    with os.fdopen(fd, "rb") as source:
        if not stat.S_ISREG(os.fstat(source.fileno()).st_mode):
            raise ValueError("invalid_initialization_state")
        value = source.read(limit + 1)
    if len(value) > limit:
        raise ValueError("invalid_initialization_state")
    return value


def create_at(directory, name, value):
    temporary = ".new-" + os.urandom(16).hex()
    fd = os.open(
        temporary,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
        0o600,
        dir_fd=directory,
    )
    try:
        with os.fdopen(fd, "wb") as output:
            output.write(value)
            output.flush()
            os.fsync(output.fileno())
        try:
            os.link(
                temporary,
                name,
                src_dir_fd=directory,
                dst_dir_fd=directory,
                follow_symlinks=False,
            )
        except FileExistsError:
            return False
        os.fsync(directory)
        return True
    finally:
        os.unlink(temporary, dir_fd=directory)
        os.fsync(directory)


def directory_at(parent, name):
    return os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)


def open_root(path):
    path = Path(path)
    if not path.is_absolute() or ".." in path.parts:
        raise ValueError("invalid_initialization_root")
    fd = os.open("/", os.O_RDONLY | os.O_DIRECTORY)
    try:
        for part in path.parts[1:]:
            child = directory_at(fd, part)
            os.close(fd)
            fd = child
        return fd
    except BaseException:
        os.close(fd)
        raise


def sign(key, purpose, value):
    return {
        "record": value,
        "signature": hmac.new(
            key, purpose + canonical(value), hashlib.sha256
        ).hexdigest(),
    }


def verify(key, purpose, raw):
    envelope = json.loads(raw)
    if not isinstance(envelope, dict) or set(envelope) != {"record", "signature"}:
        raise ValueError("invalid_initialization_receipt")
    signature = envelope["signature"]
    if not isinstance(signature, str) or not re.fullmatch(r"[a-f0-9]{64}", signature):
        raise ValueError("invalid_initialization_receipt")
    expected = sign(key, purpose, envelope["record"])["signature"]
    if not hmac.compare_digest(signature, expected):
        raise ValueError("invalid_initialization_receipt")
    return envelope["record"]


def fresh_root(root):
    with os.scandir(root) as entries:
        for entry in entries:
            if entry.name == META:
                continue
            if entry.name != "lost+found" or not entry.is_dir(follow_symlinks=False):
                return False
            fd = directory_at(root, entry.name)
            try:
                with os.scandir(fd) as found:
                    if next(found, None) is not None:
                        return False
            finally:
                os.close(fd)
    return True


def native_config(raw):
    value = json.loads(raw)
    if (
        not isinstance(value, dict)
        or value.get("terminal", {}).get("backend") != "managed_execution"
    ):
        raise ValueError("invalid_initialization_config")
    return value


def initialize(root, context_path, config_path, key_path, *, interrupt=None):
    os.umask(0o077)
    context = json.loads(external(context_path, 16384))
    binding = checked_context(context)
    config = external(config_path, 65536)
    native_config(config)
    key = external(key_path, 128).decode().strip()
    if not re.fullmatch(r"[a-f0-9]{64}", key):
        raise ValueError("invalid_initialization_key")
    key = bytes.fromhex(key)
    root_fd = open_root(root)
    metadata = lock = None
    try:
        try:
            metadata = directory_at(root_fd, META)
        except FileNotFoundError:
            if not fresh_root(root_fd):
                raise ValueError("unexplained_native_home") from None
            try:
                os.mkdir(META, mode=0o700, dir_fd=root_fd)
                os.fsync(root_fd)
            except FileExistsError:
                pass
            metadata = directory_at(root_fd, META)
        lock = os.open(
            "initialize.lock",
            os.O_WRONLY | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK,
            0o600,
            dir_fd=metadata,
        )
        if not stat.S_ISREG(os.fstat(lock).st_mode):
            raise ValueError("invalid_initialization_state")
        deadline = time.monotonic() + 10
        while True:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if time.monotonic() >= deadline:
                    raise ValueError("initialization_busy") from None
                time.sleep(0.05)
        expected = {
            "binding": binding,
            "configDigest": hashlib.sha256(config).hexdigest(),
            "template": context["template"],
        }
        existing = read_at(metadata, INTENT, 16384)
        if existing is None:
            if read_at(metadata, RECEIPT, 16384) is not None or not fresh_root(root_fd):
                raise ValueError("unexplained_native_home")
            with os.scandir(metadata) as entries:
                for entry in entries:
                    if entry.name != "initialize.lock" and not (
                        entry.name.startswith(".new-")
                        and entry.is_file(follow_symlinks=False)
                    ):
                        raise ValueError("unexplained_native_home")
            create_at(
                metadata,
                INTENT,
                canonical(sign(key, b"hermes-home-intent-v1\0", expected)),
            )
            existing = read_at(metadata, INTENT, 16384)
        intent = verify(key, b"hermes-home-intent-v1\0", existing)
        if intent.get("binding") != binding:
            raise ValueError("native_home_owner_conflict")
        receipt = read_at(metadata, RECEIPT, 16384)
        if receipt is not None:
            complete = verify(key, b"hermes-home-completion-v1\0", receipt)
            if (
                complete != {"intent": intent, "complete": True}
                or read_at(root_fd, "config.yaml", 16 * 1024 * 1024) is None
            ):
                raise ValueError("invalid_initialization_receipt")
            return receipt
        if intent != expected:
            raise ValueError("initialization_input_changed")
        if interrupt:
            interrupt("intent")
        installed = read_at(root_fd, "config.yaml", 65536)
        if installed is None:
            create_at(root_fd, "config.yaml", config)
            installed = read_at(root_fd, "config.yaml", 65536)
        if installed != config:
            raise ValueError("incomplete_initialization_conflict")
        if interrupt:
            interrupt("config")
        create_at(
            metadata,
            RECEIPT,
            canonical(
                sign(
                    key,
                    b"hermes-home-completion-v1\0",
                    {"intent": intent, "complete": True},
                )
            ),
        )
        receipt = read_at(metadata, RECEIPT, 16384)
        verify(key, b"hermes-home-completion-v1\0", receipt)
        return receipt
    finally:
        for descriptor in (lock, metadata, root_fd):
            if descriptor is not None:
                os.close(descriptor)


def write_result(result, receipt):
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(dir=result.parent, delete=False) as output:
            temporary = output.name
            output.write(receipt)
            output.flush()
            os.fsync(output.fileno())
        try:
            os.replace(temporary, result)
            temporary = None
        except OSError as error:
            if error.errno not in {errno.EBUSY, errno.EPERM, errno.EACCES}:
                raise
            # Kubelet bind-mounts the termination-message file. Renaming over
            # that mount is EBUSY (or EPERM under sticky /tmp). Only this
            # verified mountpoint may use a
            # no-follow descriptor write; ordinary results stay atomic.
            with open("/proc/self/mountinfo", encoding="utf-8") as source:
                mounted = any(
                    len(fields := line.split()) > 5 and fields[4] == str(result)
                    for line in source
                )
            if not mounted:
                raise ValueError("invalid_initialization_result") from None
            fd = os.open(result, os.O_WRONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
            with os.fdopen(fd, "wb") as output:
                info = os.fstat(output.fileno())
                if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
                    raise ValueError("invalid_initialization_result")
                os.ftruncate(output.fileno(), 0)
                output.write(receipt)
                output.flush()
                os.fsync(output.fileno())
    finally:
        if temporary is not None:
            os.unlink(temporary)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default="/opt/data")
    parser.add_argument("--context", default="/run/hermes/init/context.json")
    parser.add_argument("--config", default="/run/hermes/init/native-config.json")
    parser.add_argument("--key", default="/run/hermes/init/init-key")
    parser.add_argument("--result", default="/tmp/native-init-result.json")
    args = parser.parse_args()
    try:
        receipt = initialize(args.root, args.context, args.config, args.key)
        # /tmp survives application-container restarts. Never follow an old
        # owner-created result symlink or truncate an aliased home file.
        result = Path(args.result)
        write_result(result, receipt)
    except (OSError, ValueError, TypeError, KeyError, AttributeError):
        raise SystemExit(
            "Native initialization unavailable; private details suppressed."
        ) from None
    print("Native home initialization verified.")


if __name__ == "__main__":
    main()
