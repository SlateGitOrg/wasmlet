//! `wasmlet`: run, validate and inspect WebAssembly modules.

use std::cell::RefCell;
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::time::Instant;

use wasmlet::types::ValType;
use wasmlet::wasi::{self, Output, WasiCtx};
use wasmlet::{Config, Error, ExternType, Linker, Module, Store, Value};

const USAGE: &str = "\
wasmlet — a WebAssembly 2.0 interpreter

USAGE:
    wasmlet run [OPTIONS] <FILE> [ARGS...]
    wasmlet validate <FILE>
    wasmlet inspect [--ir] <FILE>

FILE is a binary .wasm module or a .wat text module.

RUN OPTIONS:
    --invoke <NAME>     call export NAME with ARGS parsed per its parameter
                        types, instead of running WASI `_start`
    --env <KEY=VALUE>   set a WASI environment variable (repeatable)
    --stdin <PATH>      file whose bytes the program reads from fd 0
    --fuel <N>          stop with a trap after N branches/calls
    --deterministic     fixed clock and RNG for reproducible WASI runs
    --stats             print phase timings to stderr
    -v, --verbose       debug logging to stderr";

struct Logger;

static LOGGER: Logger = Logger;

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata<'_>) -> bool {
        m.level() <= log::max_level()
    }
    fn log(&self, r: &log::Record<'_>) {
        if self.enabled(r.metadata()) {
            eprintln!("[{}] {}", r.level(), r.args());
        }
    }
    fn flush(&self) {}
}

#[derive(Debug, Default)]
struct RunOpts {
    invoke: Option<String>,
    env: Vec<(String, String)>,
    stdin: Option<PathBuf>,
    fuel: Option<u64>,
    deterministic: bool,
    stats: bool,
    file: Option<PathBuf>,
    args: Vec<String>,
}

fn load(path: &PathBuf) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if path.extension().is_some_and(|e| e == "wat") {
        return wat::parse_bytes(&bytes)
            .map(|b| b.into_owned())
            .map_err(|e| format!("{}: {e}", path.display()));
    }
    Ok(bytes)
}

fn parse_value(text: &str, ty: ValType) -> Result<Value, String> {
    let bad = || format!("cannot parse `{text}` as {ty}");
    let int = |t: &str| -> Option<i128> {
        let (neg, t) = t.strip_prefix('-').map_or((false, t), |r| (true, r));
        let v = match t.strip_prefix("0x") {
            Some(h) => i128::from_str_radix(h, 16).ok()?,
            None => t.parse::<i128>().ok()?,
        };
        Some(if neg { -v } else { v })
    };
    Ok(match ty {
        ValType::I32 => {
            let v = int(text).ok_or_else(bad)?;
            if v < i128::from(i32::MIN) || v > i128::from(u32::MAX) {
                return Err(bad());
            }
            Value::I32(v as i32)
        }
        ValType::I64 => {
            let v = int(text).ok_or_else(bad)?;
            if v < i128::from(i64::MIN) || v > i128::from(u64::MAX) {
                return Err(bad());
            }
            Value::I64(v as i64)
        }
        ValType::F32 => Value::F32(text.parse().map_err(|_| bad())?),
        ValType::F64 => Value::F64(text.parse().map_err(|_| bad())?),
        ValType::Ref(_) => return Err(format!("{ty} arguments are not supported on the command line")),
    })
}

fn run(opts: RunOpts) -> Result<ExitCode, String> {
    let file = opts.file.ok_or("run: missing FILE")?;
    let bytes = load(&file)?;
    let t0 = Instant::now();
    let module = Module::new(&bytes).map_err(|e| e.to_string())?;
    let t_compile = t0.elapsed();
    let mut store = Store::new(Config {
        fuel: opts.fuel,
        ..Config::default()
    });
    let report = |store: &Store, t_run: std::time::Duration| {
        if opts.stats {
            eprintln!(
                "[stats] decode+validate+compile {:.3} ms, run {:.3} ms",
                t_compile.as_secs_f64() * 1e3,
                t_run.as_secs_f64() * 1e3
            );
            if let (Some(budget), Some(left)) = (opts.fuel, store.fuel()) {
                eprintln!("[stats] fuel used {} of {budget}", budget - left);
            }
        }
    };

    if let Some(name) = &opts.invoke {
        // Plain invocation; WASI imports are still available if the module wants them.
        let mut linker = Linker::new();
        let ctx = Rc::new(RefCell::new(WasiCtx::new().output(Output::Inherit)));
        wasi::add_to_linker(&mut linker, &mut store, ctx);
        let instance = linker.instantiate(&mut store, &module).map_err(|e| e.to_string())?;
        let func = store.instance_func(instance, name).map_err(|e| e.to_string())?;
        let ty = store.func_type(func).clone();
        if ty.params().len() != opts.args.len() {
            return Err(format!(
                "`{name}` has type {ty}: expected {} arguments, got {}",
                ty.params().len(),
                opts.args.len()
            ));
        }
        let args = opts
            .args
            .iter()
            .zip(ty.params())
            .map(|(a, &t)| parse_value(a, t))
            .collect::<Result<Vec<_>, _>>()?;
        let t1 = Instant::now();
        let result = store.invoke(func, &args);
        report(&store, t1.elapsed());
        let results = result.map_err(|e| e.to_string())?;
        for v in results {
            println!("{v}");
        }
        return Ok(ExitCode::SUCCESS);
    }

    let mut ctx = WasiCtx::new()
        .output(Output::Inherit)
        .deterministic(opts.deterministic)
        .args(std::iter::once(file.display().to_string()).chain(opts.args));
    for (k, v) in opts.env {
        ctx = ctx.env(k, v);
    }
    if let Some(p) = &opts.stdin {
        ctx = ctx.stdin(std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?);
    }
    let t1 = Instant::now();
    let status = wasi::run_command(&mut store, &module, Rc::new(RefCell::new(ctx)));
    report(&store, t1.elapsed());
    match status {
        Ok(s) => Ok(ExitCode::from(s.code().clamp(0, 255) as u8)),
        Err(e @ Error::Trap(_)) => Err(format!("{e}")),
        Err(e) => Err(e.to_string()),
    }
}

