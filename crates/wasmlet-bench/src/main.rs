//! `wasmlet-bench`: times wasmlet and wasmi on the same programs.
//!
//! ```text
//! wasmlet-bench [--runs N] [--quick]
//! ```
//!
//! For every workload wasmlet (with and without superinstructions) and wasmi
//! compile the same bytes, instantiate, and call the same export with the
//! same argument. Reported times are the median
//! of N runs (default 5) of the call alone; compile times are reported
//! separately. Results of the two engines are compared and the process exits
//! non-zero on any disagreement, so the benchmark doubles as a differential
//! test. wasmi runs with `CompilationMode::Eager` so both engines translate
//! everything up front.

use std::process::ExitCode;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
enum Arg {
    I32(i32),
    I64(i64),
}

struct Workload {
    name: &'static str,
    what: &'static str,
    wasm: Vec<u8>,
    export: &'static str,
    arg: Arg,
    quick_arg: Arg,
}

fn wat(src: &str) -> Vec<u8> {
    wat::parse_str(src).expect("fixture WAT is valid")
}

fn workloads() -> Vec<Workload> {
    let kernels = include_bytes!("../../../fixtures/wasm/kernels-rust.wasm").to_vec();
    vec![
        Workload {
            name: "fib-recursive",
            what: "recursive fib(n), hand-written WAT; call-heavy",
            wasm: wat(include_str!("../../../fixtures/wat/fib.wat")),
            export: "fib",
            arg: Arg::I32(27),
            quick_arg: Arg::I32(15),
        },
        Workload {
            name: "loop-arith",
            what: "sum of i*i mod 7 for i < n, WAT; branch + i64 ALU",
            wasm: wat(include_str!("../../../fixtures/wat/loop.wat")),
            export: "sum",
            arg: Arg::I64(5_000_000),
            quick_arg: Arg::I64(10_000),
        },
        Workload {
            name: "sieve",
            what: "count primes below n, WAT; byte loads/stores",
            wasm: wat(include_str!("../../../fixtures/wat/sieve.wat")),
            export: "sieve",
            arg: Arg::I32(4_000_000),
            quick_arg: Arg::I32(10_000),
        },
        Workload {
            name: "matmul-f64",
            what: "n x n f64 matrix multiply, WAT; f64 loads + calls",
            wasm: wat(include_str!("../../../fixtures/wat/matmul.wat")),
            export: "matmul",
            arg: Arg::I32(120),
            quick_arg: Arg::I32(8),
        },
        Workload {
            name: "sha256",
            what: "SHA-256 compression of n blocks, rustc -Os output",
            wasm: kernels.clone(),
            export: "sha256_blocks",
            arg: Arg::I32(20_000),
            quick_arg: Arg::I32(20),
        },
        Workload {
            name: "mandelbrot",
            what: "n x n grid, 64 iterations, rustc -Os output; f64",
            wasm: kernels,
            export: "mandelbrot",
            arg: Arg::I32(300),
            quick_arg: Arg::I32(20),
        },
    ]
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn run_wasmlet(
    w: &Workload,
    arg: Arg,
    runs: usize,
    superinstructions: bool,
) -> Result<(Duration, Duration, String), String> {
    use wasmlet::{CompileOptions, Linker, Module, Store, Value};
    let options = CompileOptions { superinstructions };
    let mut compile = Vec::new();
    let mut exec = Vec::new();
    let mut result = String::new();
    for _ in 0..runs {
        let t = Instant::now();
        let module = Module::with_options(&w.wasm, options).map_err(|e| e.to_string())?;
        compile.push(t.elapsed());
        let mut store = Store::default();
        let inst = Linker::new()
            .instantiate(&mut store, &module)
            .map_err(|e| e.to_string())?;
        let f = store
            .instance_func(inst, w.export)
            .map_err(|e| e.to_string())?;
        let a = match arg {
            Arg::I32(v) => Value::I32(v),
            Arg::I64(v) => Value::I64(v),
        };
        let t = Instant::now();
        let out = store.invoke(f, &[a]).map_err(|e| e.to_string())?;
        exec.push(t.elapsed());
        result = match out.first() {
            Some(Value::I32(v)) => v.to_string(),
            Some(Value::I64(v)) => v.to_string(),
            Some(Value::F64(v)) => v.to_string(),
            other => format!("{other:?}"),
        };
    }
    Ok((median(compile), median(exec), result))
}

fn run_wasmi(w: &Workload, arg: Arg, runs: usize) -> Result<(Duration, Duration, String), String> {
    use wasmi::{CompilationMode, Config, Engine, Linker, Module, Store, Val};
    let mut config = Config::default();
    config.compilation_mode(CompilationMode::Eager);
    let engine = Engine::new(&config);
    let mut compile = Vec::new();
    let mut exec = Vec::new();
    let mut result = String::new();
    for _ in 0..runs {
        let t = Instant::now();
        let module = Module::new(&engine, &w.wasm).map_err(|e| e.to_string())?;
        compile.push(t.elapsed());
        let mut store = Store::new(&engine, ());
        let inst = Linker::<()>::new(&engine)
            .instantiate_and_start(&mut store, &module)
            .map_err(|e| e.to_string())?;
        let f = inst.get_func(&store, w.export).ok_or("missing export")?;
        let a = match arg {
            Arg::I32(v) => Val::I32(v),
            Arg::I64(v) => Val::I64(v),
        };
        let mut out = [Val::I32(0)];
        if let Some(t) = f.ty(&store).results().first() {
            out[0] = Val::default_for_ty(*t);
        }
        let t = Instant::now();
        f.call(&mut store, &[a], &mut out)
            .map_err(|e| e.to_string())?;
        exec.push(t.elapsed());
        result = match &out[0] {
            Val::I32(v) => v.to_string(),
            Val::I64(v) => v.to_string(),
            Val::F64(v) => v.to_float().to_string(),
            other => format!("{other:?}"),
        };
    }
    Ok((median(compile), median(exec), result))
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn main() -> ExitCode {
    let mut runs = 5;
    let mut quick = false;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--runs" => runs = it.next().and_then(|n| n.parse().ok()).unwrap_or(5),
            "--quick" => quick = true,
            _ => {
                eprintln!("usage: wasmlet-bench [--runs N] [--quick]");
                return ExitCode::from(2);
            }
        }
    }
    println!(
        "runs per workload: {runs} (median reported); build profile: {}",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    println!();
    println!("| workload | what | n | result | wasmlet ms | wasmlet no-fusion ms | wasmi ms | wasmlet / wasmi | fusion speedup | wasmlet compile ms | wasmi compile ms |");
    println!("|---|---|---:|---|---:|---:|---:|---:|---:|---:|---:|");
    let mut ok = true;
    let (mut ratios, mut speedups) = (Vec::new(), Vec::new());
    for w in workloads() {
        let arg = if quick { w.quick_arg } else { w.arg };
        let fused = run_wasmlet(&w, arg, runs, true);
        let plain = run_wasmlet(&w, arg, runs, false);
        let theirs = run_wasmi(&w, arg, runs);
        match (fused, plain, theirs) {
            (Ok((ca, ea, ra)), Ok((_, ep, rp)), Ok((cb, eb, rb))) => {
                if ra != rb || rp != rb {
                    ok = false;
                    eprintln!(
                        "MISMATCH in {}: wasmlet={ra} no-fusion={rp} wasmi={rb}",
                        w.name
                    );
                }
                let ratio = ea.as_secs_f64() / eb.as_secs_f64().max(1e-9);
                let speedup = ep.as_secs_f64() / ea.as_secs_f64().max(1e-9);
                ratios.push(ratio);
                speedups.push(speedup);
                println!(
                    "| {} | {} | {} | {} | {:.2} | {:.2} | {:.2} | {:.2}x | {:.2}x | {:.3} | {:.3} |",
                    w.name,
                    w.what,
                    match arg {
                        Arg::I32(v) => v.to_string(),
                        Arg::I64(v) => v.to_string(),
                    },
                    ra,
                    ms(ea),
                    ms(ep),
                    ms(eb),
                    ratio,
                    speedup,
                    ms(ca),
                    ms(cb)
                );
            }
            (a, p, b) => {
                ok = false;
                eprintln!(
                    "{}: wasmlet {:?} / no-fusion {:?} / wasmi {:?}",
                    w.name,
                    a.err(),
                    p.err(),
                    b.err()
                );
            }
        }
    }
    let geo = |v: &[f64]| (v.iter().map(|r| r.ln()).sum::<f64>() / v.len().max(1) as f64).exp();
    if !ratios.is_empty() {
        println!();
        println!(
            "geometric mean wasmlet/wasmi execution time: {:.2}x (lower is better for wasmlet)",
            geo(&ratios)
        );
        println!(
            "geometric mean speedup from superinstructions: {:.2}x",
            geo(&speedups)
        );
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
