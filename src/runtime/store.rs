//! The store: owner of all runtime state (functions, tables, memories,
//! globals, segments and instances), addressed by plain indices.

use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;
use std::sync::Arc;

use super::value::{from_slot, to_slot, Func, Value};
use super::Trap;
use crate::error::{Error, Result};
use crate::ir::CompiledFunc;
use crate::types::{FuncType, GlobalType, Limits, MemoryType, Mutability, RefType, TableType, ValType, MAX_PAGES, PAGE_SIZE};

/// Resource limits and execution settings.
#[derive(Debug, Clone)]
pub struct Config {
    /// Maximum nested call depth before [`Trap::CallStackExhausted`].
    pub max_call_depth: usize,
    /// Maximum value-stack size in 8-byte slots.
    pub max_value_stack: usize,
    /// Cap on any memory's size in pages (also bounds `memory.grow`).
    pub max_memory_pages: u32,
    /// Cap on any table's size in elements.
    pub max_table_elements: u32,
    /// Fuel budget: every executed branch and call costs one unit.
    /// `None` disables metering.
    pub fuel: Option<u64>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            max_call_depth: 50_000,
            max_value_stack: 4 * 1024 * 1024,
            max_memory_pages: 16_384, // 1 GiB
            max_table_elements: 10_000_000,
            fuel: None,
        }
    }
}

/// A host function: receives the calling context and arguments, returns results.
pub type HostFunc = Rc<dyn Fn(&mut Caller<'_>, &[Value]) -> Result<Vec<Value>, Trap>>;

/// Context passed to host functions.
pub struct Caller<'a> {
    pub(crate) store: &'a mut Store,
    pub(crate) instance: Option<u32>,
}

impl fmt::Debug for Caller<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Caller").field("instance", &self.instance).finish()
    }
}

impl Caller<'_> {
    /// The store.
    pub fn store(&mut self) -> &mut Store {
        self.store
    }

    /// The calling instance's memory 0, if the caller is WebAssembly code
    /// with a memory.
    pub fn memory(&mut self) -> Option<&mut Vec<u8>> {
        let inst = self.instance?;
        let addr = *self.store.instances[inst as usize].mem_addrs.first()?;
        Some(&mut self.store.memories[addr as usize].data)
    }
}

pub(crate) enum FuncInst {
    Wasm {
        ty: u32,
        instance: u32,
        code: Arc<CompiledFunc>,
    },
    Host {
        ty: u32,
        func: HostFunc,
    },
}

impl FuncInst {
    pub(crate) fn ty(&self) -> u32 {
        match self {
            FuncInst::Wasm { ty, .. } | FuncInst::Host { ty, .. } => *ty,
        }
    }
}

#[derive(Debug)]
pub(crate) struct TableInst {
    pub elem: RefType,
    pub max: Option<u32>,
    pub elems: Vec<u64>,
}

#[derive(Debug)]
pub(crate) struct MemoryInst {
    pub max: Option<u32>,
    pub data: Vec<u8>,
}

impl MemoryInst {
    pub(crate) fn pages(&self) -> u32 {
        (self.data.len() / PAGE_SIZE) as u32
    }
}

#[derive(Debug)]
pub(crate) struct GlobalInst {
    pub ty: GlobalType,
    pub value: u64,
}

/// Per-instance address maps (module index -> store address).
#[derive(Debug, Default)]
pub(crate) struct InstanceData {
    pub type_ids: Vec<u32>,
    pub func_addrs: Vec<u32>,
    pub table_addrs: Vec<u32>,
    pub mem_addrs: Vec<u32>,
    pub global_addrs: Vec<u32>,
    pub elem_addrs: Vec<u32>,
    pub data_addrs: Vec<u32>,
    pub exports: Vec<(String, Extern)>,
}

/// A handle to a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Table(pub(crate) u32);
/// A handle to a linear memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Memory(pub(crate) u32);
/// A handle to a global.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Global(pub(crate) u32);
/// A handle to a module instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Instance(pub(crate) u32);

/// Any external value that can be imported or exported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Extern {
    /// A function.
    Func(Func),
    /// A table.
    Table(Table),
    /// A memory.
    Memory(Memory),
    /// A global.
    Global(Global),
}

