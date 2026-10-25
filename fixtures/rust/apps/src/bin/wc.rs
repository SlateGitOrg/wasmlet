//! `wc` clone: reads all of stdin and reports lines, words, bytes and the
//! most frequent words.
use std::collections::HashMap;
use std::io::Read;

fn main() {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).expect("stdin is UTF-8");
    let lines = input.lines().count();
    let words: Vec<String> = input
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect();
    let mut freq: HashMap<&str, usize> = HashMap::new();
    for w in &words {
        *freq.entry(w.as_str()).or_insert(0) += 1;
    }
    let mut top: Vec<(&str, usize)> = freq.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    println!("{lines} lines, {} words, {} bytes", words.len(), input.len());
    for (w, n) in top.iter().take(5) {
        println!("{n:>5}  {w}");
    }
}
