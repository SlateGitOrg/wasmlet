//! Differential execution: wasmlet vs wasmi on the same valid module.
//!
//! Both engines instantiate the module (running its start function), then
//! every exported function is called in export order with zero/null
//! arguments, and finally all exported globals, memories and tables are
//! compared. Values are compared bit-exactly (`wasm-smith` canonicalises NaNs
//! produced by arithmetic); references are compared by null-ness only, since
//! handles are engine-specific. Traps are compared by category.
//!
//! Resource limits legitimately differ between engines (fuel accounting,
//! call-depth limits), so any case where either engine hits one is counted as
//! *inconclusive* and abandoned, never as a mismatch.

use wasmi::TrapCode;
use wasmlet::{Extern, Linker, Module, Store, Trap, Value};

use crate::{catch, fuzz_config, reference_valid, FindingKind, Stats, FUEL};

/// Fuel for wasmi per call. wasmi meters roughly per instruction while
/// wasmlet meters per branch/call, so wasmi gets far more; it only runs after
/// wasmlet has finished the same call within [`FUEL`].
const WASMI_FUEL: u64 = 200_000_000;

/// An engine-neutral observation.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Obs {
    I32(u32),
    I64(u64),
    F32(u32),
    F64(u64),
    Ref { null: bool },
}

/// An engine-neutral outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Values(Vec<Obs>),
    Trap(&'static str),
    /// A resource limit was hit; the case cannot be compared.
    Limit,
}

fn obs_wasmlet(v: &Value) -> Obs {
    match *v {
        Value::I32(x) => Obs::I32(x as u32),
        Value::I64(x) => Obs::I64(x as u64),
        Value::F32(x) => Obs::F32(x.to_bits()),
        Value::F64(x) => Obs::F64(x.to_bits()),
        Value::FuncRef(r) => Obs::Ref { null: r.is_none() },
        Value::ExternRef(r) => Obs::Ref { null: r.is_none() },
    }
}

fn obs_wasmi(v: &wasmi::Val) -> Obs {
    match v {
        wasmi::Val::I32(x) => Obs::I32(*x as u32),
        wasmi::Val::I64(x) => Obs::I64(*x as u64),
        wasmi::Val::F32(x) => Obs::F32(x.to_bits()),
        wasmi::Val::F64(x) => Obs::F64(x.to_bits()),
        wasmi::Val::FuncRef(r) => Obs::Ref { null: r.is_null() },
        wasmi::Val::ExternRef(r) => Obs::Ref { null: r.is_null() },
        other => unreachable!("wasm-smith is configured without SIMD, got {other:?}"),
    }
}

fn trap_wasmlet(t: &Trap) -> Outcome {
    Outcome::Trap(match t {
        Trap::Unreachable => "unreachable",
        Trap::MemoryOutOfBounds => "memory-oob",
        // wasmi reports an out-of-range call_indirect index as a table access.
        Trap::TableOutOfBounds | Trap::UndefinedElement => "table-oob",
        Trap::UninitializedElement => "null-call",
        Trap::IndirectCallTypeMismatch => "signature",
        Trap::IntegerDivideByZero => "div-zero",
        Trap::IntegerOverflow => "int-overflow",
        Trap::InvalidConversion => "bad-conversion",
        Trap::CallStackExhausted | Trap::OutOfFuel => return Outcome::Limit,
        Trap::Exit(_) | Trap::Host(_) => "host",
    })
}

fn trap_wasmi(e: &wasmi::Error) -> Outcome {
    let Some(code) = e.as_trap_code() else {
        // Segment initialisation failures during instantiation surface as
        // plain errors rather than trap codes in wasmi.
        let msg = e.to_string();
        return Outcome::Trap(if msg.contains("out of bounds table access") {
            "table-oob"
        } else if msg.contains("out of bounds memory access") {
            "memory-oob"
        } else {
            "host"
        });
    };
    Outcome::Trap(match code {
        TrapCode::UnreachableCodeReached => "unreachable",
        TrapCode::MemoryOutOfBounds => "memory-oob",
        TrapCode::TableOutOfBounds => "table-oob",
        TrapCode::IndirectCallToNull => "null-call",
        TrapCode::BadSignature => "signature",
        TrapCode::IntegerDivisionByZero => "div-zero",
        TrapCode::IntegerOverflow => "int-overflow",
        TrapCode::BadConversionToInteger => "bad-conversion",
        _ => return Outcome::Limit,
    })
}

