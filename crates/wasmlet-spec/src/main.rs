//! `wasmlet-spec`: run `.wast` scripts and print pass counts per file.
//!
//! ```text
//! wasmlet-spec [--verbose] [--no-fuse] [--expect FILE] [--write-expect FILE] PATH...
//! ```
//!
//! `PATH` may be a `.wast` file or a directory (all `*.wast` inside, sorted).
//! `--expect` compares against a committed baseline and exits non-zero on any
//! regression (fewer passes or more failures in any file).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use wasmlet::CompileOptions;
use wasmlet_spec::{run_file_with, FileReport};

struct Args {
    verbose: bool,
    no_fuse: bool,
    expect: Option<PathBuf>,
    write_expect: Option<PathBuf>,
    paths: Vec<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        verbose: false,
        no_fuse: false,
        expect: None,
        write_expect: None,
        paths: Vec::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "-v" | "--verbose" => args.verbose = true,
            "--no-fuse" => args.no_fuse = true,
            "--expect" => args.expect = Some(it.next().ok_or("--expect needs a file")?.into()),
            "--write-expect" => {
                args.write_expect = Some(it.next().ok_or("--write-expect needs a file")?.into())
            }
            "-h" | "--help" => {
                return Err(
                    "usage: wasmlet-spec [--verbose] [--no-fuse] [--expect FILE] [--write-expect FILE] PATH..."
                        .into(),
                )
            }
            _ => args.paths.push(a.into()),
        }
    }
    if args.paths.is_empty() {
        return Err("no .wast files given".into());
    }
    Ok(args)
}

fn collect(paths: &[PathBuf]) -> std::io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for p in paths {
        if p.is_dir() {
            let mut inner: Vec<PathBuf> = std::fs::read_dir(p)?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "wast"))
                .collect();
            inner.sort();
            files.extend(inner);
        } else {
            files.push(p.clone());
        }
    }
    Ok(files)
}

/// `file -> (passed, failed, skipped)`
type Baseline = BTreeMap<String, (usize, usize, usize)>;

fn read_baseline(path: &Path) -> Result<Baseline, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut map = Baseline::new();
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
    {
        let cols: Vec<&str> = line.split('\t').collect();
        let num = |i: usize| -> Result<usize, String> {
            cols.get(i)
                .and_then(|c| c.parse().ok())
                .ok_or_else(|| format!("bad baseline line: {line}"))
        };
        map.insert(cols[0].to_owned(), (num(1)?, num(2)?, num(3)?));
    }
    Ok(map)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let files = match collect(&args.paths) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let mut reports: Vec<FileReport> = Vec::new();
    println!("{:<28} {:>13} {:>12}", "file", "passed/run", "");
    for f in &files {
        let options = CompileOptions {
            superinstructions: !args.no_fuse,
        };
        let r = match run_file_with(f, options) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error reading {}: {e}", f.display());
                return ExitCode::from(2);
            }
        };
        println!("{r}");
        if args.verbose {
            for fail in &r.failures {
                println!("    line {:>5}: {}", fail.line, fail.message);
            }
            for (expected, actual) in &r.message_mismatches {
                println!("    message: expected `{expected}`, wasmlet said `{actual}`");
            }
        }
        reports.push(r);
    }
    let sum = |f: fn(&FileReport) -> usize| reports.iter().map(f).sum::<usize>();
    let (passed, failed, skipped) = (sum(|r| r.passed), sum(|r| r.failed), sum(|r| r.skipped));
    let (msg_ok, msg_n) = (sum(|r| r.messages_matched), sum(|r| r.messages_checked));
    let clean = reports
        .iter()
        .filter(|r| r.failed == 0 && r.parse_error.is_none())
        .count();
    println!("----");
    println!(
        "TOTAL: {passed}/{} directives passed ({:.2}%), {failed} failed, {skipped} skipped; {clean}/{} files fully passing",
        passed + failed,
        100.0 * passed as f64 / (passed + failed).max(1) as f64,
        reports.len()
    );
    println!(
        "rejection messages matching the reference interpreter: {msg_ok}/{msg_n} (informational)"
    );

    let mut reasons: BTreeMap<&str, usize> = BTreeMap::new();
    for r in &reports {
        for (k, v) in &r.skip_reasons {
            *reasons.entry(k).or_default() += v;
        }
    }
    for (k, v) in reasons {
        println!("skipped: {v:>5}  {k}");
    }

    if let Some(path) = &args.write_expect {
        let mut out = String::from(
            "# file\tpassed\tfailed\tskipped (generated by wasmlet-spec --write-expect)\n",
        );
        for r in &reports {
            out.push_str(&format!(
                "{}\t{}\t{}\t{}\n",
                r.name, r.passed, r.failed, r.skipped
            ));
        }
        if let Err(e) = std::fs::write(path, out) {
            eprintln!("error writing {}: {e}", path.display());
            return ExitCode::from(2);
        }
    }

    let mut ok = reports.iter().all(|r| r.parse_error.is_none());
    if let Some(path) = &args.expect {
        let baseline = match read_baseline(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(2);
            }
        };
        for r in &reports {
            match baseline.get(&r.name) {
                Some(&(p, f, _)) if r.passed < p || r.failed > f => {
                    eprintln!(
                        "REGRESSION {}: passed {} (baseline {p}), failed {} (baseline {f})",
                        r.name, r.passed, r.failed
                    );
                    ok = false;
                }
                None => {
                    eprintln!("{} is not in the baseline", r.name);
                    ok = false;
                }
                _ => {}
            }
        }
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
