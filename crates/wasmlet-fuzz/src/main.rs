//! `wasmlet-fuzz`: run the deterministic fuzz campaigns from the command line.
//!
//! ```text
//! wasmlet-fuzz [--seed S] [--decode N] [--diff N] [--artifacts DIR] [--root DIR]
//! wasmlet-fuzz --replay FILE...
//! ```
//!
//! `--replay` re-runs saved inputs through both checks (verdict agreement and,
//! for valid modules, differential execution) and prints what happened.
//!
//! Exits non-zero if any finding was recorded; each finding's input is saved
//! to `DIR/<kind>-<n>.wasm` (default `fuzz-artifacts/`).

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use wasmlet_fuzz::{
    check_input, decode_campaign, diff, differential_campaign, load_corpus, on_big_stack,
    reference_valid, Stats,
};

struct Args {
    seed: u64,
    decode: u64,
    diff: u64,
    artifacts: PathBuf,
    root: PathBuf,
    replay: Vec<PathBuf>,
}

fn parse() -> Result<Args, String> {
    let mut a = Args {
        seed: 1,
        decode: 20_000,
        diff: 2_000,
        artifacts: "fuzz-artifacts".into(),
        root: ".".into(),
        replay: Vec::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().ok_or(format!("{flag} needs a value"));
        let num = |s: String| s.parse::<u64>().map_err(|e| format!("bad number {s}: {e}"));
        match flag.as_str() {
            "--seed" => a.seed = num(val()?)?,
            "--decode" => a.decode = num(val()?)?,
            "--diff" => a.diff = num(val()?)?,
            "--artifacts" => a.artifacts = val()?.into(),
            "--root" => a.root = val()?.into(),
            "--replay" => {
                a.replay.push(val()?.into());
                a.replay.extend(it.by_ref().map(PathBuf::from));
            }
            _ => {
                return Err(
                    "usage: wasmlet-fuzz [--seed S] [--decode N] [--diff N] [--artifacts DIR] [--root DIR] | --replay FILE..."
                        .into(),
                )
            }
        }
    }
    Ok(a)
}

fn report(name: &str, s: &Stats, secs: f64) {
    println!(
        "{name:<12} inputs={:<7} accepted={:<7} rejected={:<7} calls={:<7} compared={:<6} inconclusive={:<5} findings={}  ({secs:.1}s)",
        s.inputs, s.accepted, s.rejected, s.calls, s.compared, s.inconclusive, s.findings.len()
    );
    for f in s.findings.iter().take(10) {
        println!("  [{}] {}", f.kind, f.detail);
    }
}

fn save(args: &Args, s: &Stats) -> std::io::Result<()> {
    if s.findings.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(&args.artifacts)?;
    for (i, f) in s.findings.iter().enumerate() {
        let p = args.artifacts.join(format!("{}-{i}.wasm", f.kind));
        std::fs::write(&p, &f.input)?;
        println!("  saved {}", p.display());
    }
    Ok(())
}

fn replay(files: &[PathBuf]) -> ExitCode {
    let mut stats = Stats::default();
    for f in files {
        let bytes = match std::fs::read(f) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("{}: {e}", f.display());
                return ExitCode::from(2);
            }
        };
        let before = stats.findings.len();
        check_input(&bytes, &mut stats);
        if stats.findings.len() == before && reference_valid(&bytes).is_ok() {
            on_big_stack(|| diff::compare(&bytes, &mut stats));
        }
        let new = &stats.findings[before..];
        println!(
            "{}: {}",
            f.display(),
            if new.is_empty() { "ok" } else { "FINDING" }
        );
        for x in new {
            println!("  [{}] {}", x.kind, x.detail);
        }
    }
    if stats.findings.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn main() -> ExitCode {
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    // Panics are caught and reported as findings; keep stderr quiet.
    std::panic::set_hook(Box::new(|_| {}));
    if !args.replay.is_empty() {
        return replay(&args.replay);
    }
    let t = Instant::now();
    let corpus = load_corpus(&args.root);
    println!("seed={} corpus={} modules", args.seed, corpus.len());
    let decode = decode_campaign(args.seed, args.decode, &corpus);
    report("decode", &decode, t.elapsed().as_secs_f64());
    let t = Instant::now();
    let diff = differential_campaign(args.seed, args.diff);
    report("differential", &diff, t.elapsed().as_secs_f64());
    let mut all = decode;
    all.findings.extend(diff.findings);
    if let Err(e) = save(&args, &all) {
        eprintln!("could not save artifacts: {e}");
    }
    if all.findings.is_empty() {
        println!("no findings");
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