struct Ours {
    store: Store,
    inst: wasmlet::Instance,
}

struct Theirs {
    store: wasmi::Store<()>,
    inst: wasmi::Instance,
}

fn instantiate_ours(module: &Module) -> Result<Ours, Outcome> {
    let mut store = Store::new(fuzz_config());
    store.set_fuel(Some(FUEL));
    match Linker::new().instantiate(&mut store, module) {
        Ok(inst) => Ok(Ours { store, inst }),
        Err(wasmlet::Error::Trap(t)) => Err(trap_wasmlet(&t)),
        Err(_) => Err(Outcome::Trap("instantiation")),
    }
}

fn instantiate_theirs(bytes: &[u8]) -> Result<Theirs, Outcome> {
    let mut config = wasmi::Config::default();
    config.consume_fuel(true);
    let engine = wasmi::Engine::new(&config);
    let module = wasmi::Module::new(&engine, bytes).map_err(|_| Outcome::Trap("compile"))?;
    let mut store = wasmi::Store::new(&engine, ());
    store.set_fuel(WASMI_FUEL).map_err(|_| Outcome::Limit)?;
    match wasmi::Linker::<()>::new(&engine).instantiate_and_start(&mut store, &module) {
        Ok(inst) => Ok(Theirs { store, inst }),
        Err(e) => Err(trap_wasmi(&e)),
    }
}

fn call_ours(o: &mut Ours, name: &str) -> Outcome {
    let Ok(f) = o.store.instance_func(o.inst, name) else {
        return Outcome::Trap("missing");
    };
    let args: Vec<Value> = o
        .store
        .func_type(f)
        .params()
        .iter()
        .map(|&t| Value::default_for(t))
        .collect();
    o.store.set_fuel(Some(FUEL));
    match o.store.invoke(f, &args) {
        Ok(vals) => Outcome::Values(vals.iter().map(obs_wasmlet).collect()),
        Err(wasmlet::Error::Trap(t)) => trap_wasmlet(&t),
        Err(_) => Outcome::Trap("usage"),
    }
}

fn call_theirs(t: &mut Theirs, name: &str) -> Outcome {
    let Some(f) = t.inst.get_func(&t.store, name) else {
        return Outcome::Trap("missing");
    };
    let ty = f.ty(&t.store);
    let args: Vec<wasmi::Val> = ty
        .params()
        .iter()
        .map(|&p| wasmi::Val::default_for_ty(p))
        .collect();
    let mut out: Vec<wasmi::Val> = ty
        .results()
        .iter()
        .map(|&r| wasmi::Val::default_for_ty(r))
        .collect();
    if t.store.set_fuel(WASMI_FUEL).is_err() {
        return Outcome::Limit;
    }
    match f.call(&mut t.store, &args, &mut out) {
        Ok(()) => Outcome::Values(out.iter().map(obs_wasmi).collect()),
        Err(e) => trap_wasmi(&e),
    }
}

/// Exported state after all calls, as comparable text.
fn state_ours(o: &Ours) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (name, ext) in o.store.instance_exports(o.inst) {
        let desc = match ext {
            Extern::Global(g) => format!("global {:?}", obs_wasmlet(&o.store.global_get(g))),
            Extern::Memory(m) => format!("memory {}", fingerprint(o.store.memory_data(m))),
            Extern::Table(t) => {
                let size = o.store.table_type(t).limits.min;
                let nulls: Vec<bool> = (0..size)
                    .map(|i| {
                        o.store
                            .table_get(t, i)
                            .is_some_and(|v| matches!(obs_wasmlet(&v), Obs::Ref { null: true }))
                    })
                    .collect();
                format!("table {size} {}", fingerprint_bools(&nulls))
            }
            Extern::Func(_) => continue,
        };
        out.push((name.to_owned(), desc));
    }
    out.sort();
    out
}

