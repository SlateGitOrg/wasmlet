//! Function-body validation and IR lowering in a single pass.
//!
//! The type checker is the algorithm from the specification's validation
//! appendix (operand stack of possibly-unknown types plus a control stack).
//! While checking, it emits [`Op`]s: because the checker knows the exact
//! operand-stack height at every branch, it can precompute each branch's
//! [`DropKeep`] and resolve its target to an absolute code index.

use super::{err, Context, VResult};
use crate::ast::{FuncBody, Instr, LoadKind, MemArg, StoreKind};
use crate::error::ValidationError;
use crate::ir::{CompiledFunc, DropKeep, Op};
use crate::types::{BlockType, Mutability, RefType, ValType};

/// `None` is the spec's `Unknown` (the polymorphic bottom type).
type MaybeType = Option<ValType>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Func,
    Block,
    Loop,
    If,
    Else,
}

#[derive(Debug)]
struct Ctrl {
    kind: Kind,
    params: Vec<ValType>,
    results: Vec<ValType>,
    height: usize,
    unreachable: bool,
    /// Code index a backward branch jumps to (loops only).
    loop_pc: u32,
    /// Forward branches to patch with this frame's end.
    fixups: Vec<usize>,
    /// The `BrIfEqz` of an `if`, patched at `else` or `end`.
    else_fixup: Option<usize>,
}

impl Ctrl {
    fn label_types(&self) -> &[ValType] {
        if self.kind == Kind::Loop {
            &self.params
        } else {
            &self.results
        }
    }
}

struct Compiler<'c> {
    c: &'c Context,
    locals: Vec<ValType>,
    vals: Vec<MaybeType>,
    ctrls: Vec<Ctrl>,
    code: Vec<Op>,
    max_height: usize,
}

pub(super) fn compile(
    c: &Context,
    type_idx: u32,
    body: &FuncBody,
    superinstructions: bool,
) -> VResult<CompiledFunc> {
    let ft = c.type_at(type_idx)?;
    let mut locals: Vec<ValType> = ft.params().to_vec();
    for &(n, t) in &body.locals {
        locals.extend(std::iter::repeat_n(t, n as usize));
    }
    let mut comp = Compiler {
        c,
        locals,
        vals: Vec::new(),
        ctrls: vec![Ctrl {
            kind: Kind::Func,
            params: Vec::new(),
            results: ft.results().to_vec(),
            height: 0,
            unreachable: false,
            loop_pc: 0,
            fixups: Vec::new(),
            else_fixup: None,
        }],
        code: Vec::with_capacity(body.code.len()),
        max_height: 0,
    };
    for (i, instr) in body.code.iter().enumerate() {
        if comp.ctrls.is_empty() {
            return Err(at(
                i,
                ValidationError::new("operators remaining after end of function"),
            ));
        }
        comp.instr(instr).map_err(|e| at(i, e))?;
    }
    if !comp.ctrls.is_empty() {
        return err("unexpected end of function");
    }
    let params = ft.params().len() as u32;
    let code = if superinstructions {
        crate::fuse::fuse(comp.code)
    } else {
        comp.code
    };
    Ok(CompiledFunc {
        type_idx,
        params,
        locals: comp.locals.len() as u32 - params,
        results: ft.results().len() as u32,
        max_stack: comp.max_height as u32 + 1,
        code: code.into_boxed_slice(),
    })
}

fn at(i: usize, mut e: ValidationError) -> ValidationError {
    if e.instr.is_none() {
        e.instr = Some(i);
    }
    e
}

fn mismatch<T>() -> VResult<T> {
    err("type mismatch")
}

