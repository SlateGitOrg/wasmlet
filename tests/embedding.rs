//! The embedding API end to end: module introspection, host functions,
//! linking, host-created objects, resource limits and error reporting.

use std::cell::Cell;
use std::rc::Rc;

use wasmlet::types::{
    FuncType, GlobalType, Limits, MemoryType, Mutability, RefType, TableType, ValType,
};
use wasmlet::{Config, Error, Extern, ExternType, Linker, Module, Store, Trap, Value};

fn module(src: &str) -> Module {
    Module::new(&wat::parse_str(src).unwrap()).unwrap()
}

fn i32s(v: &[Value]) -> Vec<i32> {
    v.iter().map(|x| x.as_i32().unwrap()).collect()
}

#[test]
fn module_introspection() {
    let m = module(
        r#"(module
          (import "env" "log" (func (param i32)))
          (import "env" "mem" (memory 1 2))
          (import "env" "tab" (table 1 funcref))
          (import "env" "g" (global (mut i64)))
          (table 3 externref)
          (global f32 (f32.const 1.5))
          (func $f (export "f") (param i32 i32) (result i32) (i32.add (local.get 0) (local.get 1)))
          (func $s)
          (start $s)
          (export "t0" (table 0))
          (export "t1" (table 1))
          (export "m0" (memory 0))
          (export "g0" (global 0))
          (export "g1" (global 1))
          (@custom "note" "hello"))"#,
    );
    let imports: Vec<_> = m
        .imports()
        .map(|(a, b, t)| (a.to_owned(), b.to_owned(), t))
        .collect();
    assert_eq!(imports.len(), 4);
    assert_eq!(
        imports[0].2,
        ExternType::Func(FuncType::new([ValType::I32], []))
    );
    assert!(matches!(
        imports[1].2,
        ExternType::Memory(MemoryType {
            limits: Limits {
                min: 1,
                max: Some(2)
            }
        })
    ));
    assert_eq!(imports[2].2.kind().to_string(), "table");
    assert!(matches!(
        imports[3].2,
        ExternType::Global(GlobalType {
            mutability: Mutability::Var,
            ..
        })
    ));

    let exports: Vec<_> = m.exports().map(|(n, t)| (n.to_owned(), t)).collect();
    assert_eq!(
        exports[0].1,
        ExternType::Func(FuncType::new([ValType::I32; 2], [ValType::I32]))
    );
    assert!(matches!(&exports[1].1, ExternType::Table(t) if t.elem == RefType::Func));
    assert!(
        matches!(&exports[2].1, ExternType::Table(t) if t.elem == RefType::Extern && t.limits.min == 3)
    );
    assert!(matches!(&exports[3].1, ExternType::Memory(t) if t.limits.max == Some(2)));
    assert!(matches!(&exports[4].1, ExternType::Global(g) if g.content == ValType::I64));
    assert!(matches!(&exports[5].1, ExternType::Global(g) if g.content == ValType::F32));
    for (_, t) in &exports {
        let _ = t.kind();
    }

    assert_eq!(m.imported_funcs(), 1);
    assert_eq!(m.compiled_funcs().count(), 2);
    assert_eq!(m.start(), Some(2));
    assert_eq!(m.custom_sections().next(), Some(("note", 5)));
    assert!(
        m.custom_sections().any(|(n, _)| n == "name"),
        "wat emits a name section"
    );
    let f = m.compiled_funcs().next().unwrap();
    assert_eq!((f.params, f.results), (2, 1));
    assert_eq!(m.func_type(f.type_idx).unwrap().params().len(), 2);
    assert!(m.func_type(99).is_none());
    assert!(f.to_string().contains("I32Add"));
}

