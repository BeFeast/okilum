//! Standalone hosted audit of the production bounded parser; no alternate engine.
#![allow(dead_code)]
#[path = "../../crates/okilum-core/src/tex.rs"]
mod tex;

fn main() {
    let input = std::fs::read_to_string(std::env::args_os().nth(1).expect("formula input")).unwrap();
    for (i, formula) in input.split('\0').enumerate() {
        match tex::linear(formula) {
            Ok(linear) => println!("{i}\tOK\t{:?}", linear.text),
            Err(error) => println!("{i}\tERR\t{:?}\t{formula:?}", error.0),
        }
    }
}
