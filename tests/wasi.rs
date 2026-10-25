//! WASI preview1 subset: real `rustc` guests plus hand-written probes of every
//! implemented call and its error paths.

use std::cell::RefCell;
use std::rc::Rc;

use wasmlet::wasi::{self, errno, ExitStatus, WasiCtx};
use wasmlet::{Error, Linker, Module, Store, Trap, Value};

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    if name.ends_with(".wat") {
        wat::parse_bytes(&bytes).unwrap().into_owned()
    } else {
        bytes
    }
}

fn run(bytes: &[u8], ctx: WasiCtx) -> (wasmlet::Result<ExitStatus>, Rc<RefCell<WasiCtx>>) {
    let module = Module::new(bytes).expect("fixture validates");
    let ctx = Rc::new(RefCell::new(ctx));
    let mut store = Store::default();
    let status = wasi::run_command(&mut store, &module, Rc::clone(&ctx));
    (status, ctx)
}

fn stdout(ctx: &Rc<RefCell<WasiCtx>>) -> String {
    String::from_utf8(ctx.borrow().stdout().to_vec()).unwrap()
}

#[test]
fn rust_hello_world_uses_args_env_and_stdio() {
    let ctx = WasiCtx::new()
        .args(["hello.wasm", "a", "b"])
        .env("NAME", "wasmlet");
    let (status, ctx) = run(&fixture("wasm/hello-rust.wasm"), ctx);
    assert_eq!(status.unwrap(), ExitStatus::Returned);
    let out = stdout(&ctx);
    assert!(
        out.starts_with("Hello from Rust compiled to wasm32-wasip1!\n"),
        "{out}"
    );
    assert!(out.contains(r#"argv = ["hello.wasm", "a", "b"]"#), "{out}");
    assert!(out.contains("NAME = wasmlet"), "{out}");
    assert!(out.contains(r#""the": 3"#), "{out}");
    assert!(out.contains("(sum 385)"), "{out}");
    assert_eq!(ctx.borrow().stderr(), b"this line goes to stderr\n");
}

#[test]
fn rust_exit_codes_come_back_as_exit_status() {
    let ctx = WasiCtx::new().args(["hello.wasm", "--fail"]);
    let (status, ctx) = run(&fixture("wasm/hello-rust.wasm"), ctx);
    let status = status.unwrap();
    assert_eq!(status, ExitStatus::Exited(3));
    assert_eq!(status.code(), 3);
    assert!(stdout(&ctx).contains("NAME = stranger"));
    assert_eq!(ExitStatus::Returned.code(), 0);
}

#[test]
fn rust_wc_reads_stdin() {
    let text = "one two three\nTwo three\nthree!\n";
    let (status, ctx) = run(&fixture("wasm/wc-rust.wasm"), WasiCtx::new().stdin(text));
    assert_eq!(status.unwrap(), ExitStatus::Returned);
    let out = stdout(&ctx);
    assert!(out.starts_with("3 lines, 6 words, 31 bytes\n"), "{out}");
    assert!(
        out.contains("    3  three\n    2  two\n    1  one\n"),
        "{out}"
    );
}

#[test]
fn hand_written_hello() {
    let (status, ctx) = run(&fixture("wat/hello.wat"), WasiCtx::default());
    assert_eq!(status.unwrap(), ExitStatus::Returned);
    assert_eq!(stdout(&ctx), "hello from hand-written wasm\n");
}

/// A probe module importing every implemented call; each export runs one
/// call with pointers into memory and returns the errno (and the probe
/// reads back the results from memory).
const PROBE: &str = r#"(module
  (import "wasi_snapshot_preview1" "args_sizes_get" (func $args_sizes_get (param i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "args_get" (func $args_get (param i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "environ_sizes_get" (func $environ_sizes_get (param i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "environ_get" (func $environ_get (param i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_read" (func $fd_read (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_close" (func $fd_close (param i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_seek" (func $fd_seek (param i32 i64 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_fdstat_get" (func $fd_fdstat_get (param i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_prestat_get" (func $fd_prestat_get (param i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_prestat_dir_name" (func $fd_prestat_dir_name (param i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "clock_time_get" (func $clock_time_get (param i32 i64 i32) (result i32)))
  (import "wasi_snapshot_preview1" "clock_res_get" (func $clock_res_get (param i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "random_get" (func $random_get (param i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "sched_yield" (func $sched_yield (result i32)))
  (import "wasi_snapshot_preview1" "path_open" (func $path_open (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "proc_raise" (func $proc_raise (param i32) (result i32)))
  (import "wasi_snapshot_preview1" "poll_oneoff_noerrno" (func $no_errno (param i32)))
  (import "wasi_snapshot_preview1" "proc_exit" (func $proc_exit (param i32)))
  (memory (export "memory") 1)
  (func (export "args_sizes") (param $p i32) (result i32) (call $args_sizes_get (local.get $p) (i32.add (local.get $p) (i32.const 4))))
  (func (export "args") (param $ptrs i32) (param $buf i32) (result i32) (call $args_get (local.get $ptrs) (local.get $buf)))
  (func (export "env_sizes") (param $p i32) (result i32) (call $environ_sizes_get (local.get $p) (i32.add (local.get $p) (i32.const 4))))
  (func (export "env") (param $ptrs i32) (param $buf i32) (result i32) (call $environ_get (local.get $ptrs) (local.get $buf)))
  (func (export "write") (param $fd i32) (param $iovs i32) (param $n i32) (param $out i32) (result i32)
    (call $fd_write (local.get $fd) (local.get $iovs) (local.get $n) (local.get $out)))
  (func (export "read") (param $fd i32) (param $iovs i32) (param $n i32) (param $out i32) (result i32)
    (call $fd_read (local.get $fd) (local.get $iovs) (local.get $n) (local.get $out)))
  (func (export "close") (param i32) (result i32) (call $fd_close (local.get 0)))
  (func (export "seek") (param i32) (result i32) (call $fd_seek (local.get 0) (i64.const 0) (i32.const 0) (i32.const 0)))
  (func (export "fdstat") (param i32 i32) (result i32) (call $fd_fdstat_get (local.get 0) (local.get 1)))
  (func (export "prestat") (result i32) (call $fd_prestat_get (i32.const 3) (i32.const 0)))
  (func (export "prestat_name") (result i32) (call $fd_prestat_dir_name (i32.const 3) (i32.const 0) (i32.const 0)))
  (func (export "time") (param i32 i32) (result i32) (call $clock_time_get (local.get 0) (i64.const 0) (local.get 1)))
  (func (export "res") (param i32) (result i32) (call $clock_res_get (i32.const 0) (local.get 0)))
  (func (export "random") (param i32 i32) (result i32) (call $random_get (local.get 0) (local.get 1)))
  (func (export "yield") (result i32) (call $sched_yield))
  (func (export "open") (result i32) (call $path_open (i32.const 3) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i64.const 0) (i64.const 0) (i32.const 0) (i32.const 0)))
  (func (export "raise") (result i32) (call $proc_raise (i32.const 9)))
  (func (export "no_errno") (call $no_errno (i32.const 0)))
  (func (export "exit") (param i32) (call $proc_exit (local.get 0)))
)"#;

struct Probe {
    store: Store,
    instance: wasmlet::Instance,
    memory: wasmlet::Memory,
}

impl Probe {
    fn new(ctx: WasiCtx) -> Probe {
        let module = Module::new(&wat::parse_str(PROBE).unwrap()).unwrap();
        let mut store = Store::default();
        let mut linker = Linker::new();
        wasi::add_to_linker(&mut linker, &mut store, Rc::new(RefCell::new(ctx)));
        let instance = linker.instantiate(&mut store, &module).unwrap();
        let memory = match store.instance_export(instance, "memory") {
            Some(wasmlet::Extern::Memory(m)) => m,
            other => panic!("memory export: {other:?}"),
        };
        Probe {
            store,
            instance,
            memory,
        }
    }

    fn call(&mut self, name: &str, args: &[i32]) -> wasmlet::Result<Vec<Value>> {
        let f = self.store.instance_func(self.instance, name)?;
        let ty = self.store.func_type(f).clone();
        let args: Vec<Value> = args
            .iter()
            .zip(ty.params())
            .map(|(&a, _)| Value::I32(a))
            .collect();
        self.store.invoke(f, &args)
    }

    fn errno(&mut self, name: &str, args: &[i32]) -> i32 {
        self.call(name, args).unwrap()[0].as_i32().unwrap()
    }

    fn mem(&mut self) -> &mut [u8] {
        self.store.memory_data_mut(self.memory)
    }

    fn u32_at(&self, at: usize) -> u32 {
        let m = self.store.memory_data(self.memory);
        u32::from_le_bytes(m[at..at + 4].try_into().unwrap())
    }

    fn u64_at(&self, at: usize) -> u64 {
        let m = self.store.memory_data(self.memory);
        u64::from_le_bytes(m[at..at + 8].try_into().unwrap())
    }

    fn cstr_at(&self, at: usize) -> String {
        let m = self.store.memory_data(self.memory);
        let end = m[at..].iter().position(|&b| b == 0).unwrap();
        String::from_utf8(m[at..at + end].to_vec()).unwrap()
    }
}

const OOB: i32 = 70_000; // beyond the probe's single 64 KiB page

#[test]
fn args_and_environment_round_trip() {
    let mut p = Probe::new(
        WasiCtx::new()
            .args(["prog", "x y"])
            .env("K", "v")
            .env("EMPTY", ""),
    );
    assert_eq!(p.errno("args_sizes", &[0]), errno::SUCCESS);
    assert_eq!((p.u32_at(0), p.u32_at(4)), (2, 9));
    assert_eq!(p.errno("args", &[100, 200]), errno::SUCCESS);
    let (a0, a1) = (p.u32_at(100) as usize, p.u32_at(104) as usize);
    assert_eq!(
        (p.cstr_at(a0), p.cstr_at(a1)),
        ("prog".into(), "x y".into())
    );

    assert_eq!(p.errno("env_sizes", &[8]), errno::SUCCESS);
    assert_eq!((p.u32_at(8), p.u32_at(12)), (2, 11));
    assert_eq!(p.errno("env", &[300, 400]), errno::SUCCESS);
    assert_eq!(p.cstr_at(p.u32_at(300) as usize), "K=v");
    assert_eq!(p.cstr_at(p.u32_at(304) as usize), "EMPTY=");

    assert_eq!(p.errno("args_sizes", &[OOB]), errno::FAULT);
    assert_eq!(p.errno("args", &[100, 65_534]), errno::FAULT);
    assert_eq!(p.errno("env_sizes", &[-1]), errno::FAULT);
    assert_eq!(p.errno("env", &[OOB, 0]), errno::FAULT);
}

#[test]
fn fd_write_gathers_iovecs_and_reports_errors() {
    let ctx = Rc::new(RefCell::new(WasiCtx::new()));
    let module = Module::new(&wat::parse_str(PROBE).unwrap()).unwrap();
    let mut store = Store::default();
    let mut linker = Linker::new();
    wasi::add_to_linker(&mut linker, &mut store, Rc::clone(&ctx));
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let mut p = Probe {
        memory: match store.instance_export(instance, "memory") {
            Some(wasmlet::Extern::Memory(m)) => m,
            _ => unreachable!(),
        },
        store,
        instance,
    };
    p.mem()[1000..1005].copy_from_slice(b"hello");
    p.mem()[2000..2007].copy_from_slice(b" world\n");
    // Two iovecs at 0: {1000, 5}, {2000, 7}.
    for (i, v) in [1000u32, 5, 2000, 7].iter().enumerate() {
        p.mem()[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    assert_eq!(p.errno("write", &[1, 0, 2, 64]), errno::SUCCESS);
    assert_eq!(p.u32_at(64), 12);
    assert_eq!(p.errno("write", &[2, 0, 1, 64]), errno::SUCCESS);
    assert_eq!(ctx.borrow().stdout(), b"hello world\n");
    assert_eq!(ctx.borrow().stderr(), b"hello");
    assert_eq!(p.errno("write", &[7, 0, 1, 64]), errno::BADF);
    assert_eq!(p.errno("write", &[1, OOB, 1, 64]), errno::FAULT);
    // iovec pointing past the end of memory
    p.mem()[16..20].copy_from_slice(&65_530u32.to_le_bytes());
    p.mem()[20..24].copy_from_slice(&100u32.to_le_bytes());
    assert_eq!(p.errno("write", &[1, 16, 1, 64]), errno::FAULT);
}

#[test]
fn fd_read_drains_stdin_across_iovecs() {
    let mut p = Probe::new(WasiCtx::new().stdin(b"abcdefg".to_vec()));
    for (i, v) in [1000u32, 3, 2000, 10].iter().enumerate() {
        p.mem()[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    assert_eq!(p.errno("read", &[0, 0, 2, 64]), errno::SUCCESS);
    assert_eq!(p.u32_at(64), 7);
    assert_eq!(&p.store.memory_data(p.memory)[1000..1003], b"abc");
    assert_eq!(&p.store.memory_data(p.memory)[2000..2004], b"defg");
    assert_eq!(p.errno("read", &[0, 0, 2, 64]), errno::SUCCESS);
    assert_eq!(p.u32_at(64), 0, "EOF reads zero bytes");
    assert_eq!(p.errno("read", &[1, 0, 1, 64]), errno::BADF);
    assert_eq!(p.errno("read", &[0, OOB, 1, 64]), errno::FAULT);
}

#[test]
fn descriptor_queries() {
    let mut p = Probe::new(WasiCtx::new());
    assert_eq!(p.errno("close", &[1]), errno::SUCCESS);
    assert_eq!(p.errno("close", &[5]), errno::BADF);
    assert_eq!(p.errno("seek", &[0]), errno::SPIPE);
    assert_eq!(p.errno("seek", &[9]), errno::BADF);
    assert_eq!(p.errno("fdstat", &[1, 128]), errno::SUCCESS);
    let m = p.store.memory_data(p.memory);
    assert_eq!((m[128], m[130]), (2, 1), "character device, append");
    assert_eq!(p.errno("fdstat", &[0, 128]), errno::SUCCESS);
    assert_eq!(p.store.memory_data(p.memory)[130], 0);
    assert_eq!(p.errno("fdstat", &[4, 128]), errno::BADF);
    assert_eq!(p.errno("fdstat", &[1, OOB]), errno::FAULT);
    assert_eq!(p.errno("prestat", &[]), errno::BADF);
    assert_eq!(p.errno("prestat_name", &[]), errno::BADF);
    assert_eq!(p.errno("yield", &[]), errno::SUCCESS);
}

#[test]
fn deterministic_clock_and_random() {
    let mut p = Probe::new(WasiCtx::new().deterministic(true));
    assert_eq!(p.errno("time", &[1, 0]), errno::SUCCESS);
    assert_eq!(p.u64_at(0), 1_000_000);
    assert_eq!(p.errno("time", &[0, 0]), errno::SUCCESS);
    assert_eq!(p.u64_at(0), 2_000_000);
    assert_eq!(p.errno("time", &[9, 0]), errno::INVAL);
    assert_eq!(p.errno("time", &[0, OOB]), errno::FAULT);
    assert_eq!(p.errno("res", &[8]), errno::SUCCESS);
    assert_eq!(p.u64_at(8), 1_000);
    assert_eq!(p.errno("res", &[OOB]), errno::FAULT);

    assert_eq!(p.errno("random", &[100, 13]), errno::SUCCESS);
    let first = p.store.memory_data(p.memory)[100..113].to_vec();
    let mut q = Probe::new(WasiCtx::new().deterministic(true));
    assert_eq!(q.errno("random", &[100, 13]), errno::SUCCESS);
    assert_eq!(first, q.store.memory_data(q.memory)[100..113].to_vec());
    assert!(first.iter().any(|&b| b != 0));
    assert_eq!(p.errno("random", &[OOB, 4]), errno::FAULT);
}

#[test]
fn real_clock_is_after_2020() {
    let mut p = Probe::new(WasiCtx::new());
    assert_eq!(p.errno("time", &[0, 0]), errno::SUCCESS);
    assert!(p.u64_at(0) > 1_577_836_800 * 1_000_000_000);
}

#[test]
fn unimplemented_calls_are_stubbed() {
    let mut p = Probe::new(WasiCtx::new());
    assert_eq!(p.errno("open", &[]), errno::NOSYS);
    assert_eq!(p.errno("raise", &[]), errno::NOSYS);
    match p.call("no_errno", &[]) {
        Err(Error::Trap(Trap::Host(m))) => assert!(m.contains("poll_oneoff_noerrno"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(p.call("exit", &[42]), Err(Error::Trap(Trap::Exit(42))));
}

#[test]
fn wasi_needs_an_exported_memory() {
    let bytes = wat::parse_str(
        r#"(module
          (import "wasi_snapshot_preview1" "args_sizes_get" (func $f (param i32 i32) (result i32)))
          (func (export "_start") (drop (call $f (i32.const 0) (i32.const 4)))))"#,
    )
    .unwrap();
    let (status, _) = run(&bytes, WasiCtx::new());
    match status {
        Err(Error::Trap(Trap::Host(m))) => assert!(m.contains("without memory"), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn non_wasi_imports_still_fail_to_link() {
    let bytes =
        wat::parse_str(r#"(module (import "env" "f" (func)) (func (export "_start")))"#).unwrap();
    let (status, _) = run(&bytes, WasiCtx::new());
    match status {
        Err(Error::Link(e)) => assert!(e.message.contains("`env`.`f`"), "{e}"),
        other => panic!("{other:?}"),
    }
    let bytes = wat::parse_str(
        r#"(module (import "wasi_snapshot_preview1" "g" (global i32)) (func (export "_start")))"#,
    )
    .unwrap();
    assert!(matches!(run(&bytes, WasiCtx::new()).0, Err(Error::Link(_))));
}

#[test]
fn missing_start_is_a_usage_error() {
    let bytes = wat::parse_str("(module)").unwrap();
    assert!(matches!(
        run(&bytes, WasiCtx::new()).0,
        Err(Error::Usage(_))
    ));
}