/// Owns all runtime objects. Handles ([`Func`], [`Memory`], ...) are only
/// meaningful for the store that created them.
pub struct Store {
    pub(crate) config: Config,
    types: Vec<FuncType>,
    type_map: HashMap<FuncType, u32>,
    pub(crate) funcs: Vec<FuncInst>,
    pub(crate) tables: Vec<TableInst>,
    pub(crate) memories: Vec<MemoryInst>,
    pub(crate) globals: Vec<GlobalInst>,
    /// Element segments; dropped segments are emptied.
    pub(crate) elems: Vec<Vec<u64>>,
    /// Data segments; dropped segments are emptied.
    pub(crate) datas: Vec<Arc<[u8]>>,
    pub(crate) instances: Vec<Arc<InstanceData>>,
    pub(crate) fuel: Option<u64>,
    /// Reusable value stack.
    pub(crate) stack: Vec<u64>,
}

impl fmt::Debug for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Store")
            .field("funcs", &self.funcs.len())
            .field("tables", &self.tables.len())
            .field("memories", &self.memories.len())
            .field("globals", &self.globals.len())
            .field("instances", &self.instances.len())
            .finish()
    }
}

impl Default for Store {
    fn default() -> Self {
        Store::new(Config::default())
    }
}

impl Store {
    /// Creates an empty store.
    pub fn new(config: Config) -> Store {
        let fuel = config.fuel;
        Store {
            config,
            types: Vec::new(),
            type_map: HashMap::new(),
            funcs: Vec::new(),
            tables: Vec::new(),
            memories: Vec::new(),
            globals: Vec::new(),
            elems: Vec::new(),
            datas: Vec::new(),
            instances: Vec::new(),
            fuel,
            stack: Vec::new(),
        }
    }

    /// The active configuration.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Sets (or disables, with `None`) the remaining fuel.
    pub fn set_fuel(&mut self, fuel: Option<u64>) {
        self.fuel = fuel;
    }

    /// Remaining fuel, if metering is enabled.
    pub fn fuel(&self) -> Option<u64> {
        self.fuel
    }

    /// Interns a function type, returning its store-wide id. Structurally
    /// equal types get the same id, so `call_indirect` checks are one compare.
    pub(crate) fn intern_type(&mut self, ty: &FuncType) -> u32 {
        if let Some(&id) = self.type_map.get(ty) {
            return id;
        }
        let id = self.types.len() as u32;
        self.types.push(ty.clone());
        self.type_map.insert(ty.clone(), id);
        id
    }

    pub(crate) fn type_by_id(&self, id: u32) -> &FuncType {
        &self.types[id as usize]
    }

