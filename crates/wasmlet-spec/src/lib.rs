//! A `.wast` script runner for wasmlet.
//!
//! The `wast` crate parses scripts and encodes text modules to binary; all
//! decoding, validation, linking and execution is done by wasmlet. Each
//! directive is one *command* and is scored as passed, failed or skipped:
//!
//! * `module`, `register`, `invoke`, `assert_return`, `assert_trap`,
//!   `assert_exhaustion`, `assert_invalid`, `assert_unlinkable` and binary
//!   `assert_malformed` are executed.
//! * `assert_malformed` on a *text* (`quote`) module is **skipped**: it tests
//!   the text parser, which is not part of wasmlet.
//! * Proposals outside WebAssembly 2.0 (threads, exceptions, components) are
//!   skipped.
//!
//! Trap assertions require the trap message to match the expected text
//! (prefix match in either direction, as the reference interpreter allows
//! messages such as `"uninitialized element 2"`). Invalid/malformed/
//! unlinkable assertions require rejection at the right stage but do not
//! compare messages; the message agreement rate is reported separately.

use std::collections::HashMap;
use std::fmt;

use wasmlet::types::{
    FuncType, GlobalType, Limits, MemoryType, Mutability, RefType, TableType, ValType,
};
use wasmlet::{Error, Extern, Instance, Linker, Module, Store, Trap, Value};
use wast::core::{AbstractHeapType, HeapType, NanPattern, WastArgCore, WastRetCore};
use wast::lexer::Lexer;
use wast::parser::{self, ParseBuffer};
use wast::{QuoteWatTest, Wast, WastArg, WastDirective, WastExecute, WastInvoke, WastRet};

/// Outcome of one directive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Behaved as asserted.
    Pass,
    /// Did not behave as asserted.
    Fail(String),
    /// Not applicable to wasmlet (with reason).
    Skip(&'static str),
}

/// A failed directive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// 1-based line of the directive.
    pub line: usize,
    /// What went wrong.
    pub message: String,
}

/// Results for one script.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileReport {
    /// Script name.
    pub name: String,
    /// Directives that passed.
    pub passed: usize,
    /// Directives that failed.
    pub failed: usize,
    /// Directives skipped as out of scope.
    pub skipped: usize,
    /// Rejection assertions whose error text matched the expected text.
    pub messages_matched: usize,
    /// Rejection assertions checked for message agreement.
    pub messages_checked: usize,
    /// Details of each failure.
    pub failures: Vec<Failure>,
    /// Rejections whose message differs: `(expected, actual)`.
    pub message_mismatches: Vec<(String, String)>,
    /// Skipped directives by reason.
    pub skip_reasons: std::collections::BTreeMap<&'static str, usize>,
    /// Set if the script itself could not be parsed.
    pub parse_error: Option<String>,
}

impl FileReport {
    /// Directives that were executed (passed + failed).
    pub fn executed(&self) -> usize {
        self.passed + self.failed
    }
}

impl fmt::Display for FileReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:<28} {:>6}/{:<6} skipped {:>4}",
            self.name,
            self.passed,
            self.executed(),
            self.skipped
        )?;
        if let Some(e) = &self.parse_error {
            write!(f, "  PARSE ERROR: {e}")?;
        }
        Ok(())
    }
}

fn messages_agree(expected: &str, actual: &str) -> bool {
    actual.starts_with(expected) || expected.starts_with(actual)
}

/// Interpreter state for one script.
struct Runner {
    store: Store,
    linker: Linker,
    named: HashMap<String, Instance>,
    current: Option<Instance>,
    report: FileReport,
}

impl Runner {
    fn new(name: &str) -> Runner {
        let mut store = Store::default();
        let mut linker = Linker::new();
        define_spectest(&mut linker, &mut store);
        Runner {
            store,
            linker,
            named: HashMap::new(),
            current: None,
            report: FileReport {
                name: name.to_owned(),
                ..FileReport::default()
            },
        }
    }

