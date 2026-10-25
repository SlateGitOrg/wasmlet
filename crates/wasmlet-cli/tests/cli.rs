//! End-to-end tests of the `wasmlet` binary.

use std::process::{Command, Output};

fn root(rel: &str) -> String {
    format!("{}/../../{rel}", env!("CARGO_MANIFEST_DIR"))
}

fn wasmlet(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wasmlet"))
        .args(args)
        .output()
        .expect("run wasmlet")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn runs_rust_hello_with_args_and_env() {
    let out = wasmlet(&[
        "run",
        "--env",
        "NAME=cli",
        &root("fixtures/wasm/hello-rust.wasm"),
        "x",
    ]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(stdout.contains("NAME = cli"), "{stdout}");
    assert!(stdout.contains(r#""x"]"#), "{stdout}");
    assert!(text(&out.stderr).contains("this line goes to stderr"));
}

#[test]
fn propagates_guest_exit_codes() {
    let out = wasmlet(&["run", &root("fixtures/wasm/hello-rust.wasm"), "--fail"]);
    assert_eq!(out.status.code(), Some(3));
}

#[test]
fn feeds_stdin_from_a_file() {
    let out = wasmlet(&[
        "run",
        "--deterministic",
        "--stats",
        "--stdin",
        &root("LICENSE"),
        &root("fixtures/wasm/wc-rust.wasm"),
    ]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains(" lines, "),
        "{}",
        text(&out.stdout)
    );
    assert!(text(&out.stderr).contains("[stats] decode+validate+compile"));
}

#[test]
fn invokes_exports_with_typed_arguments() {
    let out = wasmlet(&[
        "run",
        "--invoke",
        "fib",
        &root("fixtures/wat/fib.wat"),
        "20",
    ]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout).trim(), "6765:i32");
    let out = wasmlet(&[
        "run",
        "--invoke",
        "sum",
        &root("fixtures/wat/loop.wat"),
        "0x10",
    ]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let out = wasmlet(&["run", "--invoke", "fib", &root("fixtures/wat/fib.wat")]);
    assert!(text(&out.stderr).contains("expected 1 arguments, got 0"));
    let out = wasmlet(&[
        "run",
        "--invoke",
        "fib",
        &root("fixtures/wat/fib.wat"),
        "abc",
    ]);
    assert!(text(&out.stderr).contains("cannot parse `abc` as i32"));
    let out = wasmlet(&[
        "run",
        "--invoke",
        "fib",
        &root("fixtures/wat/fib.wat"),
        "99999999999",
    ]);
    assert!(!out.status.success());
}

#[test]
fn fuel_stops_runaway_programs() {
    let out = wasmlet(&[
        "run",
        "--fuel",
        "1000",
        "--stats",
        "--invoke",
        "fib",
        &root("fixtures/wat/fib.wat"),
        "30",
    ]);
    assert!(!out.status.success());
    let err = text(&out.stderr);
    assert!(err.contains("all fuel consumed"), "{err}");
    assert!(err.contains("fuel used 1000 of 1000"), "{err}");
}

#[test]
fn validate_and_inspect() {
    let out = wasmlet(&["validate", &root("fixtures/wasm/kernels-rust.wasm")]);
    assert!(out.status.success());
    assert!(text(&out.stdout).starts_with("valid: "));
    let out = wasmlet(&["inspect", "--ir", &root("fixtures/wat/hello.wat")]);
    let s = text(&out.stdout);
    assert!(
        s.contains("wasi_snapshot_preview1.fd_write: func [i32 i32 i32 i32] -> [i32]"),
        "{s}"
    );
    assert!(s.contains("memory: memory"), "{s}");
    assert!(s.contains("Store32"), "{s}");
    let out = wasmlet(&["inspect", &root("fixtures/wat/fib.wat")]);
    assert!(
        text(&out.stdout).contains("custom section"),
        "{}",
        text(&out.stdout)
    );
}

#[test]
fn reports_errors_and_usage() {
    let out = wasmlet(&[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("USAGE"));
    assert!(wasmlet(&["--help"]).status.success());
    let out = wasmlet(&["validate", &root("Cargo.toml")]);
    assert!(
        text(&out.stderr).contains("malformed module"),
        "{}",
        text(&out.stderr)
    );
    let out = wasmlet(&["run", "/nonexistent.wasm"]);
    assert!(!out.status.success());
    let out = wasmlet(&["run"]);
    assert!(text(&out.stderr).contains("missing FILE"));
    let out = wasmlet(&["run", "--env", "NOEQUALS", &root("fixtures/wat/hello.wat")]);
    assert!(text(&out.stderr).contains("KEY=VALUE"));
    let out = wasmlet(&["run", "--fuel"]);
    assert!(text(&out.stderr).contains("needs a value"));
    let out = wasmlet(&["inspect"]);
    assert!(text(&out.stderr).contains("missing FILE"));
    let out = wasmlet(&["run", "-v", &root("fixtures/wat/fib.wat")]);
    assert!(
        text(&out.stderr).contains("_start"),
        "{}",
        text(&out.stderr)
    );
}
