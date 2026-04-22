"""Entry point for the python_basic parity fixture."""

from calculator import Calculator
from utils import format_history


def run() -> None:
    calc = Calculator()
    calc.add(2, 3)
    calc.add(10, 20)
    print(f"sum = {calc.sum()}")
    print(format_history(calc.history()))


if __name__ == "__main__":
    run()
