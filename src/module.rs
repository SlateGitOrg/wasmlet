//! Validated, compiled modules.

use std::sync::Arc;

use crate::ast::{Custom, Data, Elem, Export, Global, Import, ImportDesc};
use crate::error::Result;
use crate::ir::CompiledFunc;
use crate::types::{ExternKind, FuncType, GlobalType, MemoryType, TableType};

/// A decoded, validated and compiled WebAssembly module.
///
/// A `Module` is immutable and can be instantiated any number of times, in
/// any number of [`crate::Store`]s.
#[derive(Debug, Clone)]
pub struct Module {
    pub(crate) types: Vec<FuncType>,
    /// Type index of every function, imports first.
    pub(crate) func_types: Vec<u32>,
    pub(crate) imports: Vec<Import>,
    pub(crate) funcs: Vec<Arc<CompiledFunc>>,
    pub(crate) tables: Vec<TableType>,
    pub(crate) memories: Vec<MemoryType>,
    pub(crate) globals: Vec<Global>,
    pub(crate) exports: Vec<Export>,
    pub(crate) start: Option<u32>,
    pub(crate) elems: Vec<Elem>,
    pub(crate) datas: Vec<Data>,
    pub(crate) customs: Vec<Custom>,
}

/// The type of an import or export.
#[derive(Debug, Clone, PartialEq)]
pub enum ExternType {
    /// A function.
    Func(FuncType),
    /// A table.
    Table(TableType),
    /// A memory.
    Memory(MemoryType),
    /// A global.
    Global(GlobalType),
}

impl ExternType {
    /// The kind of this extern.
    pub fn kind(&self) -> ExternKind {
        match self {
            ExternType::Func(_) => ExternKind::Func,
            ExternType::Table(_) => ExternKind::Table,
            ExternType::Memory(_) => ExternKind::Memory,
            ExternType::Global(_) => ExternKind::Global,
        }
    }
}

/// Compilation settings for [`Module::with_options`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompileOptions {
    /// Fuse frequent op sequences into superinstructions (see the
    /// architecture notes). On by default; turning it off is useful for
    /// measuring its effect and for differential testing.
    pub superinstructions: bool,
}

impl Default for CompileOptions {
    fn default() -> Self {
        CompileOptions {
            superinstructions: true,
        }
    }
}

impl Module {
    /// Decodes, validates and compiles a binary module with default options.
    pub fn new(bytes: &[u8]) -> Result<Module> {
        Module::with_options(bytes, CompileOptions::default())
    }

    /// Decodes, validates and compiles a binary module.
    pub fn with_options(bytes: &[u8], options: CompileOptions) -> Result<Module> {
        let raw = crate::decode::decode(bytes)?;
        Ok(crate::validate::validate(raw, options)?)
    }

    /// Imports as `(module, name, type)`.
    pub fn imports(&self) -> impl Iterator<Item = (&str, &str, ExternType)> + '_ {
        self.imports.iter().map(|i| {
            let ty = match &i.desc {
                ImportDesc::Func(t) => ExternType::Func(self.types[*t as usize].clone()),
                ImportDesc::Table(t) => ExternType::Table(*t),
                ImportDesc::Memory(m) => ExternType::Memory(*m),
                ImportDesc::Global(g) => ExternType::Global(*g),
            };
            (i.module.as_str(), i.name.as_str(), ty)
        })
    }

    /// Exports as `(name, type)`.
    pub fn exports(&self) -> impl Iterator<Item = (&str, ExternType)> + '_ {
        self.exports
            .iter()
            .map(|e| (e.name.as_str(), self.extern_type(e.kind, e.index)))
    }

    fn extern_type(&self, kind: ExternKind, index: u32) -> ExternType {
        let i = index as usize;
        let nth_import = |k: ExternKind| {
            self.imports
                .iter()
                .filter(move |imp| imp.desc.kind() == k)
                .map(|imp| &imp.desc)
        };
        match kind {
            ExternKind::Func => ExternType::Func(self.types[self.func_types[i] as usize].clone()),
            ExternKind::Table => {
                let imported: Vec<_> = nth_import(kind).collect();
                match imported.get(i) {
                    Some(ImportDesc::Table(t)) => ExternType::Table(*t),
                    _ => ExternType::Table(self.tables[i - imported.len()]),
                }
            }
            ExternKind::Memory => {
                let imported: Vec<_> = nth_import(kind).collect();
                match imported.get(i) {
                    Some(ImportDesc::Memory(m)) => ExternType::Memory(*m),
                    _ => ExternType::Memory(self.memories[i - imported.len()]),
                }
            }
            ExternKind::Global => {
                let imported: Vec<_> = nth_import(kind).collect();
                match imported.get(i) {
                    Some(ImportDesc::Global(g)) => ExternType::Global(*g),
                    _ => ExternType::Global(self.globals[i - imported.len()].ty),
                }
            }
        }
    }

    /// Number of imported functions (defined function `i` has index
    /// `imported_funcs() + i`).
    pub fn imported_funcs(&self) -> usize {
        self.func_types.len() - self.funcs.len()
    }

    /// The compiled IR of every defined function, for inspection.
    pub fn compiled_funcs(&self) -> impl Iterator<Item = &CompiledFunc> + '_ {
        self.funcs.iter().map(|f| f.as_ref())
    }

    /// The function type at a type-section index.
    pub fn func_type(&self, type_idx: u32) -> Option<&FuncType> {
        self.types.get(type_idx as usize)
    }

    /// Custom sections as `(name, payload length)`.
    pub fn custom_sections(&self) -> impl Iterator<Item = (&str, usize)> + '_ {
        self.customs.iter().map(|c| (c.name.as_str(), c.len))
    }

    /// Index of the start function, if any.
    pub fn start(&self) -> Option<u32> {
        self.start
    }
}
