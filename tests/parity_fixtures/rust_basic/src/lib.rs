//! Parity fixture: a small Rust library with a handful of structs,
//! traits, impls, and cross-module calls. Exercises the Rust provider
//! plus the imports/calls/heritage phases.

pub mod math;

use math::Adder;

/// A public struct with a private helper and a public method.
pub struct Calculator {
    adder: Adder,
    history: Vec<i64>,
}

impl Calculator {
    pub fn new() -> Self {
        Self {
            adder: Adder::new(),
            history: Vec::new(),
        }
    }

    pub fn add(&mut self, a: i64, b: i64) -> i64 {
        let result = self.adder.add(a, b);
        self.record(result);
        result
    }

    fn record(&mut self, value: i64) {
        self.history.push(value);
    }

    pub fn history(&self) -> &[i64] {
        &self.history
    }
}

impl Default for Calculator {
    fn default() -> Self {
        Self::new()
    }
}

pub trait Summable {
    fn sum(&self) -> i64;
}

impl Summable for Calculator {
    fn sum(&self) -> i64 {
        self.history.iter().sum()
    }
}