    fn instance(&self, id: Option<wast::token::Id<'_>>) -> Result<Instance, String> {
        match id {
            Some(id) => self
                .named
                .get(id.name())
                .copied()
                .ok_or_else(|| format!("unknown module ${}", id.name())),
            None => self.current.ok_or_else(|| "no current module".to_owned()),
        }
    }

    fn instantiate(&mut self, bytes: &[u8]) -> Result<Instance, Error> {
        let module = Module::new(bytes)?;
        self.linker.instantiate(&mut self.store, &module)
    }

    fn invoke(&mut self, inv: &WastInvoke<'_>) -> Result<Result<Vec<Value>, Error>, String> {
        let inst = self.instance(inv.module)?;
        let f = self
            .store
            .instance_func(inst, inv.name)
            .map_err(|e| e.to_string())?;
        let args = inv
            .args
            .iter()
            .map(arg_value)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(self.store.invoke(f, &args))
    }

    fn execute(&mut self, exec: &mut WastExecute<'_>) -> Result<Result<Vec<Value>, Error>, String> {
        match exec {
            WastExecute::Invoke(inv) => self.invoke(inv),
            WastExecute::Wat(wat) => {
                let bytes = wat.encode().map_err(|e| e.to_string())?;
                Ok(self.instantiate(&bytes).map(|_| Vec::new()))
            }
            WastExecute::Get { module, global, .. } => {
                let inst = self.instance(*module)?;
                match self.store.instance_export(inst, global) {
                    Some(Extern::Global(g)) => Ok(Ok(vec![self.store.global_get(g)])),
                    _ => Err(format!("no global export `{global}`")),
                }
            }
        }
    }

    fn check_message(&mut self, expected: &str, actual: &str) {
        self.report.messages_checked += 1;
        if messages_agree(expected, actual) {
            self.report.messages_matched += 1;
        } else {
            self.report
                .message_mismatches
                .push((expected.to_owned(), actual.to_owned()));
        }
    }