    /// Defines a host function.
    pub fn func_new(
        &mut self,
        ty: FuncType,
        f: impl Fn(&mut Caller<'_>, &[Value]) -> Result<Vec<Value>, Trap> + 'static,
    ) -> Func {
        let ty = self.intern_type(&ty);
        self.funcs.push(FuncInst::Host {
            ty,
            func: Rc::new(f),
        });
        Func(self.funcs.len() as u32 - 1)
    }

    /// The signature of a function.
    pub fn func_type(&self, f: Func) -> &FuncType {
        self.type_by_id(self.funcs[f.0 as usize].ty())
    }

    /// Creates a table filled with `init`.
    pub fn table_new(&mut self, ty: TableType, init: Value) -> Result<Table> {
        if init.ty() != ValType::Ref(ty.elem) {
            return Err(Error::Usage("table initializer has the wrong type".into()));
        }
        Ok(Table(self.alloc_table(ty, to_slot(init))?))
    }

    pub(crate) fn alloc_table(&mut self, ty: TableType, init: u64) -> Result<u32> {
        if ty.limits.min > self.config.max_table_elements {
            return Err(Error::Usage(format!(
                "table of {} elements exceeds the configured limit",
                ty.limits.min
            )));
        }
        self.tables.push(TableInst {
            elem: ty.elem,
            max: ty.limits.max,
            elems: vec![init; ty.limits.min as usize],
        });
        Ok(self.tables.len() as u32 - 1)
    }

    /// Creates a zero-filled memory.
    pub fn memory_new(&mut self, ty: MemoryType) -> Result<Memory> {
        Ok(Memory(self.alloc_memory(ty)?))
    }

    pub(crate) fn alloc_memory(&mut self, ty: MemoryType) -> Result<u32> {
        if ty.limits.min > self.config.max_memory_pages {
            return Err(Error::Usage(format!(
                "memory of {} pages exceeds the configured limit of {}",
                ty.limits.min, self.config.max_memory_pages
            )));
        }
        self.memories.push(MemoryInst {
            max: ty.limits.max,
            data: vec![0; ty.limits.min as usize * PAGE_SIZE],
        });
        Ok(self.memories.len() as u32 - 1)
    }

    /// Creates a global.
    pub fn global_new(&mut self, ty: GlobalType, value: Value) -> Result<Global> {
        if value.ty() != ty.content {
            return Err(Error::Usage("global initializer has the wrong type".into()));
        }
        self.globals.push(GlobalInst {
            ty,
            value: to_slot(value),
        });
        Ok(Global(self.globals.len() as u32 - 1))
    }

    /// Reads a global.
    pub fn global_get(&self, g: Global) -> Value {
        let gi = &self.globals[g.0 as usize];
        from_slot(gi.value, gi.ty.content)
    }

    /// Writes a mutable global.
    pub fn global_set(&mut self, g: Global, v: Value) -> Result<()> {
        let gi = &mut self.globals[g.0 as usize];
        if gi.ty.mutability != Mutability::Var {
            return Err(Error::Usage("global is immutable".into()));
        }
        if v.ty() != gi.ty.content {
            return Err(Error::Usage("global value has the wrong type".into()));
        }
        gi.value = to_slot(v);
        Ok(())
    }

    /// The type of a global.
    pub fn global_type(&self, g: Global) -> GlobalType {
        self.globals[g.0 as usize].ty
    }

    /// The current bytes of a memory.
    pub fn memory_data(&self, m: Memory) -> &[u8] {
        &self.memories[m.0 as usize].data
    }

    /// Mutable access to a memory's bytes.
    pub fn memory_data_mut(&mut self, m: Memory) -> &mut [u8] {
        &mut self.memories[m.0 as usize].data
    }

    /// Current type of a memory (its size as `min`).
    pub fn memory_type(&self, m: Memory) -> MemoryType {
        let mi = &self.memories[m.0 as usize];
        MemoryType {
            limits: Limits {
                min: mi.pages(),
                max: mi.max,
            },
        }
    }

    /// Grows a memory by `delta` pages; returns the old size or `None`.
    pub fn memory_grow(&mut self, m: Memory, delta: u32) -> Option<u32> {
        grow_memory(&mut self.memories[m.0 as usize], delta, self.config.max_memory_pages)
    }

    /// Current type of a table (its size as `min`).
    pub fn table_type(&self, t: Table) -> TableType {
        let ti = &self.tables[t.0 as usize];
        TableType {
            elem: ti.elem,
            limits: Limits {
                min: ti.elems.len() as u32,
                max: ti.max,
            },
        }
    }

    /// Reads a table element.
    pub fn table_get(&self, t: Table, i: u32) -> Option<Value> {
        let ti = &self.tables[t.0 as usize];
        ti.elems
            .get(i as usize)
            .map(|&s| from_slot(s, ValType::Ref(ti.elem)))
    }

    /// Exports of an instance, in declaration order.
    pub fn instance_exports(&self, i: Instance) -> impl Iterator<Item = (&str, Extern)> + '_ {
        self.instances[i.0 as usize]
            .exports
            .iter()
            .map(|(n, e)| (n.as_str(), *e))
    }

    /// Looks up one export by name.
    pub fn instance_export(&self, i: Instance, name: &str) -> Option<Extern> {
        self.instances[i.0 as usize]
            .exports
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, e)| *e)
    }

    /// Looks up an exported function by name.
    pub fn instance_func(&self, i: Instance, name: &str) -> Result<Func> {
        match self.instance_export(i, name) {
            Some(Extern::Func(f)) => Ok(f),
            Some(_) => Err(Error::Usage(format!("export `{name}` is not a function"))),
            None => Err(Error::Usage(format!("unknown export `{name}`"))),
        }
    }

    /// Calls a function with type-checked arguments.
    pub fn invoke(&mut self, f: Func, args: &[Value]) -> Result<Vec<Value>> {
        let ty = self.func_type(f).clone();
        if args.len() != ty.params().len() || args.iter().zip(ty.params()).any(|(a, t)| a.ty() != *t) {
            return Err(Error::Usage(format!(
                "arguments do not match the function type {ty}"
            )));
        }
        let slots: Vec<u64> = args.iter().map(|&a| to_slot(a)).collect();
        let results = super::exec::invoke(self, f.0, &slots)?;
        Ok(results
            .into_iter()
            .zip(ty.results())
            .map(|(s, &t)| from_slot(s, t))
            .collect())
    }
}

