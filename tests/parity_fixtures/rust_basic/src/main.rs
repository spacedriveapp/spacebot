use rust_basic::{Calculator, Summable};

fn main() {
    let mut calc = Calculator::new();
    calc.add(2, 3);
    calc.add(10, 20);
    println!("sum = {}", calc.sum());
    println!("history = {:?}", calc.history());
}