    fn directive(&mut self, d: WastDirective<'_>) -> Outcome {
        match d {
            WastDirective::Module(mut q) | WastDirective::ModuleDefinition(mut q) => {
                let name = q.name().map(|n| n.name().to_owned());
                let bytes = match q.encode() {
                    Ok(b) => b,
                    Err(_) => return Outcome::Skip("wast could not encode module"),
                };
                match self.instantiate(&bytes) {
                    Ok(inst) => {
                        self.current = Some(inst);
                        if let Some(n) = name {
                            self.named.insert(n, inst);
                        }
                        Outcome::Pass
                    }
                    Err(e) => Outcome::Fail(format!("module failed: {e}")),
                }
            }
            WastDirective::ModuleInstance { .. } => Outcome::Skip("module instances (wasm 3.0)"),
            WastDirective::Register { name, module, .. } => match self.instance(module) {
                Ok(inst) => {
                    self.linker.instance(&self.store, name, inst);
                    Outcome::Pass
                }
                Err(e) => Outcome::Fail(e),
            },
            WastDirective::Invoke(inv) => match self.invoke(&inv) {
                Ok(Ok(_)) => Outcome::Pass,
                Ok(Err(e)) => Outcome::Fail(format!("invoke failed: {e}")),
                Err(e) => Outcome::Fail(e),
            },
            WastDirective::AssertReturn {
                mut exec, results, ..
            } => match self.execute(&mut exec) {
                Ok(Ok(values)) => match_results(&values, &results),
                Ok(Err(e)) => Outcome::Fail(format!("expected results, got {e}")),
                Err(e) => Outcome::Fail(e),
            },
            WastDirective::AssertTrap {
                mut exec, message, ..
            } => match self.execute(&mut exec) {
                Ok(Err(Error::Trap(t))) => {
                    let actual = t.to_string();
                    if messages_agree(message, &actual) {
                        Outcome::Pass
                    } else {
                        Outcome::Fail(format!("expected trap `{message}`, got trap `{actual}`"))
                    }
                }
                Ok(Err(e)) => Outcome::Fail(format!("expected trap `{message}`, got {e}")),
                Ok(Ok(v)) => Outcome::Fail(format!("expected trap `{message}`, got {v:?}")),
                Err(e) => Outcome::Fail(e),
            },
            WastDirective::AssertExhaustion { call, message, .. } => match self.invoke(&call) {
                Ok(Err(Error::Trap(Trap::CallStackExhausted))) => Outcome::Pass,
                Ok(other) => Outcome::Fail(format!("expected `{message}`, got {other:?}")),
                Err(e) => Outcome::Fail(e),
            },
            WastDirective::AssertInvalid {
                mut module,
                message,
                ..
            } => {
                let bytes = match module.encode() {
                    Ok(b) => b,
                    Err(_) => return Outcome::Skip("wast could not encode module"),
                };
                match Module::new(&bytes) {
                    Ok(_) => Outcome::Fail(format!("expected invalid module ({message})")),
                    Err(Error::Validation(e)) => {
                        self.check_message(message, &e.message);
                        Outcome::Pass
                    }
                    Err(e) => {
                        self.check_message(message, &e.to_string());
                        Outcome::Fail(format!("expected a validation error ({message}), got {e}"))
                    }
                }
            }
            WastDirective::AssertMalformed {
                mut module,
                message,
                ..
            } => {
                let bytes = match module.to_test() {
                    Ok(QuoteWatTest::Binary(b)) => b,
                    Ok(QuoteWatTest::Text(_)) => {
                        return Outcome::Skip("text-format malformed module")
                    }
                    Err(_) => return Outcome::Skip("wast could not encode module"),
                };
                match Module::new(&bytes) {
                    Ok(_) => Outcome::Fail(format!("expected malformed module ({message})")),
                    Err(Error::Decode(e)) => {
                        self.check_message(message, &e.message);
                        Outcome::Pass
                    }
                    Err(e) => {
                        // Rejected, but by the validator: still a rejection,
                        // yet the spec classifies it as a decoding error.
                        Outcome::Fail(format!("expected a decode error ({message}), got {e}"))
                    }
                }
            }
            WastDirective::AssertUnlinkable {
                mut module,
                message,
                ..
            } => {
                let bytes = match module.encode() {
                    Ok(b) => b,
                    Err(_) => return Outcome::Skip("wast could not encode module"),
                };
                let module = match Module::new(&bytes) {
                    Ok(m) => m,
                    Err(e) => return Outcome::Fail(format!("module should validate: {e}")),
                };
                match self.linker.instantiate(&mut self.store, &module) {
                    Err(Error::Link(e)) => {
                        self.check_message(message, &e.message);
                        Outcome::Pass
                    }
                    Err(e) => Outcome::Fail(format!("expected link error ({message}), got {e}")),
                    Ok(_) => Outcome::Fail(format!("expected link error ({message})")),
                }
            }
            WastDirective::AssertInvalidCustom { .. }
            | WastDirective::AssertMalformedCustom { .. } => {
                Outcome::Skip("custom-section assertions")
            }
            WastDirective::AssertException { .. } => Outcome::Skip("exception handling proposal"),
            WastDirective::AssertSuspension { .. } => Outcome::Skip("stack switching proposal"),
            WastDirective::Thread(_) | WastDirective::Wait { .. } => {
                Outcome::Skip("threads proposal")
            }
        }
    }
}

