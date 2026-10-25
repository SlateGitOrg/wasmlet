//! Runtime: store, instantiation, linking and the interpreter.

pub(crate) mod exec;
mod instance;
mod linker;
mod num;
mod store;
mod trap;
mod value;

pub use linker::{ImportResolver, Linker};
pub use store::{Caller, Config, Extern, Global, HostFunc, Instance, Memory, Store, Table};
pub use trap::Trap;
pub use value::{Func, Value};
