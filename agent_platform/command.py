"""Bound operational commands while keeping credentials out of diagnostics."""

from __future__ import annotations

import asyncio
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Sequence
    from pathlib import Path

_COMMAND_ERRORS = (OSError, TimeoutError)


async def _run(
    args: Sequence[str],
    data: bytes | None,
    seconds: float,
    cwd: Path | None,
    *,
    include_stderr: bool,
    diagnostics: Path | None,
) -> bytes:
    process = await asyncio.create_subprocess_exec(
        *args,
        cwd=cwd,
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    try:
        stdout, stderr = await asyncio.wait_for(process.communicate(data), seconds)
    except BaseException:
        if process.returncode is None:
            process.kill()
        await process.wait()
        raise
    if diagnostics is not None:
        diagnostics.write_bytes((stdout + b"\n" + stderr)[-2_097_152:])
    if process.returncode:
        msg = f"Operational command failed with exit status {process.returncode}."
        raise RuntimeError(msg)
    return stdout + b"\n" + stderr if include_stderr else stdout


def run(
    args: Sequence[str],
    *,
    data: bytes | None = None,
    timeout: float = 60,
    cwd: Path | None = None,
    include_stderr: bool = False,
    diagnostics: Path | None = None,
) -> bytes:
    """Return private output in memory; never echo arguments, stdin or stderr."""
    try:
        return asyncio.run(
            _run(
                args,
                data,
                timeout,
                cwd,
                include_stderr=include_stderr,
                diagnostics=diagnostics,
            ),
        )
    except _COMMAND_ERRORS:
        msg = "Operational command could not run or exceeded its deadline."
        raise RuntimeError(msg) from None
