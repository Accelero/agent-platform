"""Refuse to publish a release whose exact commit has not passed its checks.

A release is built from a tag, so the commit it names must already have green
runs of the workflows that verify it: the ordinary checks, and the cluster
scenarios that install the chart on a disposable cluster. The decision is made
here from the workflow runs GitHub reports for that commit, so it can be tested
without GitHub; the release workflow only fetches the runs and calls this.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

# Workflow names, as `name:` in .github/workflows; a test keeps them in step.
REQUIRED = ("Checks", "Cluster scenarios")


def problems(runs: list[dict], sha: str, required=REQUIRED) -> list[str]:
    """Why `sha` may not be released, one line per workflow that has not passed.

    For each required workflow only its latest run for exactly this commit
    counts, so a later failure outweighs an earlier success and a run for any
    other commit is ignored. A run that is queued, running, cancelled, skipped
    or failed has not passed.
    """
    found = []
    for name in required:
        latest = max(
            (
                run
                for run in runs
                if run.get("name") == name and run.get("head_sha") == sha
            ),
            key=lambda run: run.get("created_at", ""),
            default=None,
        )
        if latest is None:
            found.append(f"{name}: no run for this commit")
        elif latest.get("status") != "completed":
            found.append(f"{name}: the latest run is {latest.get('status')}")
        elif latest.get("conclusion") != "success":
            found.append(f"{name}: the latest run {latest.get('conclusion')}")
    return found


def runs_of(document) -> list[dict]:
    """The workflow runs in GitHub's answer: one page, or the list of pages
    that `gh api --paginate --slurp` collects."""
    pages = document if isinstance(document, list) else [document]
    return [run for page in pages for run in page.get("workflow_runs", [])]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sha", required=True, help="the commit the tag names")
    parser.add_argument(
        "--runs",
        required=True,
        type=Path,
        help="the JSON of GitHub's workflow runs for that commit (a page or pages)",
    )
    arguments = parser.parse_args()
    found = problems(runs_of(json.loads(arguments.runs.read_text())), arguments.sha)
    for line in found:
        print(f"release gate: {line}", file=sys.stderr)
    if found:
        print(
            "release gate: the tagged commit has not passed; run the missing "
            "workflows on it, then tag again",
            file=sys.stderr,
        )
        return 1
    print("release gate: the tagged commit passed every required workflow")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