fn arg_value(a: &WastArg<'_>) -> Result<Value, String> {
    let WastArg::Core(c) = a else {
        return Err("component-model argument".into());
    };
    Ok(match c {
        WastArgCore::I32(v) => Value::I32(*v),
        WastArgCore::I64(v) => Value::I64(*v),
        WastArgCore::F32(v) => Value::F32(f32::from_bits(v.bits)),
        WastArgCore::F64(v) => Value::F64(f64::from_bits(v.bits)),
        WastArgCore::RefNull(HeapType::Abstract {
            ty: AbstractHeapType::Func,
            ..
        }) => Value::FuncRef(None),
        WastArgCore::RefNull(HeapType::Abstract {
            ty: AbstractHeapType::Extern,
            ..
        }) => Value::ExternRef(None),
        WastArgCore::RefExtern(n) => Value::ExternRef(Some(*n)),
        other => return Err(format!("unsupported argument {other:?}")),
    })
}

fn match_one(v: &Value, expected: &WastRetCore<'_>) -> bool {
    match (expected, v) {
        (WastRetCore::I32(e), Value::I32(a)) => e == a,
        (WastRetCore::I64(e), Value::I64(a)) => e == a,
        (WastRetCore::F32(p), Value::F32(a)) => {
            let bits = a.to_bits();
            match p {
                NanPattern::CanonicalNan => bits & 0x7fff_ffff == 0x7fc0_0000,
                NanPattern::ArithmeticNan => a.is_nan() && bits & 0x0040_0000 != 0,
                NanPattern::Value(e) => e.bits == bits,
            }
        }
        (WastRetCore::F64(p), Value::F64(a)) => {
            let bits = a.to_bits();
            match p {
                NanPattern::CanonicalNan => bits & 0x7fff_ffff_ffff_ffff == 0x7ff8_0000_0000_0000,
                NanPattern::ArithmeticNan => a.is_nan() && bits & 0x0008_0000_0000_0000 != 0,
                NanPattern::Value(e) => e.bits == bits,
            }
        }
        (WastRetCore::RefNull(ht), Value::FuncRef(None)) => matches!(
            ht,
            None | Some(HeapType::Abstract {
                ty: AbstractHeapType::Func,
                ..
            })
        ),
        (WastRetCore::RefNull(ht), Value::ExternRef(None)) => matches!(
            ht,
            None | Some(HeapType::Abstract {
                ty: AbstractHeapType::Extern,
                ..
            })
        ),
        (WastRetCore::RefExtern(e), Value::ExternRef(Some(a))) => e.is_none_or(|e| e == *a),
        (WastRetCore::RefFunc(_), Value::FuncRef(Some(_))) => true,
        (WastRetCore::Either(alts), v) => alts.iter().any(|alt| match_one(v, alt)),
        _ => false,
    }
}

