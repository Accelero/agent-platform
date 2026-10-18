"""Atomic native-home writes shared by seeds, login imports and runtime updates."""
import os
from pathlib import Path
import tempfile

import yaml


def present(path):
    """A dangling link is native state too; missing mode must preserve it."""
    return path.exists() or path.is_symlink()


def safe_parent(path, root):
    """Do not follow a native parent link into a different mounted directory."""
    path, root = Path(path), Path(root)
    if not path.is_relative_to(root):
        raise ValueError('Seed target is outside its selected native mount')
    if any(parent.is_symlink() for parent in path.parents):
        return False
    for parent in reversed(path.parents):
        if parent == root or parent.is_relative_to(root):
            if parent.is_symlink():
                return False
            if parent.exists() and not parent.is_dir():
                return False
            parent.mkdir(exist_ok=True, mode=0o700)
    return True


def atomic_file(path, data, root, mode, *, merge_yaml=False):
    """Link an already complete temporary file for race-safe missing writes."""
    if mode == 'missing' and present(path):
        return False
    if path.is_dir() and not path.is_symlink():
        raise RuntimeError('Seed cannot replace a directory: ' + str(path))
    if not safe_parent(path, root):
        return False
    # Overwrite follows no user-created symlinks. Replacing this managed path
    # leaves an independently configured link destination untouched.
    if merge_yaml and path.is_file() and not path.is_symlink():
        existing = yaml.safe_load(path.read_text()) or {}
        managed = yaml.safe_load(data) or {}
        if not isinstance(existing, dict) or not isinstance(managed, dict):
            raise RuntimeError('Native configuration must be a YAML mapping')
        data = yaml.safe_dump(merge(existing, managed), sort_keys=False).encode()
    if path.is_file() and not path.is_symlink() and path.read_bytes() == data:
        return False
    descriptor, temporary = tempfile.mkstemp(prefix='.native-seed-', dir=path.parent)
    try:
        with os.fdopen(descriptor, 'wb') as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        os.chmod(temporary, 0o600)
        if mode == 'missing':
            try:
                os.link(temporary, path)
            except FileExistsError:
                return False
        else:
            os.replace(temporary, path)
        return True
    finally:
        if os.path.lexists(temporary):
            os.unlink(temporary)


def merge(existing, managed):
    for key, value in managed.items():
        if isinstance(value, dict) and isinstance(existing.get(key), dict):
            merge(existing[key], value)
        else:
            existing[key] = value
    return existing


def atomic_link(path, destination, root, mode):
    if mode == 'missing' and present(path):
        return False
    if path.is_symlink() and path.readlink() == Path(destination):
        return False
    if path.is_dir() and not path.is_symlink():
        raise RuntimeError('Seed cannot replace a directory: ' + str(path))
    if not safe_parent(path, root):
        return False
    if mode == 'missing':
        try:
            path.symlink_to(destination)
        except FileExistsError:
            return False
    else:
        temporary = path.parent / ('.native-seed-link-' + os.urandom(12).hex())
        try:
            temporary.symlink_to(destination)
            os.replace(temporary, path)
        finally:
            temporary.unlink(missing_ok=True)
    return True


def sync_tree(source, destination, root, mode):
    changes = 0
    if not source.is_dir():
        raise RuntimeError('Required seed directory is absent: ' + str(source))
    for entry in sorted(source.rglob('*')):
        if '__pycache__' in entry.parts:
            continue
        if entry.is_symlink():
            raise RuntimeError('Seed bundle must contain ordinary files only')
        if entry.is_file():
            changes += atomic_file(destination / entry.relative_to(source), entry.read_bytes(), root, mode)
    return changes