impl Compiler<'_> {
    // ---- operand stack -------------------------------------------------

    fn push(&mut self, t: ValType) {
        self.vals.push(Some(t));
        self.max_height = self.max_height.max(self.vals.len());
    }

    fn push_maybe(&mut self, t: MaybeType) {
        self.vals.push(t);
        self.max_height = self.max_height.max(self.vals.len());
    }

    fn pop(&mut self) -> VResult<MaybeType> {
        let frame = self
            .ctrls
            .last()
            .expect("control stack is non-empty while compiling");
        if self.vals.len() == frame.height {
            if frame.unreachable {
                return Ok(None);
            }
            return mismatch();
        }
        Ok(self.vals.pop().expect("height checked above"))
    }

    fn pop_expect(&mut self, expected: ValType) -> VResult<MaybeType> {
        let actual = self.pop()?;
        match actual {
            Some(t) if t != expected => mismatch(),
            _ => Ok(actual),
        }
    }

    fn pop_vals(&mut self, types: &[ValType]) -> VResult<()> {
        for &t in types.iter().rev() {
            self.pop_expect(t)?;
        }
        Ok(())
    }

    fn push_vals(&mut self, types: &[ValType]) {
        for &t in types {
            self.push(t);
        }
    }

    fn pop_ref(&mut self) -> VResult<MaybeType> {
        match self.pop()? {
            Some(ValType::Ref(r)) => Ok(Some(ValType::Ref(r))),
            None => Ok(None),
            Some(_) => mismatch(),
        }
    }

    // ---- control stack -------------------------------------------------

    fn push_ctrl(&mut self, kind: Kind, params: Vec<ValType>, results: Vec<ValType>) {
        let height = self.vals.len();
        self.push_vals(&params);
        self.ctrls.push(Ctrl {
            kind,
            params,
            results,
            height,
            unreachable: false,
            loop_pc: self.code.len() as u32,
            fixups: Vec::new(),
            else_fixup: None,
        });
    }

    fn pop_ctrl(&mut self) -> VResult<Ctrl> {
        let results = self
            .ctrls
            .last()
            .ok_or_else(|| ValidationError::new("unexpected end"))?
            .results
            .clone();
        self.pop_vals(&results)?;
        let frame = self.ctrls.last().expect("checked above");
        if self.vals.len() != frame.height {
            return mismatch();
        }
        Ok(self.ctrls.pop().expect("checked above"))
    }

    fn set_unreachable(&mut self) {
        let frame = self.ctrls.last_mut().expect("control stack is non-empty");
        self.vals.truncate(frame.height);
        frame.unreachable = true;
    }

    fn label(&self, depth: u32) -> VResult<usize> {
        let n = self.ctrls.len();
        if depth as usize >= n {
            return err(format!("unknown label {depth}"));
        }
        Ok(n - 1 - depth as usize)
    }

    fn block_type(&self, bt: BlockType) -> VResult<(Vec<ValType>, Vec<ValType>)> {
        Ok(match bt {
            BlockType::Empty => (Vec::new(), Vec::new()),
            BlockType::Value(t) => (Vec::new(), vec![t]),
            BlockType::Func(idx) => {
                let ft = self.c.type_at(idx)?;
                (ft.params().to_vec(), ft.results().to_vec())
            }
        })
    }

    // ---- code emission ---------------------------------------------------

    fn emit(&mut self, op: Op) -> usize {
        self.code.push(op);
        self.code.len() - 1
    }

    /// Unwinding needed to branch to frame `f` with the current stack.
    fn drop_keep(&self, f: usize) -> DropKeep {
        let frame = &self.ctrls[f];
        let keep = frame.label_types().len();
        let drop = self.vals.len().saturating_sub(frame.height + keep);
        DropKeep {
            drop: drop as u32,
            keep: keep as u32,
        }
    }

    /// Records that code index `at` must jump to frame `f`'s label and
    /// returns the target if it is already known (loops).
    fn branch_target(&mut self, f: usize, at: usize) -> u32 {
        let frame = &mut self.ctrls[f];
        if frame.kind == Kind::Loop {
            frame.loop_pc
        } else {
            frame.fixups.push(at);
            u32::MAX
        }
    }

    fn patch(&mut self, at: usize, target: u32) {
        match &mut self.code[at] {
            Op::Br { target: t, .. }
            | Op::BrIfNez { target: t, .. }
            | Op::BrIfEqz { target: t }
            | Op::BrTarget { target: t, .. } => *t = target,
            other => unreachable!("patching a non-branch op {other:?}"),
        }
    }

    fn need_memory(&self) -> VResult<()> {
        if self.c.mems.is_empty() {
            return err("unknown memory 0");
        }
        Ok(())
    }

    fn check_align(&self, m: &MemArg, natural: u32) -> VResult<()> {
        self.need_memory()?;
        if m.align > natural {
            return err("alignment must not be larger than natural");
        }
        Ok(())
    }

    fn local(&self, idx: u32) -> VResult<ValType> {
        self.locals
            .get(idx as usize)
            .copied()
            .ok_or_else(|| ValidationError::new(format!("unknown local {idx}")))
    }

    fn table(&self, idx: u32) -> VResult<RefType> {
        self.c
            .tables
            .get(idx as usize)
            .map(|t| t.elem)
            .ok_or_else(|| ValidationError::new(format!("unknown table {idx}")))
    }

    fn elem(&self, idx: u32) -> VResult<RefType> {
        self.c
            .elems
            .get(idx as usize)
            .copied()
            .ok_or_else(|| ValidationError::new(format!("unknown elem segment {idx}")))
    }

    fn data(&self, idx: u32) -> VResult<()> {
        match self.c.data_count {
            Some(n) if idx < n => Ok(()),
            _ => err(format!("unknown data segment {idx}")),
        }
    }

    fn global(&self, idx: u32) -> VResult<crate::types::GlobalType> {
        self.c
            .globals
            .get(idx as usize)
            .copied()
            .ok_or_else(|| ValidationError::new(format!("unknown global {idx}")))
    }

    fn instr(&mut self, instr: &Instr) -> VResult<()> {
        use ValType::*;
        match instr {
            Instr::Unreachable => {
                self.emit(Op::Unreachable);
                self.set_unreachable();
            }
            Instr::Nop => {}
            Instr::Block(bt) | Instr::Loop(bt) => {
                let (params, results) = self.block_type(*bt)?;
                self.pop_vals(&params)?;
                let kind = if matches!(instr, Instr::Block(_)) {
                    Kind::Block
                } else {
                    Kind::Loop
                };
                self.push_ctrl(kind, params, results);
            }
            Instr::If(bt) => {
                let (params, results) = self.block_type(*bt)?;
                self.pop_expect(I32)?;
                self.pop_vals(&params)?;
                let at = self.emit(Op::BrIfEqz { target: u32::MAX });
                self.push_ctrl(Kind::If, params, results);
                self.ctrls.last_mut().expect("just pushed").else_fixup = Some(at);
            }
            Instr::Else => {
                if self.ctrls.last().map(|f| f.kind) != Some(Kind::If) {
                    return err("else without matching if");
                }
                let frame = self.pop_ctrl()?;
                let at = self.emit(Op::Br {
                    target: u32::MAX,
                    dk: DropKeep::default(),
                });
                let else_start = self.code.len() as u32;
                if let Some(fix) = frame.else_fixup {
                    self.patch(fix, else_start);
                }
                let mut fixups = frame.fixups;
                fixups.push(at);
                self.push_ctrl(Kind::Else, frame.params, frame.results);
                self.ctrls.last_mut().expect("just pushed").fixups = fixups;
            }
            Instr::End => {
                let frame = self.pop_ctrl()?;
                if frame.kind == Kind::If && frame.params != frame.results {
                    return mismatch();
                }
                let end = if frame.kind == Kind::Func {
                    self.emit(Op::Return {
                        keep: frame.results.len() as u32,
                    }) as u32
                } else {
                    self.code.len() as u32
                };
                if let Some(fix) = frame.else_fixup {
                    self.patch(fix, end);
                }
                for fix in frame.fixups {
                    self.patch(fix, end);
                }
                if frame.kind != Kind::Func {
                    self.push_vals(&frame.results);
                }
            }
            Instr::Br(l) => {
                let f = self.label(*l)?;
                let dk = self.drop_keep(f);
                let types = self.ctrls[f].label_types().to_vec();
                self.pop_vals(&types)?;
                if self.ctrls[f].kind == Kind::Func {
                    self.emit(Op::Return { keep: dk.keep });
                } else {
                    let at = self.code.len();
                    let target = self.branch_target(f, at);
                    self.emit(Op::Br { target, dk });
                }
                self.set_unreachable();
            }
            Instr::BrIf(l) => {
                let f = self.label(*l)?;
                self.pop_expect(I32)?;
                let dk = self.drop_keep(f);
                let types = self.ctrls[f].label_types().to_vec();
                self.pop_vals(&types)?;
                self.push_vals(&types);
                let at = self.code.len();
                let target = self.branch_target(f, at);
                self.emit(Op::BrIfNez { target, dk });
            }
            Instr::BrTable(targets) => {
                self.pop_expect(I32)?;
                let (default, rest) = targets
                    .split_last()
                    .expect("decoder always stores a default");
                let df = self.label(*default)?;
                let arity = self.ctrls[df].label_types().len();
                self.emit(Op::BrTable {
                    len: rest.len() as u32,
                });
                for &l in targets.iter() {
                    let f = self.label(l)?;
                    if self.ctrls[f].label_types().len() != arity {
                        return mismatch();
                    }
                    let dk = self.drop_keep(f);
                    let at = self.code.len();
                    let target = self.branch_target(f, at);
                    self.emit(Op::BrTarget { target, dk });
                    // Check against this label's types without consuming them
                    // (spec: `push_vals(pop_vals(label_types(ctrls[n])))`).
                    let types = self.ctrls[f].label_types().to_vec();
                    let mut popped = Vec::with_capacity(types.len());
                    for &t in types.iter().rev() {
                        popped.push(self.pop_expect(t)?);
                    }
                    for t in popped.into_iter().rev() {
                        self.push_maybe(t);
                    }
                }
                let types = self.ctrls[df].label_types().to_vec();
                self.pop_vals(&types)?;
                self.set_unreachable();
            }
            Instr::Return => {
                let results = self.ctrls[0].results.clone();
                self.pop_vals(&results)?;
                self.emit(Op::Return {
                    keep: results.len() as u32,
                });
                self.set_unreachable();
            }
            Instr::Call(f) => {
                let ft = self.c.func_type(*f)?.clone();
                self.pop_vals(ft.params())?;
                self.push_vals(ft.results());
                self.emit(Op::Call(*f));
            }
            Instr::CallIndirect { type_idx, table } => {
                if self.table(*table)? != RefType::Func {
                    return mismatch();
                }
                let ft = self.c.type_at(*type_idx)?.clone();
                self.pop_expect(I32)?;
                self.pop_vals(ft.params())?;
                self.push_vals(ft.results());
                self.emit(Op::CallIndirect {
                    type_idx: *type_idx,
                    table: *table,
                });
            }
            Instr::Drop => {
                self.pop()?;
                self.emit(Op::Drop);
            }
            Instr::Select(None) => {
                self.pop_expect(I32)?;
                let t1 = self.pop()?;
                let t2 = self.pop()?;
                let num = |t: MaybeType| t.is_none_or(|t| t.is_num());
                if !(num(t1) && num(t2)) {
                    return mismatch();
                }
                if t1.is_some() && t2.is_some() && t1 != t2 {
                    return mismatch();
                }
                self.push_maybe(t1.or(t2));
                self.emit(Op::Select);
            }
            Instr::Select(Some(types)) => {
                if types.len() != 1 {
                    return err("invalid result arity");
                }
                let t = types[0];
                self.pop_expect(I32)?;
                self.pop_expect(t)?;
                self.pop_expect(t)?;
                self.push(t);
                self.emit(Op::Select);
            }
            Instr::LocalGet(x) => {
                let t = self.local(*x)?;
                self.push(t);
                self.emit(Op::LocalGet(*x));
            }
            Instr::LocalSet(x) => {
                let t = self.local(*x)?;
                self.pop_expect(t)?;
                self.emit(Op::LocalSet(*x));
            }
            Instr::LocalTee(x) => {
                let t = self.local(*x)?;
                self.pop_expect(t)?;
                self.push(t);
                self.emit(Op::LocalTee(*x));
            }
            Instr::GlobalGet(x) => {
                let g = self.global(*x)?;
                self.push(g.content);
                self.emit(Op::GlobalGet(*x));
            }
            Instr::GlobalSet(x) => {
                let g = self.global(*x)?;
                if g.mutability != Mutability::Var {
                    return err("global is immutable");
                }
                self.pop_expect(g.content)?;
                self.emit(Op::GlobalSet(*x));
            }
            Instr::TableGet(x) => {
                let t = self.table(*x)?;
                self.pop_expect(I32)?;
                self.push(Ref(t));
                self.emit(Op::TableGet(*x));
            }
            Instr::TableSet(x) => {
                let t = self.table(*x)?;
                self.pop_expect(Ref(t))?;
                self.pop_expect(I32)?;
                self.emit(Op::TableSet(*x));
            }
            Instr::TableSize(x) => {
                self.table(*x)?;
                self.push(I32);
                self.emit(Op::TableSize(*x));
            }
            Instr::TableGrow(x) => {
                let t = self.table(*x)?;
                self.pop_expect(I32)?;
                self.pop_expect(Ref(t))?;
                self.push(I32);
                self.emit(Op::TableGrow(*x));
            }
            Instr::TableFill(x) => {
                let t = self.table(*x)?;
                self.pop_expect(I32)?;
                self.pop_expect(Ref(t))?;
                self.pop_expect(I32)?;
                self.emit(Op::TableFill(*x));
            }
            Instr::TableCopy { dst, src } => {
                let d = self.table(*dst)?;
                let s = self.table(*src)?;
                if d != s {
                    return mismatch();
                }
                self.pop_vals(&[I32, I32, I32])?;
                self.emit(Op::TableCopy {
                    dst: *dst,
                    src: *src,
                });
            }
            Instr::TableInit { elem, table } => {
                let t = self.table(*table)?;
                let e = self.elem(*elem)?;
                if t != e {
                    return mismatch();
                }
                self.pop_vals(&[I32, I32, I32])?;
                self.emit(Op::TableInit {
                    elem: *elem,
                    table: *table,
                });
            }
            Instr::ElemDrop(x) => {
                self.elem(*x)?;
                self.emit(Op::ElemDrop(*x));
            }
            Instr::Load(kind, m) => {
                self.check_align(m, kind.natural_align())?;
                self.pop_expect(I32)?;
                self.push(kind.result());
                let o = m.offset;
                self.emit(match kind {
                    LoadKind::I32 | LoadKind::F32 | LoadKind::I64_32U => Op::Load32U(o),
                    LoadKind::I64 | LoadKind::F64 => Op::Load64(o),
                    LoadKind::I32_8S | LoadKind::I64_8S => Op::Load8S(o),
                    LoadKind::I32_8U | LoadKind::I64_8U => Op::Load8U(o),
                    LoadKind::I32_16S | LoadKind::I64_16S => Op::Load16S(o),
                    LoadKind::I32_16U | LoadKind::I64_16U => Op::Load16U(o),
                    LoadKind::I64_32S => Op::Load32S(o),
                });
            }
            Instr::Store(kind, m) => {
                self.check_align(m, kind.natural_align())?;
                self.pop_expect(kind.operand())?;
                self.pop_expect(I32)?;
                let o = m.offset;
                self.emit(match kind {
                    StoreKind::I32_8 | StoreKind::I64_8 => Op::Store8(o),
                    StoreKind::I32_16 | StoreKind::I64_16 => Op::Store16(o),
                    StoreKind::I32 | StoreKind::F32 | StoreKind::I64_32 => Op::Store32(o),
                    StoreKind::I64 | StoreKind::F64 => Op::Store64(o),
                });
            }
            Instr::MemorySize => {
                self.need_memory()?;
                self.push(I32);
                self.emit(Op::MemorySize);
            }
            Instr::MemoryGrow => {
                self.need_memory()?;
                self.pop_expect(I32)?;
                self.push(I32);
                self.emit(Op::MemoryGrow);
            }
            Instr::MemoryInit(x) => {
                self.need_memory()?;
                self.data(*x)?;
                self.pop_vals(&[I32, I32, I32])?;
                self.emit(Op::MemoryInit(*x));
            }
            Instr::DataDrop(x) => {
                self.data(*x)?;
                self.emit(Op::DataDrop(*x));
            }
            Instr::MemoryCopy | Instr::MemoryFill => {
                self.need_memory()?;
                self.pop_vals(&[I32, I32, I32])?;
                self.emit(if matches!(instr, Instr::MemoryCopy) {
                    Op::MemoryCopy
                } else {
                    Op::MemoryFill
                });
            }
            Instr::I32Const(v) => {
                self.push(I32);
                self.emit(Op::Const(u64::from(*v as u32)));
            }
            Instr::I64Const(v) => {
                self.push(I64);
                self.emit(Op::Const(*v as u64));
            }
            Instr::F32Const(bits) => {
                self.push(F32);
                self.emit(Op::Const(u64::from(*bits)));
            }
            Instr::F64Const(bits) => {
                self.push(F64);
                self.emit(Op::Const(*bits));
            }
            Instr::RefNull(t) => {
                self.push(Ref(*t));
                self.emit(Op::Const(0));
            }
            Instr::RefIsNull => {
                self.pop_ref()?;
                self.push(I32);
                self.emit(Op::RefIsNull);
            }
            Instr::RefFunc(x) => {
                self.c.func_type(*x)?;
                if !self.c.refs.contains(x) {
                    return err("undeclared function reference");
                }
                self.push(ValType::FUNCREF);
                self.emit(Op::RefFunc(*x));
            }
            Instr::Num(n) => {
                let (params, result) = n.signature();
                self.pop_vals(params)?;
                self.push(result);
                self.emit(Op::from_num(*n));
            }
        }
        Ok(())
    }
}
