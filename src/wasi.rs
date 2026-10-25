//! A WASI preview1 (`wasi_snapshot_preview1`) subset: enough to run
//! command-line programs that use stdio, arguments, environment variables,
//! clocks, randomness and `proc_exit`. There is no filesystem: no directories
//! are preopened, so file opens fail with `EBADF`/`ENOSYS`.
//!
//! Functions outside the subset are linked as stubs that return `ENOSYS`, so
//! modules that merely *import* them still instantiate.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::module::ExternType;
use crate::runtime::{Caller, Extern, Linker, Store, Trap, Value};
use crate::types::{FuncType, ValType};

/// The WASI preview1 import module name.
pub const MODULE: &str = "wasi_snapshot_preview1";

/// WASI errno values used by this subset.
#[allow(missing_docs)]
pub mod errno {
    pub const SUCCESS: i32 = 0;
    pub const BADF: i32 = 8;
    pub const FAULT: i32 = 21;
    pub const INVAL: i32 = 28;
    pub const NOSYS: i32 = 52;
    pub const SPIPE: i32 = 70;
}

/// Where guest stdout/stderr go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// Forward to the host process's stdout/stderr.
    Inherit,
    /// Buffer in [`WasiCtx`] (see [`WasiCtx::stdout`]).
    Capture,
}

/// Configuration and state of one WASI program.
#[derive(Debug)]
pub struct WasiCtx {
    args: Vec<String>,
    env: Vec<(String, String)>,
    stdin: Vec<u8>,
    stdin_pos: usize,
    output: Output,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    deterministic: bool,
    rng: u64,
    clock_ticks: u64,
}

impl Default for WasiCtx {
    fn default() -> Self {
        WasiCtx::new()
    }
}

impl WasiCtx {
    /// A context with no arguments, no environment, empty stdin and captured output.
    pub fn new() -> WasiCtx {
        WasiCtx {
            args: Vec::new(),
            env: Vec::new(),
            stdin: Vec::new(),
            stdin_pos: 0,
            output: Output::Capture,
            stdout: Vec::new(),
            stderr: Vec::new(),
            deterministic: false,
            rng: 0x9e37_79b9_7f4a_7c15,
            clock_ticks: 0,
        }
    }

    /// Sets `argv` (including the program name as `argv[0]`).
    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    /// Adds an environment variable.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Provides the bytes the guest reads from fd 0.
    pub fn stdin(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.stdin = bytes.into();
        self
    }

    /// Selects inherited or captured stdout/stderr.
    pub fn output(mut self, output: Output) -> Self {
        self.output = output;
        self
    }

    /// Deterministic mode: clocks advance by 1 ms per query from zero and
    /// `random_get` uses a fixed-seed PRNG, so runs are reproducible.
    pub fn deterministic(mut self, on: bool) -> Self {
        self.deterministic = on;
        self
    }

    /// Captured stdout.
    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    /// Captured stderr.
    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    fn write_fd(&mut self, fd: i32, bytes: &[u8]) -> Result<(), i32> {
        match (fd, self.output) {
            (1, Output::Capture) => self.stdout.extend_from_slice(bytes),
            (2, Output::Capture) => self.stderr.extend_from_slice(bytes),
            (1, Output::Inherit) => {
                let mut out = std::io::stdout();
                out.write_all(bytes).map_err(|_| errno::BADF)?;
                out.flush().map_err(|_| errno::BADF)?;
            }
            (2, Output::Inherit) => std::io::stderr()
                .write_all(bytes)
                .map_err(|_| errno::BADF)?,
            _ => return Err(errno::BADF),
        }
        Ok(())
    }

