"""Require explicit proving levels for collected Python tests."""

import pytest

from tests.support.verification import case_level

pytest_plugins = ["pytester"]


def pytest_collection_modifyitems(items):
    for item in items:
        try:
            case_level(item)
        except ValueError as error:
            raise pytest.UsageError(f"{item.nodeid}: {error}") from None