/// `memory.grow` semantics: returns the previous page count, or `None` if
/// the new size would exceed the declared maximum, 4 GiB, or the configured cap.
pub(crate) fn grow_memory(m: &mut MemoryInst, delta: u32, cap: u32) -> Option<u32> {
    let old = m.pages();
    let new = u64::from(old) + u64::from(delta);
    let max = u64::from(m.max.unwrap_or(MAX_PAGES).min(cap));
    if new > max {
        return None;
    }
    let new_len = new as usize * PAGE_SIZE;
    if m.data.try_reserve_exact(new_len - m.data.len()).is_err() {
        return None;
    }
    m.data.resize(new_len, 0);
    Some(old)
}

/// `table.grow` semantics.
pub(crate) fn grow_table(t: &mut TableInst, delta: u32, init: u64, cap: u32) -> Option<u32> {
    let old = t.elems.len() as u32;
    let new = u64::from(old) + u64::from(delta);
    let max = u64::from(t.max.unwrap_or(u32::MAX).min(cap));
    if new > max {
        return None;
    }
    if t.elems.try_reserve_exact(delta as usize).is_err() {
        return None;
    }
    t.elems.resize(new as usize, init);
    Some(old)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_objects_and_type_interning() {
        let mut s = Store::default();
        let ty = FuncType::new([ValType::I32], [ValType::I32]);
        let f = s.func_new(ty.clone(), |_, args| Ok(vec![args[0]]));
        let g = s.func_new(ty.clone(), |_, _| Ok(vec![Value::I32(0)]));
        assert_eq!(s.funcs[f.0 as usize].ty(), s.funcs[g.0 as usize].ty());
        assert_eq!(s.func_type(f), &ty);
        assert_eq!(s.invoke(f, &[Value::I32(4)]).unwrap(), vec![Value::I32(4)]);
        assert!(matches!(s.invoke(f, &[]), Err(Error::Usage(_))));
        assert!(matches!(s.invoke(f, &[Value::I64(1)]), Err(Error::Usage(_))));

        let gt = GlobalType {
            content: ValType::I64,
            mutability: Mutability::Var,
        };
        let gl = s.global_new(gt, Value::I64(1)).unwrap();
        s.global_set(gl, Value::I64(9)).unwrap();
        assert_eq!(s.global_get(gl), Value::I64(9));
        assert!(s.global_set(gl, Value::I32(9)).is_err());
        assert_eq!(s.global_type(gl), gt);
        let c = s
            .global_new(
                GlobalType {
                    content: ValType::I32,
                    mutability: Mutability::Const,
                },
                Value::I32(1),
            )
            .unwrap();
        assert!(s.global_set(c, Value::I32(2)).is_err());
        assert!(s.global_new(gt, Value::I32(0)).is_err());
    }

    #[test]
    fn memory_and_table_growth_limits() {
        let mut s = Store::new(Config {
            max_memory_pages: 3,
            max_table_elements: 5,
            ..Config::default()
        });
        let m = s
            .memory_new(MemoryType {
                limits: Limits { min: 1, max: None },
            })
            .unwrap();
        assert_eq!(s.memory_grow(m, 2), Some(1));
        assert_eq!(s.memory_grow(m, 1), None);
        assert_eq!(s.memory_type(m).limits.min, 3);
        s.memory_data_mut(m)[0] = 7;
        assert_eq!(s.memory_data(m)[0], 7);
        assert!(s
            .memory_new(MemoryType {
                limits: Limits { min: 4, max: None },
            })
            .is_err());

        let tt = TableType {
            elem: RefType::Extern,
            limits: Limits { min: 2, max: Some(4) },
        };
        let t = s.table_new(tt, Value::ExternRef(Some(3))).unwrap();
        assert_eq!(s.table_get(t, 1), Some(Value::ExternRef(Some(3))));
        assert_eq!(s.table_get(t, 2), None);
        assert_eq!(grow_table(&mut s.tables[t.0 as usize], 2, 0, 5), Some(2));
        assert_eq!(grow_table(&mut s.tables[t.0 as usize], 1, 0, 5), None);
        assert_eq!(s.table_type(t).limits.min, 4);
        assert!(s.table_new(tt, Value::FuncRef(None)).is_err());
        assert!(s
            .table_new(
                TableType {
                    elem: RefType::Func,
                    limits: Limits { min: 6, max: None }
                },
                Value::FuncRef(None)
            )
            .is_err());
        assert!(format!("{s:?}").contains("memories: 1"));
    }
}
