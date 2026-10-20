"""Bind the managed terminal policy to every routed Hermes profile.

The pinned Hermes gateway builds a routed profile's TERMINAL_* scope from its
built-in defaults, the profile .env and the profile config.yaml, skipping the
managed /etc/hermes overlay that load_config() applies. A profile without its
own terminal.backend then falls back to local execution in the controller.
Applying the managed terminal section last makes it win, as it does elsewhere.
"""
import sys
from pathlib import Path

ANCHOR = '''        if isinstance(raw_terminal, dict):
            _apply(raw_terminal)
    _resolve_scope_cwd_placeholder(scope)
    return scope
'''
PATCH = '''        if isinstance(raw_terminal, dict):
            _apply(raw_terminal)
    # agent-platform: managed /etc/hermes terminal policy binds routed profiles too.
    from hermes_cli.managed_scope import load_managed_config
    managed_terminal = (load_managed_config() or {}).get("terminal")
    if isinstance(managed_terminal, dict):
        _apply(managed_terminal)
    _resolve_scope_cwd_placeholder(scope)
    return scope
'''


def main(path: Path) -> None:
    source = path.read_text()
    if source.count(ANCHOR) != 1:
        sys.exit(f'Unexpected Hermes terminal scope source in {path}; review the managed policy patch')
    path.write_text(source.replace(ANCHOR, PATCH))


if __name__ == '__main__':
    main(Path(sys.argv[1]))
