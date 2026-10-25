//! A deterministic fuzzer for wasmlet that runs on the stable toolchain.
//!
//! Two campaigns, both driven by a seeded PRNG so every run is reproducible:
//!
//! * [`decode_campaign`] feeds random and mutated bytes to [`wasmlet::Module::new`].
//!   It checks that the decoder and validator never panic and that wasmlet's
//!   accept/reject verdict agrees with `wasmparser`'s validator configured for
//!   the same feature set (WebAssembly 2.0 without SIMD). Accepted modules
//!   without imports are also instantiated and their exported functions run
//!   under a fuel budget, so the interpreter is exercised on garbage too.
//! * [`differential_campaign`] generates valid modules with `wasm-smith`,
//!   runs every exported function in both wasmlet and `wasmi`, and compares
//!   results, traps, exported globals and exported memories.
//!
//! Any panic, verdict disagreement or execution mismatch is a [`Finding`]; the
//! input bytes are kept so the CLI can save them for reproduction.

use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;

use arbitrary::Unstructured;
use wasmlet::{Config, Extern, Linker, Module, Store, Trap, Value};

pub mod diff;

/// SplitMix64: tiny, fast and good enough to drive mutations.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    /// A generator seeded with `seed`.
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A value in `0..n` (`n > 0`).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    /// `n` random bytes.
    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next_u64() as u8).collect()
    }
}

/// What went wrong on one input.
#[derive(Debug, Clone)]
pub struct Finding {
    /// Which check failed.
    pub kind: FindingKind,
    /// Details for a human.
    pub detail: String,
    /// The input that triggered it.
    pub input: Vec<u8>,
}

/// Classes of findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingKind {
    /// wasmlet panicked.
    Panic,
    /// wasmlet and wasmparser disagree on validity.
    VerdictMismatch,
    /// wasmlet and wasmi disagree on execution.
    ExecMismatch,
}

impl fmt::Display for FindingKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            FindingKind::Panic => "panic",
            FindingKind::VerdictMismatch => "verdict-mismatch",
            FindingKind::ExecMismatch => "exec-mismatch",
        })
    }
}

/// Counters and findings of one campaign.
#[derive(Debug, Clone, Default)]
pub struct Stats {
    /// Inputs tried.
    pub inputs: u64,
    /// Inputs wasmlet accepted as valid modules.
    pub accepted: u64,
    /// Inputs wasmlet rejected.
    pub rejected: u64,
    /// Exported functions executed.
    pub calls: u64,
    /// Differential cases that were compared end to end.
    pub compared: u64,
    /// Differential cases skipped because a resource limit differs by design
    /// between the engines (fuel, stack depth, allocation caps).
    pub inconclusive: u64,
    /// Everything that went wrong.
    pub findings: Vec<Finding>,
}

impl Stats {
    fn record(&mut self, kind: FindingKind, detail: String, input: &[u8]) {
        self.findings.push(Finding {
            kind,
            detail,
            input: input.to_vec(),
        });
    }
}

/// The store configuration used for fuzzing: small caps so garbage modules
/// cannot allocate gigabytes or loop forever.
pub fn fuzz_config() -> Config {
    Config {
        max_call_depth: 2_000,
        max_value_stack: 256 * 1024,
        max_memory_pages: 64,
        max_table_elements: 100_000,
        fuel: Some(FUEL),
    }
}

/// Fuel per exported-function call (wasmlet units: branches and calls).
pub const FUEL: u64 = 20_000;

/// Runs `f`, converting a panic into `Err(message)`.
pub fn catch<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    panic::catch_unwind(AssertUnwindSafe(f)).map_err(|p| {
        p.downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| p.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic payload".into())
    })
}

