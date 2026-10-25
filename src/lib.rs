//! # wasmlet
//!
//! A WebAssembly interpreter written from scratch in safe Rust.
//!
//! Pipeline: [`decode`] (binary -> [`ast::RawModule`]) -> validation and
//! lowering to a compact IR ([`ir`]) -> instantiation in a [`Store`] ->
//! interpretation. A WASI preview1 subset lives in [`wasi`].
//!
//! Supported proposals: everything in the WebAssembly 2.0 core specification
//! except SIMD — i.e. the MVP plus multi-value, sign-extension, non-trapping
//! float-to-int, bulk memory and reference types.
//!
//! ```
//! use wasmlet::{Linker, Module, Store, Value};
//!
//! // (module (func (export "add") (param i32 i32) (result i32)
//! //   local.get 0 local.get 1 i32.add))
//! let bytes = [
//!     0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x07, 0x01, 0x60, 0x02, 0x7f,
//!     0x7f, 0x01, 0x7f, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, 0x61, 0x64, 0x64,
//!     0x00, 0x00, 0x0a, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x20, 0x01, 0x6a, 0x0b,
//! ];
//! let module = Module::new(&bytes).unwrap();
//! let mut store = Store::default();
//! let instance = Linker::new().instantiate(&mut store, &module).unwrap();
//! let add = store.instance_func(instance, "add").unwrap();
//! let out = store.invoke(add, &[Value::I32(2), Value::I32(40)]).unwrap();
//! assert_eq!(out, vec![Value::I32(42)]);
//! ```

pub mod ast;
pub mod decode;
mod error;
mod fuse;
pub mod ir;
mod module;
pub mod ops;
mod reader;
mod runtime;
pub mod types;
mod validate;
pub mod wasi;

pub use error::{DecodeError, Error, LinkError, Result, ValidationError};
pub use module::{CompileOptions, ExternType, Module};
pub use runtime::{
    Caller, Config, Extern, Func, Global, HostFunc, ImportResolver, Instance, Linker, Memory,
    Store, Table, Trap, Value,
};
