//! End-to-end tests of the `wasmlet-spec` binary.

use std::process::{Command, Output};

fn spec(rel: &str) -> String {
    format!("{}/../../tests/spec/{rel}", env!("CARGO_MANIFEST_DIR"))
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wasmlet-spec"))
        .args(args)
        .output()
        .expect("run wasmlet-spec")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn reports_per_file_counts_and_checks_the_baseline() {
    let out = run(&[
        "--expect",
        &spec("expected.tsv"),
        &spec("core/fac.wast"),
        &spec("core/br_table.wast"),
    ]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let s = text(&out.stdout);
    assert!(s.contains("fac.wast"), "{s}");
    assert!(s.contains("TOTAL: 182/182 directives passed"), "{s}");
    assert!(s.contains("files fully passing"), "{s}");
}

#[test]
fn writes_a_baseline_and_detects_regressions() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let base = dir.join("wasmlet-spec-baseline.tsv");
    let out = run(&[
        "--verbose",
        "--write-expect",
        base.to_str().unwrap(),
        &spec("core/obsolete-keywords.wast"),
        &spec("core/fac.wast"),
    ]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("skipped:"),
        "{}",
        text(&out.stdout)
    );
    let written = std::fs::read_to_string(&base).unwrap();
    assert!(written.contains("fac.wast\t8\t0\t0"), "{written}");

    // A baseline demanding more passes than achieved is a regression.
    let inflated = dir.join("wasmlet-spec-inflated.tsv");
    std::fs::write(&inflated, "fac.wast\t9\t0\t0\n").unwrap();
    let out = run(&[
        "--expect",
        inflated.to_str().unwrap(),
        &spec("core/fac.wast"),
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("REGRESSION fac.wast"),
        "{}",
        text(&out.stderr)
    );
    // A file missing from the baseline also fails.
    let out = run(&[
        "--expect",
        inflated.to_str().unwrap(),
        &spec("core/nop.wast"),
    ]);
    assert!(text(&out.stderr).contains("not in the baseline"));
}

#[test]
fn directories_are_expanded_and_bad_usage_is_rejected() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("wasmlet-spec-dir");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(spec("core/nop.wast"), dir.join("nop.wast")).unwrap();
    std::fs::write(dir.join("ignored.txt"), "x").unwrap();
    let out = run(&[dir.to_str().unwrap()]);
    assert!(out.status.success());
    assert!(text(&out.stdout).contains("nop.wast"));
    assert!(!text(&out.stdout).contains("ignored"));

    assert_eq!(run(&[]).status.code(), Some(2));
    assert_eq!(run(&["--help"]).status.code(), Some(2));
    assert_eq!(run(&["--expect"]).status.code(), Some(2));
    assert_eq!(run(&["/nonexistent/x.wast"]).status.code(), Some(2));
    let bad = dir.join("bad.tsv");
    std::fs::write(&bad, "nop.wast\tx\n").unwrap();
    assert_eq!(
        run(&["--expect", bad.to_str().unwrap(), &spec("core/nop.wast")])
            .status
            .code(),
        Some(2)
    );
}
