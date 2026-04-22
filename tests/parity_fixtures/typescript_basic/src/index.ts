import { Calculator } from "./calculator";
import { formatHistory } from "./utils";

export function run(): void {
  const calc = new Calculator();
  calc.add(2, 3);
  calc.add(10, 20);
  console.log(`sum = ${calc.sum()}`);
  console.log(formatHistory(calc.history()));
}

run();
