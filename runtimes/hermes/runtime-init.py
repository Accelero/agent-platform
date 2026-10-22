"""Validate personal native homes and refresh the installed runtime adapters.

personal-init.py creates the native configuration from manager-supplied input.
Startup only enforces the managed terminal backend and updates adapter code
supplied by the current image.
"""
import json
import os
from pathlib import Path

import yaml

from native_files import atomic_file, sync_tree


def overlay(user, admin):
    """Match Hermes' recursive managed-configuration overlay."""
    result = dict(user)
    for key, value in admin.items():
        current = result.get(key, {})
        result[key] = overlay(current, value) if isinstance(value, dict) and isinstance(current, dict) else value
    return result


def initialize(root: Path = Path('/opt/data'), plugin_home: Path = Path('/opt/hermes-stack/plugin-home')) -> None:
    os.umask(0o077)
    homes = [root]
    if (root / 'profiles').is_dir():
        homes.extend(profile for profile in sorted((root / 'profiles').iterdir())
                     if profile.is_dir() and (profile / 'config.yaml').is_file())
    # personal-run.py points HERMES_MANAGED_DIR at the manager's read-only policy.
    admin = {}
    if os.environ.get('HERMES_MANAGED_DIR'):
        admin = yaml.safe_load((Path(os.environ['HERMES_MANAGED_DIR']) / 'config.yaml').read_text())
        if not isinstance(admin, dict):
            raise RuntimeError('Managed Hermes configuration must be a mapping')
    source = plugin_home / 'plugins' / 'hindsight'
    if not source.is_dir():
        raise RuntimeError('Installed runtime adapter source is absent: ' + str(source))
    # Validate every home before changing any runtime file.
    for home in homes:
        config = yaml.safe_load((home / 'config.yaml').read_text()) or {}
        if not isinstance(config, dict):
            raise RuntimeError('Native Hermes configuration must be a YAML mapping')
        # Validate the effective native config without writing managed leaves
        # into the owner's retained file.
        config = overlay(config, admin)
        disabled = config.get('agent', {}).get('disabled_toolsets', [])
        # Native config saves omit keys the managed layer supplies; the effective value counts.
        backend = (config.get('terminal') or {}).get('backend')
        if 'terminal' not in disabled and backend != 'managed_execution':
            raise RuntimeError(f'Unsafe native terminal backend in {home / "config.yaml"}; configure managed_execution before startup')
        for file in source.rglob('*'):
            if not file.is_file() or '__pycache__' in file.parts:
                continue
            target = home / 'plugins' / 'hindsight' / file.relative_to(source)
            if any(path.is_symlink() or (path.exists() and not path.is_dir()) for path in target.parents):
                raise RuntimeError('Installed runtime adapters need ordinary directory parents: ' + str(target))
            if target.is_dir() and not target.is_symlink():
                raise RuntimeError('Installed runtime adapter file is a directory: ' + str(target))
    metadata = plugin_home / 'plugins/.install-metadata.json'
    installed = json.loads(metadata.read_text()) if metadata.is_file() else {}
    for home in homes:
        sync_tree(source, home / 'plugins' / 'hindsight', root, 'overwrite')
        if installed:
            local = home / 'plugins/.install-metadata.json'
            records = json.loads(local.read_text()) if local.is_file() else {}
            updated = {**records, **installed}
            if updated != records:
                atomic_file(local, (json.dumps(updated, indent=2) + '\n').encode(), root, 'overwrite')
    print('Native Hermes configuration preserved; installed runtime adapters refreshed.')


if __name__ == '__main__':
    initialize()