#[test]
fn host_functions_see_caller_memory_and_can_reenter() {
    let m = module(
        r#"(module
          (import "host" "sum_bytes" (func $sum (param i32 i32) (result i32)))
          (import "host" "twice" (func $twice (param i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 8) "\01\02\03\04\05")
          (func (export "run") (result i32) (call $sum (i32.const 8) (i32.const 5)))
          (func (export "inc") (param i32) (result i32) (i32.add (local.get 0) (i32.const 1)))
          (func (export "via_host") (param i32) (result i32) (call $twice (local.get 0))))"#,
    );
    let mut store = Store::default();
    let mut linker = Linker::new();
    let ty = FuncType::new([ValType::I32, ValType::I32], [ValType::I32]);
    linker.func(&mut store, "host", "sum_bytes", ty, |caller, args| {
        let (p, n) = (
            args[0].as_i32().unwrap() as usize,
            args[1].as_i32().unwrap() as usize,
        );
        let mem = caller
            .memory()
            .ok_or_else(|| Trap::Host("no memory".into()))?;
        Ok(vec![Value::I32(
            mem[p..p + n].iter().map(|&b| i32::from(b)).sum(),
        )])
    });
    // `twice` calls back into the guest's `inc` twice: host -> wasm re-entry.
    let inc_slot: Rc<Cell<Option<wasmlet::Func>>> = Rc::new(Cell::new(None));
    let slot = Rc::clone(&inc_slot);
    linker.func(
        &mut store,
        "host",
        "twice",
        FuncType::new([ValType::I32], [ValType::I32]),
        move |caller, args| {
            let inc = slot.get().expect("set after instantiation");
            let once = caller
                .store()
                .invoke(inc, args)
                .map_err(|e| Trap::Host(e.to_string()))?;
            caller
                .store()
                .invoke(inc, &once)
                .map_err(|e| Trap::Host(e.to_string()))
        },
    );
    let inst = linker.instantiate(&mut store, &m).unwrap();
    inc_slot.set(Some(store.instance_func(inst, "inc").unwrap()));
    let run = store.instance_func(inst, "run").unwrap();
    assert_eq!(i32s(&store.invoke(run, &[]).unwrap()), [15]);
    let via = store.instance_func(inst, "via_host").unwrap();
    assert_eq!(i32s(&store.invoke(via, &[Value::I32(40)]).unwrap()), [42]);
    assert!(format!("{linker:?}").contains("definitions: 2"));
}

#[test]
fn host_functions_called_directly_and_with_bad_results() {
    let mut store = Store::default();
    let f = store.func_new(
        FuncType::new([ValType::I64], [ValType::I64]),
        |caller, a| {
            assert!(caller.memory().is_none(), "no wasm caller");
            Ok(vec![Value::I64(a[0].as_i64().unwrap() * 2)])
        },
    );
    assert_eq!(
        store.invoke(f, &[Value::I64(21)]).unwrap(),
        vec![Value::I64(42)]
    );
    let liar = store.func_new(FuncType::new([], [ValType::I32]), |_, _| {
        Ok(vec![Value::F32(1.0)])
    });
    match store.invoke(liar, &[]) {
        Err(Error::Trap(Trap::Host(m))) => assert!(m.contains("do not match")),
        other => panic!("{other:?}"),
    }
    // A wasm caller that receives badly typed host results traps too.
    let m = module(
        r#"(module (import "h" "f" (func $f (result i32))) (func (export "go") (result i32) call $f))"#,
    );
    let mut linker = Linker::new();
    linker.define("h", "f", Extern::Func(liar));
    let inst = linker.instantiate(&mut store, &m).unwrap();
    let go = store.instance_func(inst, "go").unwrap();
    assert!(matches!(
        store.invoke(go, &[]),
        Err(Error::Trap(Trap::Host(_)))
    ));
}

