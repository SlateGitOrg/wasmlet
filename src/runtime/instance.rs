//! Module instantiation (spec section 4.5.4).

use std::sync::Arc;

use super::exec;
use super::store::{Extern, FuncInst, Global, GlobalInst, Instance, InstanceData, Memory, Store, Table};
use super::value::Func;
use crate::ast::{ConstExpr, DataMode, ElemMode, ImportDesc, Instr};
use crate::error::{Error, LinkError, Result};
use crate::module::Module;
use crate::types::ExternKind;

fn link_err<T>(msg: String) -> Result<T> {
    Err(Error::Link(LinkError { message: msg }))
}

/// Evaluates a validated constant expression to a slot value.
fn eval_const(store: &Store, inst: &InstanceData, expr: &ConstExpr) -> u64 {
    let mut value = 0;
    for i in &expr.0 {
        value = match i {
            Instr::I32Const(v) => u64::from(*v as u32),
            Instr::I64Const(v) => *v as u64,
            Instr::F32Const(b) => u64::from(*b),
            Instr::F64Const(b) => *b,
            Instr::RefNull(_) => 0,
            Instr::RefFunc(f) => u64::from(inst.func_addrs[*f as usize]) + 1,
            Instr::GlobalGet(g) => store.globals[inst.global_addrs[*g as usize] as usize].value,
            other => unreachable!("validator admitted non-constant {other:?}"),
        };
    }
    value
}

impl Store {
    /// Instantiates `module` with `imports` given in the module's import
    /// order. On a trap during segment initialisation or the start function
    /// the error is returned, but (as the spec requires) effects on imported
    /// objects persist.
    pub fn instantiate(&mut self, module: &Module, imports: &[Extern]) -> Result<Instance> {
        if imports.len() != module.imports.len() {
            return link_err(format!(
                "expected {} imports, got {}",
                module.imports.len(),
                imports.len()
            ));
        }
        let mut inst = InstanceData {
            type_ids: module.types.iter().map(|t| self.intern_type(t)).collect(),
            ..InstanceData::default()
        };

        for (imp, ext) in module.imports.iter().zip(imports) {
            let incompatible = || {
                link_err(format!(
                    "incompatible import type for `{}`.`{}`",
                    imp.module, imp.name
                ))
            };
            match (&imp.desc, *ext) {
                (ImportDesc::Func(t), Extern::Func(f)) => {
                    if self.func_type(f) != &module.types[*t as usize] {
                        return incompatible();
                    }
                    inst.func_addrs.push(f.0);
                }
                (ImportDesc::Table(tt), Extern::Table(t)) => {
                    let actual = self.table_type(t);
                    if actual.elem != tt.elem || !actual.limits.matches(&tt.limits) {
                        return incompatible();
                    }
                    inst.table_addrs.push(t.0);
                }
                (ImportDesc::Memory(mt), Extern::Memory(m)) => {
                    if !self.memory_type(m).limits.matches(&mt.limits) {
                        return incompatible();
                    }
                    inst.mem_addrs.push(m.0);
                }
                (ImportDesc::Global(gt), Extern::Global(g)) => {
                    if self.global_type(g) != *gt {
                        return incompatible();
                    }
                    inst.global_addrs.push(g.0);
                }
                _ => return incompatible(),
            }
        }

        let instance_idx = self.instances.len() as u32;
        for (i, code) in module.funcs.iter().enumerate() {
            let ty = inst.type_ids[module.func_types[module.imported_funcs() + i] as usize];
            self.funcs.push(FuncInst::Wasm {
                ty,
                instance: instance_idx,
                code: Arc::clone(code),
            });
            inst.func_addrs.push(self.funcs.len() as u32 - 1);
        }
        for tt in &module.tables {
            inst.table_addrs.push(self.alloc_table(*tt, 0)?);
        }
        for mt in &module.memories {
            inst.mem_addrs.push(self.alloc_memory(*mt)?);
        }
        for g in &module.globals {
            let value = eval_const(self, &inst, &g.init);
            self.globals.push(GlobalInst { ty: g.ty, value });
            inst.global_addrs.push(self.globals.len() as u32 - 1);
        }
        for e in &module.elems {
            let refs = e.init.iter().map(|x| eval_const(self, &inst, x)).collect();
            self.elems.push(refs);
            inst.elem_addrs.push(self.elems.len() as u32 - 1);
        }
        for d in &module.datas {
            self.datas.push(d.init.as_slice().into());
            inst.data_addrs.push(self.datas.len() as u32 - 1);
        }
        for ex in &module.exports {
            let i = ex.index as usize;
            let e = match ex.kind {
                ExternKind::Func => Extern::Func(Func(inst.func_addrs[i])),
                ExternKind::Table => Extern::Table(Table(inst.table_addrs[i])),
                ExternKind::Memory => Extern::Memory(Memory(inst.mem_addrs[i])),
                ExternKind::Global => Extern::Global(Global(inst.global_addrs[i])),
            };
            inst.exports.push((ex.name.clone(), e));
        }

        let inst = Arc::new(inst);
        self.instances.push(Arc::clone(&inst));
        log::debug!(
            "instance {instance_idx}: {} funcs, {} tables, {} memories, {} globals",
            inst.func_addrs.len(),
            inst.table_addrs.len(),
            inst.mem_addrs.len(),
            inst.global_addrs.len()
        );

        // Active and declarative element segments, then data segments.
        for (i, e) in module.elems.iter().enumerate() {
            let seg = inst.elem_addrs[i] as usize;
            match &e.mode {
                ElemMode::Active { table, offset } => {
                    let d = eval_const(self, &inst, offset) as u32;
                    let n = self.elems[seg].len() as u32;
                    exec::table_init(self, inst.table_addrs[*table as usize], seg, d, 0, n)?;
                    self.elems[seg] = Vec::new();
                }
                ElemMode::Declarative => self.elems[seg] = Vec::new(),
                ElemMode::Passive => {}
            }
        }
        for (i, d) in module.datas.iter().enumerate() {
            if let DataMode::Active { memory, offset } = &d.mode {
                let seg = inst.data_addrs[i] as usize;
                let at = eval_const(self, &inst, offset) as u32;
                let n = self.datas[seg].len() as u32;
                exec::memory_init(self, inst.mem_addrs[*memory as usize], seg, at, 0, n)?;
                self.datas[seg] = Arc::from([]);
            }
        }
        if let Some(s) = module.start {
            exec::invoke(self, inst.func_addrs[s as usize], &[]).map_err(Error::Trap)?;
        }
        Ok(Instance(instance_idx))
    }
}

