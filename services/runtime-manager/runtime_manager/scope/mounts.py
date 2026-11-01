"""Selected directory mounts from namespace-local persistent claims."""
from pathlib import PurePosixPath
import re


def mount_directory(spec, name, claim, relative, target, *, read_only=False, prefix=''):
    """Mount a selected subdirectory, never the whole backing claim.

    Callers must validate the actual source directory/symlinks before calling.
    Claim/prefix come from administrator-owned policy, not task requests.
    """
    if not re.fullmatch(r'[a-z0-9](?:[-a-z0-9.]{0,251}[a-z0-9])?', claim):
        raise ValueError('Invalid persistent claim name')
    path = PurePosixPath(relative)
    base = PurePosixPath(prefix)
    if (path.is_absolute() or base.is_absolute() or '..' in path.parts
            or '..' in base.parts or not path.parts):
        raise ValueError('A selected relative PVC directory is required')
    selected = str(base / path)
    if '\x00' in selected or '\\' in selected:
        raise ValueError('Invalid PVC directory')
    spec['volumes'].append({'name': name, 'persistentVolumeClaim': {'claimName': claim}})
    spec['volume_mounts'].append({'name': name, 'mountPath': str(target),
                                'subPath': selected, 'readOnly': read_only})
