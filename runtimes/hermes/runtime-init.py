"""Validate sandboxed native homes and refresh the installed runtime adapters.

Configuration, identities, skills and credentials belong to explicit seed/login
operations. Startup only updates adapter code supplied by the current image.
"""
import json
import os
from pathlib import Path

import yaml

from native_files import atomic_file, safe_parent, sync_tree


def initialize(root=Path('/opt/data'), plugin_home=Path('/opt/hermes-stack/plugin-home')):
    os.umask(0o077)
    if not (root / 'config.yaml').is_file():
        raise RuntimeError('Native Hermes home is uninitialized; run bin/seed --only native')
    homes = [root]
    if (root / 'profiles').is_dir():
        homes.extend(profile for profile in sorted((root / 'profiles').iterdir())
                     if profile.is_dir() and (profile / 'config.yaml').is_file())
    # Validate every home before changing any runtime file.
    for home in homes:
        config = yaml.safe_load((home / 'config.yaml').read_text()) or {}
        if not isinstance(config, dict):
            raise RuntimeError('Native Hermes configuration must be a YAML mapping')
        disabled = config.get('agent', {}).get('disabled_toolsets', [])
        if 'terminal' not in disabled and config.get('terminal', {}).get('backend') != 'managed_execution':
            raise RuntimeError(f'Unsafe native terminal backend in {home / "config.yaml"}; configure managed_execution before startup')
        for plugin in ('hindsight', 'managed_execution'):
            source = plugin_home / 'plugins' / plugin
            if not source.is_dir():
                raise RuntimeError('Installed runtime adapter source is absent: ' + str(source))
            for file in source.rglob('*'):
                if not file.is_file() or '__pycache__' in file.parts:
                    continue
                target = home / 'plugins' / plugin / file.relative_to(source)
                if any(path.is_symlink() or (path.exists() and not path.is_dir()) for path in target.parents):
                    raise RuntimeError('Installed runtime adapters need ordinary directory parents: ' + str(target))
                if target.is_dir() and not target.is_symlink():
                    raise RuntimeError('Installed runtime adapter file is a directory: ' + str(target))
    metadata = plugin_home / 'plugins/.install-metadata.json'
    installed = json.loads(metadata.read_text()) if metadata.is_file() else {}
    for home in homes:
        if home.parent == root / 'profiles':
            workspace = root / 'workspaces' / home.name
            if not safe_parent(workspace / '.workspace', root):
                raise RuntimeError('Profile workspace needs ordinary directory parents: ' + str(workspace))
        for plugin in ('hindsight', 'managed_execution'):
            sync_tree(plugin_home / 'plugins' / plugin, home / 'plugins' / plugin, root, 'overwrite')
        if installed:
            local = home / 'plugins/.install-metadata.json'
            records = json.loads(local.read_text()) if local.is_file() else {}
            updated = {**records, **installed}
            if updated != records:
                atomic_file(local, (json.dumps(updated, indent=2) + '\n').encode(), root, 'overwrite')
    print('Native Hermes configuration preserved; installed runtime adapters refreshed.')


if __name__ == '__main__':
    initialize()
