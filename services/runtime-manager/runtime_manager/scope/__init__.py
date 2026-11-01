"""Mount scoping: what an invocation may see of the shared storage."""
from . import codex, hermes


def expand(template, *, runtime, mode, profile, session):
    """The template with this invocation's directories mounted, or ValueError.

    Codex templates (`runtime_guest_root`) scope a task, a worker home and a
    session store; Hermes templates (`workdir_scope`) scope a task workdir or a
    profile workspace; other templates accept no runtime metadata at all.
    """
    if not isinstance(runtime, dict):
        raise ValueError('Invalid invocation scope')
    if template.get('runtime_guest_root'):
        if mode != 'per_session':
            raise ValueError('Invalid Codex execution scope')
        return codex.runtime_template(template, runtime, session)
    if template.get('workdir_scope'):
        return hermes.workdir_template(template, runtime, mode, profile)
    if runtime:
        raise ValueError('Runtime mounts are not permitted for this template')
    return template