fn validate(file: &PathBuf) -> Result<ExitCode, String> {
    let bytes = load(file)?;
    let module = Module::new(&bytes).map_err(|e| e.to_string())?;
    let ops: usize = module.compiled_funcs().map(|f| f.code.len()).sum();
    println!(
        "valid: {} functions ({} imported), {} IR ops",
        module.compiled_funcs().count() + module.imported_funcs(),
        module.imported_funcs(),
        ops
    );
    Ok(ExitCode::SUCCESS)
}

fn describe(t: &ExternType) -> String {
    match t {
        ExternType::Func(f) => format!("func {f}"),
        ExternType::Table(t) => format!("table {} {:?}", t.elem, t.limits),
        ExternType::Memory(m) => format!("memory {:?}", m.limits),
        ExternType::Global(g) => format!("global {:?} {}", g.mutability, g.content),
    }
}

fn inspect(file: &PathBuf, ir: bool) -> Result<ExitCode, String> {
    let bytes = load(file)?;
    let module = Module::new(&bytes).map_err(|e| e.to_string())?;
    println!("{}: {} bytes", file.display(), bytes.len());
    println!("imports:");
    for (m, n, t) in module.imports() {
        println!("  {m}.{n}: {}", describe(&t));
    }
    println!("exports:");
    for (n, t) in module.exports() {
        println!("  {n}: {}", describe(&t));
    }
    if let Some(s) = module.start() {
        println!("start: func {s}");
    }
    for (name, len) in module.custom_sections() {
        println!("custom section `{name}`: {len} bytes");
    }
    let base = module.imported_funcs();
    for (i, f) in module.compiled_funcs().enumerate() {
        let ty = module
            .func_type(f.type_idx)
            .map(|t| t.to_string())
            .unwrap_or_default();
        println!("func {} {ty}: {} ops, max stack {}", base + i, f.code.len(), f.max_stack);
        if ir {
            print!("{f}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let verbose = args.iter().any(|a| a == "-v" || a == "--verbose");
    args.retain(|a| a != "-v" && a != "--verbose");
    let level = if verbose {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Warn
    };
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(level);

    let result = match args.first().map(String::as_str) {
        Some("run") => parse_run(&args[1..]).and_then(run),
        Some("validate") if args.len() == 2 => validate(&PathBuf::from(&args[1])),
        Some("inspect") => {
            let ir = args.iter().any(|a| a == "--ir");
            match args[1..].iter().find(|a| *a != "--ir") {
                Some(f) => inspect(&PathBuf::from(f), ir),
                None => Err("inspect: missing FILE".into()),
            }
        }
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn parse_run(args: &[String]) -> Result<RunOpts, String> {
    let mut o = RunOpts::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if o.file.is_some() {
            // Everything after FILE belongs to the guest.
            o.args.push(a.clone());
            continue;
        }
        let mut value = |flag: &str| it.next().cloned().ok_or(format!("{flag} needs a value"));
        match a.as_str() {
            "--invoke" => o.invoke = Some(value("--invoke")?),
            "--env" => {
                let kv = value("--env")?;
                let (k, v) = kv.split_once('=').ok_or("--env expects KEY=VALUE")?;
                o.env.push((k.to_owned(), v.to_owned()));
            }
            "--stdin" => o.stdin = Some(value("--stdin")?.into()),
            "--fuel" => o.fuel = Some(value("--fuel")?.parse().map_err(|_| "--fuel expects a number")?),
            "--deterministic" => o.deterministic = true,
            "--stats" => o.stats = true,
            "--" => {}
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            file => o.file = Some(file.into()),
        }
    }
    Ok(o)
}