/// Seed inputs for mutation: every `.wasm` file under `fixtures/wasm` and the
/// binary encoding of every module in the vendored spec scripts.
pub fn load_corpus(root: &Path) -> Vec<Vec<u8>> {
    let mut corpus = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root.join("fixtures/wasm")) {
        let mut paths: Vec<_> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        paths.sort();
        for p in paths {
            if let Ok(b) = std::fs::read(&p) {
                corpus.push(b);
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(root.join("tests/spec/core")) {
        let mut paths: Vec<_> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        paths.sort();
        for p in paths {
            if let Ok(text) = std::fs::read_to_string(&p) {
                corpus.extend(modules_in_wast(&text));
            }
        }
    }
    corpus
}

/// Binary encodings of the modules defined in a `.wast` script (including
/// those inside `assert_invalid`/`assert_malformed`, which make good seeds).
pub fn modules_in_wast(text: &str) -> Vec<Vec<u8>> {
    use wast::parser::{self, ParseBuffer};
    use wast::{QuoteWat, Wast, WastDirective};
    let Ok(buf) = ParseBuffer::new(text) else {
        return Vec::new();
    };
    let Ok(script) = parser::parse::<Wast>(&buf) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for d in script.directives {
        let mut m: QuoteWat<'_> = match d {
            WastDirective::Module(m) => m,
            WastDirective::AssertInvalid { module, .. } => module,
            WastDirective::AssertMalformed { module, .. } => module,
            _ => continue,
        };
        if let Ok(b) = m.encode() {
            out.push(b);
        }
    }
    out
}

/// Applies 1-4 random structural mutations to `input`.
pub fn mutate(rng: &mut Rng, input: &[u8], corpus: &[Vec<u8>]) -> Vec<u8> {
    const INTERESTING: [u8; 10] = [0x00, 0x01, 0x40, 0x7f, 0x80, 0xff, 0x0b, 0x60, 0x70, 0x6f];
    let mut v = input.to_vec();
    for _ in 0..=rng.below(4) {
        let len = v.len();
        match rng.below(9) {
            0 if len > 0 => {
                let i = rng.below(len);
                v[i] ^= 1 << rng.below(8);
            }
            1 if len > 0 => {
                let i = rng.below(len);
                v[i] = rng.next_u64() as u8;
            }
            2 if len > 0 => {
                let i = rng.below(len);
                v[i] = INTERESTING[rng.below(INTERESTING.len())];
            }
            3 => {
                let i = rng.below(len + 1);
                let n = 1 + rng.below(8);
                let ins = rng.bytes(n);
                v.splice(i..i, ins);
            }
            4 if len > 0 => {
                let i = rng.below(len);
                let n = 1 + rng.below((len - i).min(16));
                v.drain(i..i + n);
            }
            5 if len > 0 => {
                v.truncate(rng.below(len));
            }
            6 if len > 0 => {
                let i = rng.below(len);
                let n = 1 + rng.below((len - i).min(32));
                let dup = v[i..i + n].to_vec();
                let at = rng.below(len + 1);
                v.splice(at..at, dup);
            }
            7 if !corpus.is_empty() => {
                let other = &corpus[rng.below(corpus.len())];
                if !other.is_empty() {
                    let i = rng.below(other.len());
                    let n = 1 + rng.below((other.len() - i).min(64));
                    let at = rng.below(len + 1);
                    v.splice(at..at, other[i..i + n].iter().copied());
                }
            }
            8 if len > 0 => {
                // Overwrite a byte with a LEB128 continuation-heavy value.
                let i = rng.below(len);
                v[i] = 0x80 | (rng.next_u64() as u8);
            }
            _ => {}
        }
    }
    v
}

/// The `wasmparser` feature set matching wasmlet: WebAssembly 2.0 minus SIMD.
pub fn reference_features() -> wasmparser::WasmFeatures {
    use wasmparser::WasmFeatures as F;
    F::WASM2 & !F::SIMD & !F::RELAXED_SIMD
}

/// Does `wasmparser` consider `bytes` a valid module under [`reference_features`]?
pub fn reference_valid(bytes: &[u8]) -> Result<(), String> {
    let mut v = wasmparser::Validator::new_with_features(reference_features());
    v.validate_all(bytes).map(|_| ()).map_err(|e| e.to_string())
}

/// Checks one input: no panic anywhere, and the same verdict as wasmparser.
pub fn check_input(bytes: &[u8], stats: &mut Stats) {
    stats.inputs += 1;
    let ours = match catch(|| Module::new(bytes)) {
        Ok(r) => r,
        Err(msg) => {
            stats.record(
                FindingKind::Panic,
                format!("Module::new panicked: {msg}"),
                bytes,
            );
            return;
        }
    };
    let reference = reference_valid(bytes);
    match (&ours, &reference) {
        (Ok(_), Err(e)) => stats.record(
            FindingKind::VerdictMismatch,
            format!("wasmlet accepted, wasmparser rejected: {e}"),
            bytes,
        ),
        (Err(e), Ok(())) => stats.record(
            FindingKind::VerdictMismatch,
            format!("wasmlet rejected ({e}), wasmparser accepted"),
            bytes,
        ),
        _ => {}
    }
    match ours {
        Ok(module) => {
            stats.accepted += 1;
            if module.imports().next().is_none() {
                if let Err(msg) = catch(|| run_exports(&module, stats)) {
                    stats.record(
                        FindingKind::Panic,
                        format!("execution panicked: {msg}"),
                        bytes,
                    );
                }
            }
        }
        Err(_) => stats.rejected += 1,
    }
}

/// Instantiates `module` (no imports) and calls every exported function with
/// zero/null arguments under the fuzz fuel budget. Returns the outcomes.
pub fn run_exports(module: &Module, stats: &mut Stats) -> Vec<(String, Result<Vec<Value>, Trap>)> {
    let mut store = Store::new(fuzz_config());
    let Ok(inst) = Linker::new().instantiate(&mut store, module) else {
        return Vec::new();
    };
    let funcs: Vec<_> = store
        .instance_exports(inst)
        .filter_map(|(n, e)| match e {
            Extern::Func(f) => Some((n.to_owned(), f)),
            _ => None,
        })
        .collect();
    let mut out = Vec::new();
    for (name, f) in funcs {
        let args: Vec<Value> = store
            .func_type(f)
            .params()
            .iter()
            .map(|&t| Value::default_for(t))
            .collect();
        store.set_fuel(Some(FUEL));
        stats.calls += 1;
        let r = store.invoke(f, &args).map_err(|e| match e {
            wasmlet::Error::Trap(t) => t,
            other => Trap::Host(other.to_string()),
        });
        out.push((name, r));
    }
    out
}

/// Random and mutated inputs: `iters` cases from `seed`.
pub fn decode_campaign(seed: u64, iters: u64, corpus: &[Vec<u8>]) -> Stats {
    let mut rng = Rng::new(seed);
    let mut stats = Stats::default();
    for _ in 0..iters {
        let input = match rng.below(10) {
            // Pure noise, half of it behind a valid header.
            0 => {
                let n = rng.below(64);
                rng.bytes(n)
            }
            1 => {
                let mut v = b"\0asm\x01\0\0\0".to_vec();
                let n = rng.below(128);
                v.extend(rng.bytes(n));
                v
            }
            _ if !corpus.is_empty() => {
                let seed_input = &corpus[rng.below(corpus.len())];
                mutate(&mut rng, seed_input, corpus)
            }
            _ => rng.bytes(32),
        };
        check_input(&input, &mut stats);
    }
    stats
}

/// `wasm-smith` configuration restricted to what wasmlet implements.
pub fn smith_config() -> wasm_smith::Config {
    wasm_smith::Config {
        simd_enabled: false,
        relaxed_simd_enabled: false,
        threads_enabled: false,
        shared_everything_threads_enabled: false,
        exceptions_enabled: false,
        gc_enabled: false,
        tail_call_enabled: false,
        memory64_enabled: false,
        custom_page_sizes_enabled: false,
        wide_arithmetic_enabled: false,
        extended_const_enabled: false,
        compact_imports_enabled: false,
        custom_descriptors_enabled: false,
        reference_types_enabled: true,
        bulk_memory_enabled: true,
        multi_value_enabled: true,
        saturating_float_to_int_enabled: true,
        sign_extension_ops_enabled: true,
        canonicalize_nans: true,
        export_everything: true,
        max_imports: 0,
        max_memories: 1,
        max_tables: 3,
        max_memory32_bytes: 16 * 65_536,
        // Declared maxima keep memory.grow/table.grow below wasmlet's fuzz caps,
        // so growth results cannot differ for reasons of policy.
        memory_max_size_required: true,
        table_max_size_required: true,
        max_table_elements: 1_000,
        max_funcs: 20,
        max_instructions: 200,
        min_funcs: 1,
        ..wasm_smith::Config::default()
    }
}

/// Runs `f` on a thread with a 1 GiB stack (reserved lazily by the OS).
///
/// wasmi recurses on the native stack for some deeply recursive guests and
/// overflows the default 8 MiB main-thread stack before reaching its own
/// call-depth limit; wasmlet itself never recurses on guest calls.
pub fn on_big_stack<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(1 << 30)
            .spawn_scoped(s, f)
            .expect("spawn fuzz worker thread")
            .join()
            .unwrap_or_else(|p| std::panic::resume_unwind(p))
    })
}

