export interface Summable {
  sum(): number;
}

class Adder {
  add(a: number, b: number): number {
    return a + b;
  }
}

export class Calculator implements Summable {
  private adder = new Adder();
  private _history: number[] = [];

  public add(a: number, b: number): number {
    const result = this.adder.add(a, b);
    this.record(result);
    return result;
  }

  private record(value: number): void {
    this._history.push(value);
  }

  public history(): number[] {
    return [...this._history];
  }

  public sum(): number {
    return this._history.reduce((a, b) => a + b, 0);
  }
}
