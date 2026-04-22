"""A tiny Calculator class demonstrating public + private methods."""

from typing import List


class Adder:
    def add(self, a: int, b: int) -> int:
        return a + b


class Calculator:
    def __init__(self) -> None:
        self._adder = Adder()
        self._history: List[int] = []

    def add(self, a: int, b: int) -> int:
        result = self._adder.add(a, b)
        self._record(result)
        return result

    def _record(self, value: int) -> None:
        self._history.append(value)

    def history(self) -> List[int]:
        return list(self._history)

    def sum(self) -> int:
        return sum(self._history)