fn state_theirs(t: &Theirs) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for export in t.inst.exports(&t.store) {
        let name = export.name().to_owned();
        let desc = match export.into_extern() {
            wasmi::Extern::Global(g) => format!("global {:?}", obs_wasmi(&g.get(&t.store))),
            wasmi::Extern::Memory(m) => format!("memory {}", fingerprint(m.data(&t.store))),
            wasmi::Extern::Table(tab) => {
                let size = tab.size(&t.store);
                let nulls: Vec<bool> = (0..size)
                    .map(|i| tab.get(&t.store, i).is_some_and(|r| r.is_null()))
                    .collect();
                format!("table {size} {}", fingerprint_bools(&nulls))
            }
            wasmi::Extern::Func(_) => continue,
        };
        out.push((name, desc));
    }
    out.sort();
    out
}

/// FNV-1a over the bytes, plus the length.
fn fingerprint(data: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{}:{h:016x}", data.len())
}

fn fingerprint_bools(v: &[bool]) -> String {
    let bytes: Vec<u8> = v.iter().map(|&b| u8::from(b)).collect();
    fingerprint(&bytes)
}

/// Runs one valid module through both engines and records any disagreement.
pub fn compare(bytes: &[u8], stats: &mut Stats) {
    if reference_valid(bytes).is_err() {
        // wasm-smith produced something outside the shared feature set.
        stats.inconclusive += 1;
        return;
    }
    let module = match catch(|| Module::new(bytes)) {
        Err(msg) => {
            stats.record(
                FindingKind::Panic,
                format!("Module::new panicked: {msg}"),
                bytes,
            );
            return;
        }
        Ok(Err(e)) => {
            stats.rejected += 1;
            stats.record(
                FindingKind::VerdictMismatch,
                format!("wasmlet rejected a valid wasm-smith module: {e}"),
                bytes,
            );
            return;
        }
        Ok(Ok(m)) => m,
    };
    stats.accepted += 1;
    let result = catch(|| {
        let mut stats = Stats::default();
        run_pair(&module, bytes, &mut stats);
        stats
    });
    match result {
        Ok(s) => {
            stats.calls += s.calls;
            stats.compared += s.compared;
            stats.inconclusive += s.inconclusive;
            stats.findings.extend(s.findings);
        }
        Err(msg) => stats.record(
            FindingKind::Panic,
            format!("execution panicked: {msg}"),
            bytes,
        ),
    }
}