    fn next_random(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn now_ns(&mut self, clock: i32) -> Result<u64, i32> {
        if !(0..=3).contains(&clock) {
            return Err(errno::INVAL);
        }
        if self.deterministic {
            self.clock_ticks += 1_000_000;
            return Ok(self.clock_ticks);
        }
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .map_err(|_| errno::INVAL)
    }
}

type WasiResult = Result<i32, Trap>;

fn mem<'a>(caller: &'a mut Caller<'_>) -> Result<&'a mut Vec<u8>, Trap> {
    caller
        .memory()
        .ok_or_else(|| Trap::Host("WASI call from a module without memory".into()))
}

fn range(m: &[u8], ptr: i32, len: usize) -> Result<std::ops::Range<usize>, i32> {
    let start = ptr as u32 as usize;
    let end = start.checked_add(len).ok_or(errno::FAULT)?;
    if end > m.len() {
        return Err(errno::FAULT);
    }
    Ok(start..end)
}

fn put_u32(m: &mut [u8], ptr: i32, v: u32) -> Result<(), i32> {
    let r = range(m, ptr, 4)?;
    m[r].copy_from_slice(&v.to_le_bytes());
    Ok(())
}

fn put_u64(m: &mut [u8], ptr: i32, v: u64) -> Result<(), i32> {
    let r = range(m, ptr, 8)?;
    m[r].copy_from_slice(&v.to_le_bytes());
    Ok(())
}

fn get_u32(m: &[u8], ptr: i32) -> Result<u32, i32> {
    let r = range(m, ptr, 4)?;
    let mut b = [0u8; 4];
    b.copy_from_slice(&m[r]);
    Ok(u32::from_le_bytes(b))
}

/// Reads an iovec array as `(ptr, len)` pairs.
fn iovecs(m: &[u8], iovs: i32, n: i32) -> Result<Vec<(i32, usize)>, i32> {
    (0..n as u32)
        .map(|i| {
            let base = iovs.wrapping_add((i * 8) as i32);
            Ok((
                get_u32(m, base)? as i32,
                get_u32(m, base.wrapping_add(4))? as usize,
            ))
        })
        .collect()
}

fn errno_of(r: Result<(), i32>) -> i32 {
    match r {
        Ok(()) => errno::SUCCESS,
        Err(e) => e,
    }
}

/// Writes a list of strings as a NUL-terminated buffer plus pointer array.
fn put_string_list(m: &mut [u8], ptrs: i32, buf: i32, items: &[String]) -> Result<(), i32> {
    let mut off = buf as u32;
    for (i, s) in items.iter().enumerate() {
        put_u32(m, ptrs.wrapping_add((i * 4) as i32), off)?;
        let r = range(m, off as i32, s.len() + 1)?;
        m[r.start..r.end - 1].copy_from_slice(s.as_bytes());
        m[r.end - 1] = 0;
        off += s.len() as u32 + 1;
    }
    Ok(())
}

fn sizes(items: &[String]) -> (u32, u32) {
    (
        items.len() as u32,
        items.iter().map(|s| s.len() as u32 + 1).sum(),
    )
}

fn arg(args: &[Value], i: usize) -> i32 {
    match args.get(i) {
        Some(Value::I32(v)) => *v,
        _ => 0,
    }
}

/// Registers the WASI subset on `linker` and installs a fallback that stubs
/// any other `wasi_snapshot_preview1` import with an `ENOSYS` return.
pub fn add_to_linker(linker: &mut Linker, store: &mut Store, ctx: Rc<RefCell<WasiCtx>>) {
    use ValType::{I32, I64};
    let ty = |p: &[ValType], r: &[ValType]| FuncType::new(p.iter().copied(), r.iter().copied());
    let ok = |e: i32| Ok(vec![Value::I32(e)]);

    macro_rules! def {
        ($name:literal, $params:expr, $results:expr, |$c:ident, $caller:ident, $a:ident| $body:expr) => {{
            let $c = Rc::clone(&ctx);
            linker.func(
                store,
                MODULE,
                $name,
                ty(&$params, &$results),
                move |$caller, $a| {
                    let r: WasiResult = $body;
                    r.map(|e| vec![Value::I32(e)])
                },
            );
        }};
    }

    def!("args_sizes_get", [I32, I32], [I32], |c, caller, a| {
        let (n, len) = sizes(&c.borrow().args);
        let m = mem(caller)?;
        Ok(errno_of(
            put_u32(m, arg(a, 0), n).and_then(|_| put_u32(m, arg(a, 1), len)),
        ))
    });
    def!("args_get", [I32, I32], [I32], |c, caller, a| {
        let items = c.borrow().args.clone();
        Ok(errno_of(put_string_list(
            mem(caller)?,
            arg(a, 0),
            arg(a, 1),
            &items,
        )))
    });
    def!("environ_sizes_get", [I32, I32], [I32], |c, caller, a| {
        let items: Vec<String> = c
            .borrow()
            .env
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        let (n, len) = sizes(&items);
        let m = mem(caller)?;
        Ok(errno_of(
            put_u32(m, arg(a, 0), n).and_then(|_| put_u32(m, arg(a, 1), len)),
        ))
    });
    def!("environ_get", [I32, I32], [I32], |c, caller, a| {
        let items: Vec<String> = c
            .borrow()
            .env
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        Ok(errno_of(put_string_list(
            mem(caller)?,
            arg(a, 0),
            arg(a, 1),
            &items,
        )))
    });
    def!("fd_write", [I32, I32, I32, I32], [I32], |c, caller, a| {
        let (fd, iovs, n, out) = (arg(a, 0), arg(a, 1), arg(a, 2), arg(a, 3));
        let m = mem(caller)?;
        let r = (|| {
            let mut total = 0u32;
            let mut buf = Vec::new();
            for (ptr, len) in iovecs(m, iovs, n)? {
                buf.extend_from_slice(&m[range(m, ptr, len)?]);
                total += len as u32;
            }
            c.borrow_mut().write_fd(fd, &buf)?;
            put_u32(m, out, total)
        })();
        Ok(errno_of(r))
    });
    def!("fd_read", [I32, I32, I32, I32], [I32], |c, caller, a| {
        let (fd, iovs, n, out) = (arg(a, 0), arg(a, 1), arg(a, 2), arg(a, 3));
        if fd != 0 {
            return ok(errno::BADF);
        }
        let m = mem(caller)?;
        let r = (|| {
            let mut ctx = c.borrow_mut();
            let mut total = 0usize;
            for (ptr, len) in iovecs(m, iovs, n)? {
                let dst = range(m, ptr, len)?;
                let avail = &ctx.stdin[ctx.stdin_pos..];
                let k = avail.len().min(len);
                m[dst.start..dst.start + k].copy_from_slice(&avail[..k]);
                ctx.stdin_pos += k;
                total += k;
                if k < len {
                    break;
                }
            }
            put_u32(m, out, total as u32)
        })();
        Ok(errno_of(r))
    });
    def!("fd_close", [I32], [I32], |_c, _caller, a| {
        Ok(if (0..=2).contains(&arg(a, 0)) {
            errno::SUCCESS
        } else {
            errno::BADF
        })
    });
    def!("fd_seek", [I32, I64, I32, I32], [I32], |_c, _caller, a| {
        Ok(if (0..=2).contains(&arg(a, 0)) {
            errno::SPIPE
        } else {
            errno::BADF
        })
    });
    def!("fd_fdstat_get", [I32, I32], [I32], |_c, caller, a| {
        let (fd, buf) = (arg(a, 0), arg(a, 1));
        if !(0..=2).contains(&fd) {
            return ok(errno::BADF);
        }
        let m = mem(caller)?;
        let r = range(m, buf, 24).map(|r| {
            m[r.clone()].fill(0);
            m[r.start] = 2; // filetype: character_device
            if fd > 0 {
                m[r.start + 2] = 1; // fdflags: append
            }
            m[r.start + 8..r.start + 16].copy_from_slice(&u64::MAX.to_le_bytes());
        });
        Ok(errno_of(r))
    });
    def!("fd_prestat_get", [I32, I32], [I32], |_c, _caller, _a| Ok(
        errno::BADF
    ));
    def!(
        "fd_prestat_dir_name",
        [I32, I32, I32],
        [I32],
        |_c, _caller, _a| { Ok(errno::BADF) }
    );
    def!("clock_time_get", [I32, I64, I32], [I32], |c, caller, a| {
        let t = c.borrow_mut().now_ns(arg(a, 0));
        let m = mem(caller)?;
        Ok(errno_of(t.and_then(|t| put_u64(m, arg(a, 2), t))))
    });
    def!("clock_res_get", [I32, I32], [I32], |_c, caller, a| {
        let m = mem(caller)?;
        Ok(errno_of(put_u64(m, arg(a, 1), 1_000)))
    });
    def!("random_get", [I32, I32], [I32], |c, caller, a| {
        let m = mem(caller)?;
        let r = range(m, arg(a, 0), arg(a, 1) as u32 as usize).map(|r| {
            let mut ctx = c.borrow_mut();
            for chunk in m[r].chunks_mut(8) {
                let v = ctx.next_random().to_le_bytes();
                chunk.copy_from_slice(&v[..chunk.len()]);
            }
        });
        Ok(errno_of(r))
    });
    def!("sched_yield", [], [I32], |_c, _caller, _a| Ok(
        errno::SUCCESS
    ));
    linker.func(store, MODULE, "proc_exit", ty(&[I32], &[]), |_, a| {
        Err(Trap::Exit(arg(a, 0)))
    });

    linker.set_fallback(Box::new(
        |store: &mut Store, module: &str, name: &str, ty: &ExternType| {
            let ExternType::Func(ft) = ty else {
                return None;
            };
            if module != MODULE {
                return None;
            }
            log::warn!("WASI function `{name}` is not implemented; linking an ENOSYS stub");
            let returns_errno = ft.results() == [ValType::I32];
            let name = name.to_owned();
            let f = store.func_new(ft.clone(), move |_, _| {
                if returns_errno {
                    Ok(vec![Value::I32(errno::NOSYS)])
                } else {
                    Err(Trap::Host(format!("unsupported WASI function `{name}`")))
                }
            });
            Some(Extern::Func(f))
        },
    ));
}

/// Outcome of running a WASI command.
#[derive(Debug, Clone, PartialEq)]
pub enum ExitStatus {
    /// `_start` returned normally (exit code 0).
    Returned,
    /// The program called `proc_exit(code)`.
    Exited(i32),
}

impl ExitStatus {
    /// The process exit code.
    pub fn code(&self) -> i32 {
        match self {
            ExitStatus::Returned => 0,
            ExitStatus::Exited(c) => *c,
        }
    }
}

/// Instantiates a WASI command module and runs its `_start` export.
pub fn run_command(
    store: &mut Store,
    module: &crate::Module,
    ctx: Rc<RefCell<WasiCtx>>,
) -> crate::Result<ExitStatus> {
    let mut linker = Linker::new();
    add_to_linker(&mut linker, store, ctx);
    let instance = linker.instantiate(store, module)?;
    let start = store.instance_func(instance, "_start")?;
    match store.invoke(start, &[]) {
        Ok(_) => Ok(ExitStatus::Returned),
        Err(crate::Error::Trap(Trap::Exit(code))) => Ok(ExitStatus::Exited(code)),
        Err(e) => Err(e),
    }
}
