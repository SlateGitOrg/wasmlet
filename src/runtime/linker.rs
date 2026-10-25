//! Name-based import resolution.

use std::collections::HashMap;

use super::store::{Caller, Extern, Instance, Store};
use super::{Trap, Value};
use crate::error::{Error, LinkError, Result};
use crate::module::{ExternType, Module};
use crate::types::FuncType;

/// A fallback that can synthesize an import the linker has no definition for
/// (used by the WASI layer to stub out unimplemented calls).
pub type ImportResolver = Box<dyn Fn(&mut Store, &str, &str, &ExternType) -> Option<Extern>>;

/// Maps `(module, name)` pairs to externs and instantiates modules against them.
#[derive(Default)]
pub struct Linker {
    defs: HashMap<(String, String), Extern>,
    fallback: Option<ImportResolver>,
}

impl std::fmt::Debug for Linker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Linker")
            .field("definitions", &self.defs.len())
            .field("fallback", &self.fallback.is_some())
            .finish()
    }
}

impl Linker {
    /// An empty linker.
    pub fn new() -> Linker {
        Linker::default()
    }

    /// Defines (or redefines) one import.
    pub fn define(&mut self, module: &str, name: &str, ext: Extern) -> &mut Self {
        self.defs.insert((module.to_owned(), name.to_owned()), ext);
        self
    }

    /// Defines a host function import.
    pub fn func(
        &mut self,
        store: &mut Store,
        module: &str,
        name: &str,
        ty: FuncType,
        f: impl Fn(&mut Caller<'_>, &[Value]) -> Result<Vec<Value>, Trap> + 'static,
    ) -> &mut Self {
        let func = store.func_new(ty, f);
        self.define(module, name, Extern::Func(func))
    }

    /// Makes every export of `instance` importable under module name `module`
    /// (the spec-test `register` command).
    pub fn instance(&mut self, store: &Store, module: &str, instance: Instance) -> &mut Self {
        for (name, ext) in store.instance_exports(instance) {
            self.defs.insert((module.to_owned(), name.to_owned()), ext);
        }
        self
    }

    /// Installs a fallback consulted for imports with no definition.
    pub fn set_fallback(&mut self, resolver: ImportResolver) -> &mut Self {
        self.fallback = Some(resolver);
        self
    }

    /// Looks up a definition.
    pub fn get(&self, module: &str, name: &str) -> Option<Extern> {
        self.defs
            .get(&(module.to_owned(), name.to_owned()))
            .copied()
    }

    /// Resolves all imports of `module` and instantiates it.
    pub fn instantiate(&self, store: &mut Store, module: &Module) -> Result<Instance> {
        let mut externs = Vec::new();
        for (m, n, ty) in module.imports() {
            let ext = match self.get(m, n) {
                Some(e) => e,
                None => match self.fallback.as_ref().and_then(|f| f(store, m, n, &ty)) {
                    Some(e) => e,
                    None => {
                        return Err(Error::Link(LinkError {
                            message: format!("unknown import `{m}`.`{n}` ({})", ty.kind()),
                        }))
                    }
                },
            };
            externs.push(ext);
        }
        store.instantiate(module, &externs)
    }
}
