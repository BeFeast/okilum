//! Standalone hosted audit of the production bounded parser; no alternate engine.
#![allow(dead_code)]
#[path = "../../crates/okilum-core/src/tex.rs"]
mod tex;

fn main() {
    let input =
        std::fs::read_to_string(std::env::args_os().nth(1).expect("formula input")).unwrap();
    for (i, formula) in input.split('\0').enumerate() {
        let (mode, formula) = formula.split_once('\t').expect("display/inline mode");
        let result = if mode == "D" {
            tex::parse_display(formula).map(|display| format!("{:?}", display.tag))
        } else {
            tex::linear(formula).map(|linear| linear.text)
        };
        match result {
            Ok(text) => println!("{i}\tOK\t{text:?}"),
            Err(error) => println!("{i}\tERR\t{:?}\t{formula:?}", error.0),
        }
    }
}
