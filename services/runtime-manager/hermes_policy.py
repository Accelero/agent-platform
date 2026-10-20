"""Select the invocation workspace; never add a profile workspace to a task."""
from copy import deepcopy
from pathlib import Path
import re

from kubernetes_mounts import mount_directory

PROFILE_TOKEN = '{profile}'


def profile_scope(scope, profile):
    """Expand a per-profile workspace mapping; a literal mapping is shared as configured."""
    if not any(PROFILE_TOKEN in str(scope.get(key, '')) for key in ('kubernetes_subpath', 'controller_root')):
        return scope
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_-]{0,63}', profile or ''):
        raise ValueError('Invalid profile for a per-profile workspace')
    return {**scope, **{key: scope[key].replace(PROFILE_TOKEN, profile)
                        for key in ('kubernetes_subpath', 'controller_root')}}


def profile_workdir(template, result, workdir, profile=''):
    scope = template.get('profile_workspace')
    if scope:
        scope = profile_scope(scope, profile)
    if not scope:
        # These profiles retain their native ephemeral execution workspace.
        if workdir != '/workspace':
            raise ValueError('No persistent workspace mapping for this invocation')
        return result
    path = Path(workdir)
    roots = (scope['mount_path'], scope['controller_root'])
    selected = next((path.relative_to(root) for root in roots
                     if path.is_relative_to(root)), None)
    if selected is None or any(not re.fullmatch(r'\.*[A-Za-z0-9_-][A-Za-z0-9_.-]{0,255}', p)
                               for p in selected.parts):
        raise ValueError('Invocation workspace is outside the selected profile')
    base = Path(scope['validation_root'])
    prefix = Path(scope['kubernetes_subpath'])
    if (len(prefix.parts) != 1 or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_.-]{0,255}', str(prefix))
            or not base.is_dir() or base.resolve() != base):
        raise ValueError('Profile workspace storage is unavailable')
    directory = base / prefix
    # The controller initializes this directory as the worker UID. The manager
    # holds only read-only validation views of application storage.
    validated = directory / selected
    if not validated.is_dir() or validated.resolve() != validated:
        raise ValueError('Profile workspace is missing or traverses a symlink')
    spec = result['kubernetes']
    mount_directory(spec, 'profile-workspace', scope['kubernetes_claim'], prefix / selected, path)
    return result


def workdir_template(template, runtime, mode, profile=''):
    if not isinstance(runtime, dict):
        raise ValueError('Invalid Hermes workdir scope')
    result = deepcopy(template)
    if not runtime:
        # Compatibility for an older controller during a rolling upgrade.
        if template.get('profile_workspace'):
            return profile_workdir(template, result, template['profile_workspace']['mount_path'], profile)
        return result
    if (set(runtime) not in ({'workdir'}, {'workdir', 'hermes_home'})
            or not isinstance(runtime['workdir'], str)):
        raise ValueError('Invalid Hermes invocation metadata')
    if 'hermes_home' in runtime:
        home = runtime['hermes_home']
        if (not isinstance(home, str) or not Path(home).is_absolute()
                or '..' in Path(home).parts or '\x00' in home):
            raise ValueError('Invalid Hermes home metadata')
    scope = template['workdir_scope']
    workdir = Path(runtime['workdir'])
    if not workdir.is_relative_to(scope['controller_root']):
        if 'hermes_home' not in runtime:
            raise ValueError('Ordinary workspace selection requires invocation metadata')
        return profile_workdir(template, result, str(workdir), profile)
    if mode != 'per_session':
        raise ValueError('Multica workdirs require session-scoped execution')
    relative = workdir.relative_to(scope['controller_root'])
    if (len(relative.parts) != 3 or relative.parts[-1] != 'workdir'
            or any(not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_.-]{0,255}', p)
                   for p in relative.parts[:2])):
        raise ValueError('A single managed Multica workdir is required')
    if runtime.get('hermes_home', str(workdir.parent / 'hermes-home')) != str(workdir.parent / 'hermes-home'):
        raise ValueError('Task workspace does not match the effective Hermes home')
    validated = Path(scope['validation_root']) / relative
    if not validated.is_dir() or validated.resolve() != validated:
        raise ValueError('Multica workdir is missing or traverses a symlink')
    spec = result['kubernetes']
    # Task API calls use the injected MAT, never the ordinary-chat profile PAT.
    removed = {m['name'] for m in spec['volume_mounts']
               if m['mountPath'] == '/home/worker/.multica'}
    spec['volume_mounts'] = [m for m in spec['volume_mounts'] if m['name'] not in removed]
    spec['volumes'] = [v for v in spec['volumes'] if v['name'] not in removed]
    if not scope.get('kubernetes_claim'):
        raise ValueError('A task-only persistent claim is required')
    mount_directory(spec, 'multica-workdir', scope['kubernetes_claim'], relative,
        workdir, read_only=scope['read_only'], prefix=scope.get('kubernetes_prefix', ''))
    if scope.get('identity_kubernetes_claim'):
        target = str(workdir.parent / 'hermes-home')
        if scope.get('identity_kubernetes_subpath'):
            mount_directory(spec, 'hermes-identity', scope['identity_kubernetes_claim'],
                scope['identity_kubernetes_subpath'], target)
        else:
            spec['volumes'].append({'name': 'hermes-identity',
                'persistentVolumeClaim': {'claimName': scope['identity_kubernetes_claim']}})
            spec['volume_mounts'].append({'name': 'hermes-identity',
                'mountPath': target, 'readOnly': False})
    return result