fn match_results(values: &[Value], expected: &[WastRet<'_>]) -> Outcome {
    if values.len() != expected.len() {
        return Outcome::Fail(format!(
            "expected {} results, got {values:?}",
            expected.len()
        ));
    }
    for (v, e) in values.iter().zip(expected) {
        let WastRet::Core(e) = e else {
            return Outcome::Skip("component-model result");
        };
        if !match_one(v, e) {
            return Outcome::Fail(format!("expected {e:?}, got {v:?}"));
        }
    }
    Outcome::Pass
}

/// Defines the `spectest` host module the test suite imports from.
fn define_spectest(linker: &mut Linker, store: &mut Store) {
    use ValType::*;
    let prints: [(&str, &[ValType]); 7] = [
        ("print", &[]),
        ("print_i32", &[I32]),
        ("print_i64", &[I64]),
        ("print_f32", &[F32]),
        ("print_f64", &[F64]),
        ("print_i32_f32", &[I32, F32]),
        ("print_f64_f64", &[F64, F64]),
    ];
    for (name, params) in prints {
        linker.func(
            store,
            "spectest",
            name,
            FuncType::new(params.iter().copied(), []),
            |_, _| Ok(Vec::new()),
        );
    }
    let globals = [
        ("global_i32", Value::I32(666)),
        ("global_i64", Value::I64(666)),
        ("global_f32", Value::F32(666.6)),
        ("global_f64", Value::F64(666.6)),
    ];
    for (name, v) in globals {
        let ty = GlobalType {
            content: v.ty(),
            mutability: Mutability::Const,
        };
        let g = store.global_new(ty, v).expect("typed initializer");
        linker.define("spectest", name, Extern::Global(g));
    }
    let table = store
        .table_new(
            TableType {
                elem: RefType::Func,
                limits: Limits {
                    min: 10,
                    max: Some(20),
                },
            },
            Value::FuncRef(None),
        )
        .expect("small table");
    linker.define("spectest", "table", Extern::Table(table));
    let memory = store
        .memory_new(MemoryType {
            limits: Limits {
                min: 1,
                max: Some(2),
            },
        })
        .expect("small memory");
    linker.define("spectest", "memory", Extern::Memory(memory));
}

/// Runs one script given its source text.
pub fn run_script(name: &str, source: &str) -> FileReport {
    let mut runner = Runner::new(name);
    let mut lexer = Lexer::new(source);
    // names.wast deliberately uses bidi-control characters in export names.
    lexer.allow_confusing_unicode(true);
    let buf = match ParseBuffer::new_with_lexer(lexer) {
        Ok(b) => b,
        Err(e) => {
            runner.report.parse_error = Some(e.to_string());
            return runner.report;
        }
    };
    let wast: Wast<'_> = match parser::parse(&buf) {
        Ok(w) => w,
        Err(e) => {
            runner.report.parse_error = Some(e.to_string());
            return runner.report;
        }
    };
    for d in wast.directives {
        let (line, _) = d.span().linecol_in(source);
        match runner.directive(d) {
            Outcome::Pass => runner.report.passed += 1,
            Outcome::Skip(reason) => {
                runner.report.skipped += 1;
                *runner.report.skip_reasons.entry(reason).or_default() += 1;
            }
            Outcome::Fail(message) => {
                runner.report.failed += 1;
                runner.report.failures.push(Failure {
                    line: line + 1,
                    message,
                });
            }
        }
    }
    runner.report
}

/// Runs one script file.
pub fn run_file(path: &std::path::Path) -> std::io::Result<FileReport> {
    let source = std::fs::read_to_string(path)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(run_script(&name, &source))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_script() {
        let src = r#"
            (module
              (func (export "add") (param i32 i32) (result i32) local.get 0 local.get 1 i32.add)
              (func (export "div") (param i32 i32) (result i32) local.get 0 local.get 1 i32.div_s)
              (func (export "nan") (result f32) f32.const nan)
              (global (export "g") i32 (i32.const 7)))
            (assert_return (invoke "add" (i32.const 1) (i32.const 2)) (i32.const 3))
            (assert_return (invoke "add" (i32.const 1) (i32.const 2)) (i32.const 4))
            (assert_trap (invoke "div" (i32.const 1) (i32.const 0)) "integer divide by zero")
            (assert_return (invoke "nan") (f32.const nan:canonical))
            (assert_return (get "g") (i32.const 7))
            (assert_invalid (module (func (result i32))) "type mismatch")
            (assert_malformed (module binary "\00asm\02\00\00\00") "unknown binary version")
            (assert_malformed (module quote "(func") "unexpected token")
            (assert_unlinkable (module (import "nope" "f" (func))) "unknown import")
            (register "M")
            (module (import "M" "add" (func (param i32 i32) (result i32))))
        "#;
        let r = run_script("t.wast", src);
        assert_eq!(r.failed, 1, "{:?}", r.failures);
        assert_eq!(r.failures[0].line, 8);
        assert_eq!(r.skipped, 1);
        assert_eq!(r.passed, 10);
        assert!(r.to_string().starts_with("t.wast"));
        assert_eq!(r.messages_checked, 3);
    }

    #[test]
    fn parse_errors_are_reported() {
        let r = run_script("bad.wast", "(module");
        assert!(r.parse_error.is_some());
        assert!(r.to_string().contains("PARSE ERROR"));
    }
}