#[test]
fn linking_registered_instances_and_fallbacks() {
    let lib = module(
        r#"(module
          (memory (export "mem") 1)
          (global (export "counter") (mut i32) (i32.const 0))
          (table (export "tab") 2 funcref)
          (func (export "bump") (global.set 0 (i32.add (global.get 0) (i32.const 1)))))"#,
    );
    let app = module(
        r#"(module
          (import "lib" "bump" (func $bump))
          (import "lib" "counter" (global $c (mut i32)))
          (import "lib" "mem" (memory 1))
          (import "lib" "tab" (table 2 funcref))
          (import "extra" "seven" (func $seven (result i32)))
          (func (export "go") (result i32)
            call $bump call $bump
            (i32.store (i32.const 0) (call $seven))
            (i32.add (global.get $c) (i32.load (i32.const 0)))))"#,
    );
    let mut store = Store::default();
    let lib_inst = Linker::new().instantiate(&mut store, &lib).unwrap();
    let mut linker = Linker::new();
    linker.instance(&store, "lib", lib_inst);
    assert!(linker.get("lib", "bump").is_some());
    assert!(linker.get("lib", "nope").is_none());

    // Without the fallback, `extra.seven` is unknown.
    match linker.instantiate(&mut store, &app) {
        Err(Error::Link(e)) => assert!(e.to_string().contains("`extra`.`seven`"), "{e}"),
        other => panic!("{other:?}"),
    }
    linker.set_fallback(Box::new(|store, module, name, ty| {
        if (module, name) != ("extra", "seven") {
            return None;
        }
        let ExternType::Func(ft) = ty else {
            return None;
        };
        Some(Extern::Func(
            store.func_new(ft.clone(), |_, _| Ok(vec![Value::I32(7)])),
        ))
    }));
    let inst = linker.instantiate(&mut store, &app).unwrap();
    let go = store.instance_func(inst, "go").unwrap();
    assert_eq!(i32s(&store.invoke(go, &[]).unwrap()), [9]);
    // State is shared with the library instance.
    let Some(Extern::Global(g)) = store.instance_export(lib_inst, "counter") else {
        panic!()
    };
    assert_eq!(store.global_get(g), Value::I32(2));
    let Some(Extern::Memory(mem)) = store.instance_export(lib_inst, "mem") else {
        panic!()
    };
    assert_eq!(&store.memory_data(mem)[..4], &7i32.to_le_bytes());
    assert_eq!(store.instance_exports(lib_inst).count(), 4);
}

#[test]
fn link_type_mismatches_are_reported() {
    let m = module(r#"(module (import "e" "f" (func (param i32))))"#);
    let mut store = Store::default();
    let wrong = store.func_new(FuncType::new([ValType::I64], []), |_, _| Ok(vec![]));
    let mut linker = Linker::new();
    linker.define("e", "f", Extern::Func(wrong));
    match linker.instantiate(&mut store, &m) {
        Err(Error::Link(e)) => assert!(e.message.contains("incompatible import type"), "{e}"),
        other => panic!("{other:?}"),
    }
    let g = store
        .global_new(
            GlobalType {
                mutability: Mutability::Const,
                content: ValType::I32,
            },
            Value::I32(1),
        )
        .unwrap();
    linker.define("e", "f", Extern::Global(g));
    assert!(matches!(
        linker.instantiate(&mut store, &m),
        Err(Error::Link(_))
    ));
    assert!(matches!(store.instantiate(&m, &[]), Err(Error::Link(_))));
}

#[test]
fn host_created_objects() {
    let mut store = Store::default();
    let gt = GlobalType {
        mutability: Mutability::Var,
        content: ValType::F64,
    };
    let g = store.global_new(gt, Value::F64(2.5)).unwrap();
    assert_eq!(store.global_type(g), gt);
    store.global_set(g, Value::F64(-1.0)).unwrap();
    assert_eq!(store.global_get(g), Value::F64(-1.0));
    assert!(matches!(
        store.global_set(g, Value::I32(0)),
        Err(Error::Usage(_))
    ));
    assert!(store.global_new(gt, Value::I32(0)).is_err());
    let c = store
        .global_new(
            GlobalType {
                mutability: Mutability::Const,
                content: ValType::I32,
            },
            Value::I32(0),
        )
        .unwrap();
    assert!(
        matches!(store.global_set(c, Value::I32(1)), Err(Error::Usage(m)) if m.contains("immutable"))
    );

    let mem = store
        .memory_new(MemoryType {
            limits: Limits {
                min: 1,
                max: Some(3),
            },
        })
        .unwrap();
    assert_eq!(store.memory_grow(mem, 2), Some(1));
    assert_eq!(store.memory_grow(mem, 1), None);
    assert_eq!(
        store.memory_type(mem).limits,
        Limits {
            min: 3,
            max: Some(3)
        }
    );
    store.memory_data_mut(mem)[70_000] = 9;
    assert_eq!(store.memory_data(mem)[70_000], 9);

    let tt = TableType {
        elem: RefType::Extern,
        limits: Limits { min: 2, max: None },
    };
    let t = store.table_new(tt, Value::ExternRef(Some(5))).unwrap();
    assert_eq!(store.table_get(t, 1), Some(Value::ExternRef(Some(5))));
    assert_eq!(store.table_get(t, 2), None);
    assert_eq!(store.table_type(t).limits.min, 2);
    assert!(store.table_new(tt, Value::FuncRef(None)).is_err());

    // Host objects satisfy imports.
    let m = module(
        r#"(module (import "h" "m" (memory 1)) (import "h" "t" (table 1 externref))
            (import "h" "g" (global (mut f64)))
            (func (export "peek") (result i32 f64 externref)
              (i32.load8_u (i32.const 70000)) (global.get 0) (table.get 0 (i32.const 0))))"#,
    );
    let mut linker = Linker::new();
    linker
        .define("h", "m", Extern::Memory(mem))
        .define("h", "t", Extern::Table(t))
        .define("h", "g", Extern::Global(g));
    let inst = linker.instantiate(&mut store, &m).unwrap();
    let peek = store.instance_func(inst, "peek").unwrap();
    assert_eq!(
        store.invoke(peek, &[]).unwrap(),
        vec![Value::I32(9), Value::F64(-1.0), Value::ExternRef(Some(5))]
    );
}

