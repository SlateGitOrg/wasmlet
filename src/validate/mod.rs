//! Module validation (spec section 3) and lowering to IR.
//!
//! Module-level rules live here; per-function type checking and code
//! generation live in [`func`].

mod func;

use std::collections::HashSet;
use std::sync::Arc;

use crate::ast::{ConstExpr, DataMode, ElemMode, ImportDesc, Instr, RawModule};
use crate::error::ValidationError;
use crate::module::Module;
use crate::types::{
    ExternKind, FuncType, GlobalType, Limits, MemoryType, Mutability, TableType, ValType, MAX_PAGES,
};

type VResult<T> = Result<T, ValidationError>;

fn err<T>(msg: impl Into<String>) -> VResult<T> {
    Err(ValidationError::new(msg))
}

/// The validation context `C` of the specification.
#[derive(Debug, Default)]
pub(crate) struct Context {
    pub types: Vec<FuncType>,
    /// Type index of every function (imports first).
    pub funcs: Vec<u32>,
    pub tables: Vec<TableType>,
    pub mems: Vec<MemoryType>,
    pub globals: Vec<GlobalType>,
    pub imported_globals: usize,
    pub elems: Vec<crate::types::RefType>,
    pub data_count: Option<u32>,
    /// Functions that may be referenced by `ref.func` inside bodies.
    pub refs: HashSet<u32>,
}

impl Context {
    pub(crate) fn func_type(&self, idx: u32) -> VResult<&FuncType> {
        let ty = *self
            .funcs
            .get(idx as usize)
            .ok_or_else(|| ValidationError::new(format!("unknown function {idx}")))?;
        self.type_at(ty)
    }

    pub(crate) fn type_at(&self, idx: u32) -> VResult<&FuncType> {
        self.types
            .get(idx as usize)
            .ok_or_else(|| ValidationError::new(format!("unknown type {idx}")))
    }
}

fn check_table_limits(l: &Limits) -> VResult<()> {
    if let Some(max) = l.max {
        if l.min > max {
            return err("size minimum must not be greater than maximum");
        }
    }
    Ok(())
}

fn check_memory_type(m: &MemoryType) -> VResult<()> {
    let l = &m.limits;
    if l.min > MAX_PAGES || l.max.is_some_and(|m| m > MAX_PAGES) {
        return err("memory size must be at most 65536 pages (4GiB)");
    }
    check_table_limits(l)
}

/// Validates a constant expression producing `expected`.
fn check_const_expr(c: &Context, expr: &ConstExpr, expected: ValType) -> VResult<()> {
    let mut stack: Vec<ValType> = Vec::new();
    for i in &expr.0 {
        let t = match i {
            Instr::I32Const(_) => ValType::I32,
            Instr::I64Const(_) => ValType::I64,
            Instr::F32Const(_) => ValType::F32,
            Instr::F64Const(_) => ValType::F64,
            Instr::RefNull(r) => ValType::Ref(*r),
            Instr::RefFunc(f) => {
                c.func_type(*f)?;
                ValType::FUNCREF
            }
            Instr::GlobalGet(g) => {
                if *g as usize >= c.imported_globals {
                    return err(format!("unknown global {g}"));
                }
                let gt = c.globals[*g as usize];
                if gt.mutability == Mutability::Var {
                    return err("constant expression required");
                }
                gt.content
            }
            _ => return err("constant expression required"),
        };
        stack.push(t);
    }
    if stack != [expected] {
        return err("type mismatch");
    }
    Ok(())
}

fn collect_refs(expr: &ConstExpr, refs: &mut HashSet<u32>) {
    for i in &expr.0 {
        if let Instr::RefFunc(f) = i {
            refs.insert(*f);
        }
    }
}