fn run_pair(module: &Module, bytes: &[u8], stats: &mut Stats) {
    let ours = instantiate_ours(module);
    if matches!(ours, Err(Outcome::Limit)) {
        // wasmlet hit its fuel or depth cap in the start function; running
        // wasmi with its much larger budget would only burn time.
        stats.inconclusive += 1;
        return;
    }
    let theirs = instantiate_theirs(bytes);
    let (mut ours, mut theirs) = match (ours, theirs) {
        (Ok(o), Ok(t)) => (o, t),
        (Err(Outcome::Limit), _) | (_, Err(Outcome::Limit)) => {
            stats.inconclusive += 1;
            return;
        }
        (Err(a), Err(b)) => {
            if a != b {
                stats.record(
                    FindingKind::ExecMismatch,
                    format!("instantiation: wasmlet {a:?}, wasmi {b:?}"),
                    bytes,
                );
            } else {
                stats.compared += 1;
            }
            return;
        }
        (Ok(_), Err(b)) => {
            stats.record(
                FindingKind::ExecMismatch,
                format!("instantiation: wasmlet ok, wasmi {b:?}"),
                bytes,
            );
            return;
        }
        (Err(a), Ok(_)) => {
            stats.record(
                FindingKind::ExecMismatch,
                format!("instantiation: wasmlet {a:?}, wasmi ok"),
                bytes,
            );
            return;
        }
    };
    let names: Vec<String> = ours
        .store
        .instance_exports(ours.inst)
        .filter(|(_, e)| matches!(e, Extern::Func(_)))
        .map(|(n, _)| n.to_owned())
        .collect();
    for name in names {
        stats.calls += 1;
        let a = call_ours(&mut ours, &name);
        if a == Outcome::Limit {
            stats.inconclusive += 1;
            return;
        }
        let b = call_theirs(&mut theirs, &name);
        if b == Outcome::Limit {
            stats.inconclusive += 1;
            return;
        }
        if a != b {
            stats.record(
                FindingKind::ExecMismatch,
                format!("export `{name}`: wasmlet {a:?}, wasmi {b:?}"),
                bytes,
            );
            return;
        }
    }
    let (sa, sb) = (state_ours(&ours), state_theirs(&theirs));
    if sa != sb {
        let diff: Vec<String> = sa
            .iter()
            .zip(&sb)
            .filter(|(x, y)| x != y)
            .map(|(x, y)| format!("{} = {} vs {}", x.0, x.1, y.1))
            .take(3)
            .collect();
        stats.record(
            FindingKind::ExecMismatch,
            format!("final exported state differs: {}", diff.join("; ")),
            bytes,
        );
        return;
    }
    stats.compared += 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wat(src: &str) -> Vec<u8> {
        let buf = wast::parser::ParseBuffer::new(src).unwrap();
        let mut w = wast::parser::parse::<wast::Wat>(&buf).unwrap();
        w.encode().unwrap()
    }

    #[test]
    fn agreeing_engines_compare_clean() {
        let bytes = wat(r#"(module
            (memory (export "m") 1 1)
            (global (export "g") (mut i32) (i32.const 0))
            (table (export "t") 2 2 funcref)
            (func (export "a") (result i32 f64) (global.set 0 (i32.const 9))
              (i32.store (i32.const 4) (i32.const 77)) i32.const 5 f64.const 1.5)
            (func (export "b") (i32.div_s (i32.const 1) (i32.const 0)) drop))"#);
        let mut s = Stats::default();
        compare(&bytes, &mut s);
        assert!(s.findings.is_empty(), "{:?}", s.findings);
        assert_eq!((s.compared, s.calls, s.accepted), (1, 2, 1));
    }

    #[test]
    fn limits_are_inconclusive() {
        let bytes = wat(r#"(module (func (export "spin") (loop (br 0))))"#);
        let mut s = Stats::default();
        compare(&bytes, &mut s);
        assert!(s.findings.is_empty());
        assert_eq!(s.inconclusive, 1);
    }

    #[test]
    fn start_traps_compare_by_category() {
        let bytes = wat(r#"(module (func unreachable) (start 0))"#);
        let mut s = Stats::default();
        compare(&bytes, &mut s);
        assert!(s.findings.is_empty(), "{:?}", s.findings);
        assert_eq!(s.compared, 1);
    }

    #[test]
    fn trap_categories_line_up() {
        let pairs = [
            (Trap::Unreachable, TrapCode::UnreachableCodeReached),
            (Trap::MemoryOutOfBounds, TrapCode::MemoryOutOfBounds),
            (Trap::UndefinedElement, TrapCode::TableOutOfBounds),
            (Trap::UninitializedElement, TrapCode::IndirectCallToNull),
            (Trap::IndirectCallTypeMismatch, TrapCode::BadSignature),
            (Trap::IntegerDivideByZero, TrapCode::IntegerDivisionByZero),
            (Trap::IntegerOverflow, TrapCode::IntegerOverflow),
            (Trap::InvalidConversion, TrapCode::BadConversionToInteger),
        ];
        for (a, b) in pairs {
            assert_eq!(trap_wasmlet(&a), trap_wasmi(&wasmi::Error::from(b)));
        }
        assert_eq!(trap_wasmlet(&Trap::OutOfFuel), Outcome::Limit);
        assert_eq!(
            trap_wasmi(&wasmi::Error::from(TrapCode::OutOfFuel)),
            Outcome::Limit
        );
        assert_eq!(
            fingerprint(&[]),
            format!("0:{:016x}", 0xcbf2_9ce4_8422_2325u64)
        );
    }
}
