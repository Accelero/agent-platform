"""Scoped native Codex task, worker-home and session-store claim mounts."""
from copy import deepcopy
from pathlib import Path
import re

from kubernetes_mounts import mount_directory

IDENTIFIER = r'[A-Za-z0-9][A-Za-z0-9_.-]{0,255}'


def runtime_template(template, runtime, session):
    if (not isinstance(runtime, dict)
            or set(runtime) - {'agent', 'task_root', 'sessions'}
            or not isinstance(runtime.get('task_root'), str)
            or not re.fullmatch(r'[a-zA-Z0-9_-]{1,128}', session)):
        raise ValueError('Invalid Codex runtime scope')
    guest = Path(template['runtime_guest_root'])
    agent = runtime.get('agent', '')
    if not isinstance(agent, str) or not re.fullmatch(r'[a-zA-Z0-9_-]{1,100}', agent):
        raise ValueError('Invalid Codex agent identity')
    task = Path(runtime['task_root'])
    relative_task = task.relative_to(guest / '.multica/workspaces')
    if len(relative_task.parts) != 2 or not all(re.fullmatch(IDENTIFIER, p) for p in relative_task.parts):
        raise ValueError('A single task workspace is required')
    home = guest / 'environments' / session / 'home'
    paths = [task, home]
    if runtime.get('sessions'):
        if not isinstance(runtime['sessions'], str):
            raise ValueError('Invalid Codex session store')
        store = Path(runtime['sessions'])
        relative = store.relative_to(guest / '.codex/multica-sessions')
        if (len(relative.parts) != 3 or relative.parts[1] != agent
                or not all(re.fullmatch(IDENTIFIER, p) for p in relative.parts)):
            raise ValueError('Session store belongs to another agent')
        paths.append(store)
    result = deepcopy(template)
    for index, path in enumerate(paths):
        # Manager mounts the same isolated claims read-only to validate directories.
        # A task cannot redirect its PVC subPath into another task or daemon state.
        if not path.is_dir() or path.resolve() != path:
            raise ValueError('Runtime path is missing or traverses a symlink')
        relative = path.relative_to(guest)
        matches = []
        for source in result.get('runtime_kubernetes_mounts', []):
            base = Path(source['path'])
            if relative.is_relative_to(base):
                matches.append((len(base.parts), source, relative.relative_to(base)))
        if not matches:
            raise ValueError('Runtime path has no worker-visible persistent claim')
        _, source, selected = max(matches, key=lambda item: item[0])
        mount_directory(result['kubernetes'], 'codex-runtime-' + str(index),
            source['claim'], selected, str(path) if path != home else '/home/worker',
            prefix=source.get('prefix', ''))
    return result
