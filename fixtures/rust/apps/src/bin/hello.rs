//! WASI hello world exercising std: formatting, args, env, collections, exit codes.
use std::collections::BTreeMap;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    println!("Hello from Rust compiled to wasm32-wasip1!");
    println!("argv = {args:?}");
    let name = std::env::var("NAME").unwrap_or_else(|_| "stranger".to_string());
    println!("NAME = {name}");
    let text = "the quick brown fox jumps over the lazy dog and the end";
    let mut freq: BTreeMap<&str, u32> = BTreeMap::new();
    for w in text.split_whitespace() {
        *freq.entry(w).or_insert(0) += 1;
    }
    println!("word counts: {freq:?}");
    let squares: Vec<u64> = (1..=10).map(|x: u64| x * x).collect();
    println!("squares: {squares:?} (sum {})", squares.iter().sum::<u64>());
    eprintln!("this line goes to stderr");
    if args.iter().any(|a| a == "--fail") {
        std::process::exit(3);
    }
}