#[test]
fn configured_limits_are_enforced() {
    let config = Config {
        max_memory_pages: 2,
        max_table_elements: 10,
        ..Config::default()
    };
    let mut store = Store::new(config);
    assert_eq!(store.config().max_memory_pages, 2);
    let big = module("(module (memory 3))");
    assert!(
        matches!(Linker::new().instantiate(&mut store, &big), Err(Error::Usage(m)) if m.contains("limit"))
    );
    let tab = module("(module (table 11 funcref))");
    assert!(matches!(
        Linker::new().instantiate(&mut store, &tab),
        Err(Error::Usage(_))
    ));
    let grow = module(
        r#"(module (memory 1) (table 1 funcref)
            (func (export "m") (result i32) (memory.grow (i32.const 5)))
            (func (export "t") (result i32) (table.grow (ref.null func) (i32.const 50))))"#,
    );
    let inst = Linker::new().instantiate(&mut store, &grow).unwrap();
    for name in ["m", "t"] {
        let f = store.instance_func(inst, name).unwrap();
        assert_eq!(
            i32s(&store.invoke(f, &[]).unwrap()),
            [-1],
            "{name} growth is capped"
        );
    }
}

#[test]
fn fuel_and_stack_limits_trap_cleanly() {
    let m = module(
        r#"(module
          (func (export "spin") (loop (br 0)))
          (func $r (export "recurse") (param i32) (result i32)
            (if (result i32) (i32.eqz (local.get 0)) (then (i32.const 0))
              (else (i32.add (i32.const 1) (call $r (i32.sub (local.get 0) (i32.const 1))))))))"#,
    );
    let mut store = Store::new(Config {
        fuel: Some(1_000),
        max_call_depth: 100,
        ..Config::default()
    });
    let inst = Linker::new().instantiate(&mut store, &m).unwrap();
    let spin = store.instance_func(inst, "spin").unwrap();
    assert_eq!(store.invoke(spin, &[]), Err(Error::Trap(Trap::OutOfFuel)));
    assert_eq!(store.fuel(), Some(0));
    store.set_fuel(Some(10_000));
    let rec = store.instance_func(inst, "recurse").unwrap();
    assert_eq!(
        store.invoke(rec, &[Value::I32(50)]).unwrap(),
        vec![Value::I32(50)]
    );
    assert!(store.fuel().unwrap() < 10_000);
    assert_eq!(
        store.invoke(rec, &[Value::I32(500)]),
        Err(Error::Trap(Trap::CallStackExhausted))
    );
    // The store is still usable after a trap.
    store.set_fuel(None);
    assert_eq!(
        store.invoke(rec, &[Value::I32(3)]).unwrap(),
        vec![Value::I32(3)]
    );

    let mut small = Store::new(Config {
        max_value_stack: 64,
        ..Config::default()
    });
    let inst = Linker::new().instantiate(&mut small, &m).unwrap();
    let rec = small.instance_func(inst, "recurse").unwrap();
    assert_eq!(
        small.invoke(rec, &[Value::I32(1_000)]),
        Err(Error::Trap(Trap::CallStackExhausted))
    );
}