/// Generates `iters` modules with wasm-smith and compares wasmlet with wasmi.
pub fn differential_campaign(seed: u64, iters: u64) -> Stats {
    on_big_stack(|| differential_campaign_inner(seed, iters))
}

fn differential_campaign_inner(seed: u64, iters: u64) -> Stats {
    let mut rng = Rng::new(seed);
    let mut stats = Stats::default();
    let config = smith_config();
    for _ in 0..iters {
        let n = 256 + rng.below(4096);
        let entropy = rng.bytes(n);
        let mut u = Unstructured::new(&entropy);
        let Ok(module) = wasm_smith::Module::new(config.clone(), &mut u) else {
            continue;
        };
        let bytes = module.to_bytes();
        stats.inputs += 1;
        diff::compare(&bytes, &mut stats);
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_deterministic() {
        let (mut a, mut b) = (Rng::new(7), Rng::new(7));
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        assert!(Rng::new(1).below(10) < 10);
    }

    #[test]
    fn mutate_changes_inputs_without_panicking() {
        let mut rng = Rng::new(3);
        let seed = b"\0asm\x01\0\0\0".to_vec();
        let corpus = vec![seed.clone()];
        let mut changed = 0;
        for _ in 0..200 {
            if mutate(&mut rng, &seed, &corpus) != seed {
                changed += 1;
            }
        }
        assert!(changed > 150);
        assert!(mutate(&mut rng, &[], &[]).len() <= 8 * 5);
    }

    #[test]
    fn wast_modules_are_extracted() {
        let mods = modules_in_wast("(module (func (export \"f\") (result i32) i32.const 1))\n(assert_invalid (module (func (result i32))) \"type mismatch\")");
        assert_eq!(mods.len(), 2);
        assert!(reference_valid(&mods[0]).is_ok());
        assert!(reference_valid(&mods[1]).is_err());
    }

    #[test]
    fn catch_reports_panic_messages() {
        assert_eq!(catch(|| 1).unwrap(), 1);
        let e = catch(|| -> u8 { panic!("boom {}", 1) }).unwrap_err();
        assert_eq!(e, "boom 1");
    }
}
