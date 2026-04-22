//! Math helpers.

pub struct Adder;

impl Adder {
    pub fn new() -> Self {
        Adder
    }

    pub fn add(&self, a: i64, b: i64) -> i64 {
        a + b
    }
}

impl Default for Adder {
    fn default() -> Self {
        Self::new()
    }
}

pub fn multiply(a: i64, b: i64) -> i64 {
    a * b
}
