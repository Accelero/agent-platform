"""Require explicit proving levels for collected Python tests."""

import os

import pytest

from tests.support.verification import case_level

pytest_plugins = ["pytester"]


def pytest_collection_modifyitems(items):
    for item in items:
        try:
            case_level(item)
        except ValueError as error:
            raise pytest.UsageError(f"{item.nodeid}: {error}") from None

    # Scenario cases must not depend on each other: run them back to front on
    # request to prove it.
    if os.environ.get("HERMES_TEST_ORDER") == "reverse":
        items.reverse()
