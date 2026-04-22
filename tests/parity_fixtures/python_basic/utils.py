"""Formatting helpers."""

from typing import Iterable


def format_history(values: Iterable[int]) -> str:
    return "history = [" + ", ".join(str(v) for v in values) + "]"