#[test]
fn usage_errors() {
    let m = module(
        r#"(module (memory (export "mem") 1) (func (export "id") (param i32) (result i32) local.get 0))"#,
    );
    let mut store = Store::default();
    let inst = Linker::new().instantiate(&mut store, &m).unwrap();
    assert!(
        matches!(store.instance_func(inst, "mem"), Err(Error::Usage(m)) if m.contains("not a function"))
    );
    assert!(
        matches!(store.instance_func(inst, "zzz"), Err(Error::Usage(m)) if m.contains("unknown export"))
    );
    let id = store.instance_func(inst, "id").unwrap();
    assert!(matches!(store.invoke(id, &[]), Err(Error::Usage(_))));
    assert!(matches!(
        store.invoke(id, &[Value::I64(1)]),
        Err(Error::Usage(_))
    ));
    assert_eq!(store.func_type(id).to_string(), "[i32] -> [i32]");
}

#[test]
fn decode_and_validation_errors_carry_context() {
    match Module::new(b"\0asm\x02\0\0\0") {
        Err(Error::Decode(e)) => {
            assert_eq!(e.offset, 4);
            assert!(e.to_string().contains("unknown binary version"), "{e}");
        }
        other => panic!("{other:?}"),
    }
    let bad = wat::parse_str("(module (func) (func (result i32) (i64.const 0)))").unwrap();
    match Module::new(&bad) {
        Err(Error::Validation(e)) => {
            assert_eq!(e.func, Some(1));
            assert!(e.message.contains("type mismatch"), "{e}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn traps_during_instantiation_keep_earlier_effects() {
    // The first data segment lands in the imported memory before the second
    // one traps, as the spec requires.
    let mut store = Store::default();
    let mem = store
        .memory_new(MemoryType {
            limits: Limits { min: 1, max: None },
        })
        .unwrap();
    let m = module(
        r#"(module (import "h" "m" (memory 1))
            (data (i32.const 0) "ok")
            (data (i32.const 65535) "overflow"))"#,
    );
    let mut linker = Linker::new();
    linker.define("h", "m", Extern::Memory(mem));
    assert_eq!(
        linker.instantiate(&mut store, &m),
        Err(Error::Trap(Trap::MemoryOutOfBounds))
    );
    assert_eq!(&store.memory_data(mem)[..2], b"ok");
}

#[test]
fn values_display_and_accessors() {
    assert_eq!(Value::I32(-1).to_string(), "-1:i32");
    assert_eq!(Value::I64(5).to_string(), "5:i64");
    assert_eq!(Value::F32(0.5).to_string(), "0.5:f32");
    assert_eq!(Value::F64(2.0).to_string(), "2:f64");
    assert_eq!(Value::FuncRef(None).to_string(), "null:funcref");
    assert_eq!(Value::ExternRef(Some(3)).to_string(), "extern#3");
    assert_eq!(Value::I64(1).as_i32(), None);
    assert_eq!(Value::I32(1).as_i64(), None);
    assert_eq!(Value::default_for(ValType::F64), Value::F64(0.0));
    assert_eq!(
        Value::default_for(ValType::EXTERNREF),
        Value::ExternRef(None)
    );
}