/// Validates a decoded module and compiles its function bodies.
pub(crate) fn validate(raw: RawModule) -> VResult<Module> {
    let mut c = Context {
        types: raw.types.clone(),
        data_count: raw.data_count,
        ..Context::default()
    };

    for imp in &raw.imports {
        match &imp.desc {
            ImportDesc::Func(t) => {
                c.type_at(*t)?;
                c.funcs.push(*t);
            }
            ImportDesc::Table(t) => {
                check_table_limits(&t.limits)?;
                c.tables.push(*t);
            }
            ImportDesc::Memory(m) => {
                check_memory_type(m)?;
                c.mems.push(*m);
            }
            ImportDesc::Global(g) => c.globals.push(*g),
        }
    }
    c.imported_globals = c.globals.len();

    for &t in &raw.funcs {
        c.type_at(t)?;
        c.funcs.push(t);
    }
    for t in &raw.tables {
        check_table_limits(&t.limits)?;
        c.tables.push(*t);
    }
    for m in &raw.memories {
        check_memory_type(m)?;
        c.mems.push(*m);
    }
    if c.mems.len() > 1 {
        return err("multiple memories");
    }

    // C.refs: function indices referenced outside of function bodies.
    for g in &raw.globals {
        collect_refs(&g.init, &mut c.refs);
    }
    for e in &raw.elems {
        for expr in &e.init {
            collect_refs(expr, &mut c.refs);
        }
    }
    for ex in &raw.exports {
        if ex.kind == ExternKind::Func {
            c.refs.insert(ex.index);
        }
    }

    for g in &raw.globals {
        check_const_expr(&c, &g.init, g.ty.content)?;
        c.globals.push(g.ty);
    }

    for e in &raw.elems {
        for expr in &e.init {
            check_const_expr(&c, expr, ValType::Ref(e.ty))?;
        }
        if let ElemMode::Active { table, offset } = &e.mode {
            let tt = c
                .tables
                .get(*table as usize)
                .ok_or_else(|| ValidationError::new(format!("unknown table {table}")))?;
            if tt.elem != e.ty {
                return err("type mismatch");
            }
            check_const_expr(&c, offset, ValType::I32)?;
        }
        c.elems.push(e.ty);
    }

    for d in &raw.datas {
        if let DataMode::Active { memory, offset } = &d.mode {
            if *memory as usize >= c.mems.len() {
                return err(format!("unknown memory {memory}"));
            }
            check_const_expr(&c, offset, ValType::I32)?;
        }
    }

    if let Some(s) = raw.start {
        let ft = c.func_type(s)?;
        if !ft.params().is_empty() || !ft.results().is_empty() {
            return err("start function");
        }
    }

    let mut names = HashSet::new();
    for ex in &raw.exports {
        if !names.insert(ex.name.as_str()) {
            return err("duplicate export name");
        }
        let ok = match ex.kind {
            ExternKind::Func => (ex.index as usize) < c.funcs.len(),
            ExternKind::Table => (ex.index as usize) < c.tables.len(),
            ExternKind::Memory => (ex.index as usize) < c.mems.len(),
            ExternKind::Global => (ex.index as usize) < c.globals.len(),
        };
        if !ok {
            let what = match ex.kind {
                ExternKind::Func => "function",
                ExternKind::Table => "table",
                ExternKind::Memory => "memory",
                ExternKind::Global => "global",
            };
            return err(format!("unknown {what} {}", ex.index));
        }
    }

    let imported_funcs = raw.imported(ExternKind::Func);
    let mut compiled = Vec::with_capacity(raw.code.len());
    for (i, body) in raw.code.iter().enumerate() {
        let type_idx = raw.funcs[i];
        let f = func::compile(&c, type_idx, body).map_err(|mut e| {
            e.func = Some((imported_funcs + i) as u32);
            e
        })?;
        compiled.push(Arc::new(f));
    }

    log::debug!(
        "validated module: {} types, {} funcs ({} imported), {} exports",
        c.types.len(),
        c.funcs.len(),
        imported_funcs,
        raw.exports.len()
    );

    Ok(Module {
        types: raw.types,
        func_types: c.funcs,
        imports: raw.imports,
        funcs: compiled,
        tables: raw.tables,
        memories: raw.memories,
        globals: raw.globals,
        exports: raw.exports,
        start: raw.start,
        elems: raw.elems,
        datas: raw.datas,
        customs: raw.customs,
    })
}
