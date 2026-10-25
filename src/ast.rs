//! The decoded (but not yet validated) representation of a binary module.
//!
//! [`crate::decode::decode`] produces a [`RawModule`]; the validator checks it
//! and lowers function bodies into the interpreter IR.

use crate::ops::NumOp;
use crate::types::{BlockType, ExternKind, FuncType, GlobalType, MemoryType, RefType, TableType, ValType};

/// Alignment exponent and static offset of a memory access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemArg {
    /// log2 of the alignment hint.
    pub align: u32,
    /// Constant byte offset added to the dynamic address.
    pub offset: u32,
}

/// The shape of a load instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum LoadKind {
    I32,
    I64,
    F32,
    F64,
    I32_8S,
    I32_8U,
    I32_16S,
    I32_16U,
    I64_8S,
    I64_8U,
    I64_16S,
    I64_16U,
    I64_32S,
    I64_32U,
}

impl LoadKind {
    /// log2 of the access width in bytes.
    pub fn natural_align(self) -> u32 {
        use LoadKind::*;
        match self {
            I32_8S | I32_8U | I64_8S | I64_8U => 0,
            I32_16S | I32_16U | I64_16S | I64_16U => 1,
            I32 | F32 | I64_32S | I64_32U => 2,
            I64 | F64 => 3,
        }
    }

    /// The type pushed by the load.
    pub fn result(self) -> ValType {
        use LoadKind::*;
        match self {
            I32 | I32_8S | I32_8U | I32_16S | I32_16U => ValType::I32,
            F32 => ValType::F32,
            F64 => ValType::F64,
            _ => ValType::I64,
        }
    }
}

/// The shape of a store instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum StoreKind {
    I32,
    I64,
    F32,
    F64,
    I32_8,
    I32_16,
    I64_8,
    I64_16,
    I64_32,
}

impl StoreKind {
    /// log2 of the access width in bytes.
    pub fn natural_align(self) -> u32 {
        use StoreKind::*;
        match self {
            I32_8 | I64_8 => 0,
            I32_16 | I64_16 => 1,
            I32 | F32 | I64_32 => 2,
            I64 | F64 => 3,
        }
    }

    /// The type of the stored operand.
    pub fn operand(self) -> ValType {
        use StoreKind::*;
        match self {
            I32 | I32_8 | I32_16 => ValType::I32,
            F32 => ValType::F32,
            F64 => ValType::F64,
            I64 | I64_8 | I64_16 | I64_32 => ValType::I64,
        }
    }
}

/// One decoded instruction.
#[derive(Debug, Clone, PartialEq)]
#[allow(missing_docs)]
pub enum Instr {
    Unreachable,
    Nop,
    Block(BlockType),
    Loop(BlockType),
    If(BlockType),
    Else,
    End,
    Br(u32),
    BrIf(u32),
    /// Branch targets; the last element is the default target.
    BrTable(Box<[u32]>),
    Return,
    Call(u32),
    CallIndirect { type_idx: u32, table: u32 },
    Drop,
    /// `select` (untyped when `None`) or `select t*` (typed, always exactly
    /// the decoded type list; arity is checked by the validator).
    Select(Option<Box<[ValType]>>),
    LocalGet(u32),
    LocalSet(u32),
    LocalTee(u32),
    GlobalGet(u32),
    GlobalSet(u32),
    TableGet(u32),
    TableSet(u32),
    TableSize(u32),
    TableGrow(u32),
    TableFill(u32),
    TableCopy { dst: u32, src: u32 },
    TableInit { elem: u32, table: u32 },
    ElemDrop(u32),
    Load(LoadKind, MemArg),
    Store(StoreKind, MemArg),
    MemorySize,
    MemoryGrow,
    MemoryInit(u32),
    DataDrop(u32),
    MemoryCopy,
    MemoryFill,
    I32Const(i32),
    I64Const(i64),
    /// Raw IEEE-754 bits.
    F32Const(u32),
    /// Raw IEEE-754 bits.
    F64Const(u64),
    RefNull(RefType),
    RefIsNull,
    RefFunc(u32),
    Num(NumOp),
}

/// A constant expression (initializers and segment offsets), without its
/// terminating `end`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ConstExpr(pub Vec<Instr>);

/// An import entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Import {
    /// Module name.
    pub module: String,
    /// Field name.
    pub name: String,
    /// What is imported.
    pub desc: ImportDesc,
}

/// An import descriptor.
#[derive(Debug, Clone, PartialEq)]
pub enum ImportDesc {
    /// A function with the given type index.
    Func(u32),
    /// A table.
    Table(TableType),
    /// A memory.
    Memory(MemoryType),
    /// A global.
    Global(GlobalType),
}

impl ImportDesc {
    /// The kind of entity imported.
    pub fn kind(&self) -> ExternKind {
        match self {
            ImportDesc::Func(_) => ExternKind::Func,
            ImportDesc::Table(_) => ExternKind::Table,
            ImportDesc::Memory(_) => ExternKind::Memory,
            ImportDesc::Global(_) => ExternKind::Global,
        }
    }
}

/// An export entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    /// Export name.
    pub name: String,
    /// Kind of the exported entity.
    pub kind: ExternKind,
    /// Index into the corresponding index space.
    pub index: u32,
}

/// A module-defined global.
#[derive(Debug, Clone, PartialEq)]
pub struct Global {
    /// Its type.
    pub ty: GlobalType,
    /// Its initializer.
    pub init: ConstExpr,
}

/// How an element segment is used.
#[derive(Debug, Clone, PartialEq)]
pub enum ElemMode {
    /// Available to `table.init`.
    Passive,
    /// Only declares function references; dropped at instantiation.
    Declarative,
    /// Copied into `table` at `offset` during instantiation.
    Active {
        /// Target table index.
        table: u32,
        /// Start offset expression (i32).
        offset: ConstExpr,
    },
}

/// An element segment.
#[derive(Debug, Clone, PartialEq)]
pub struct Elem {
    /// Element type.
    pub ty: RefType,
    /// One constant expression per element.
    pub init: Vec<ConstExpr>,
    /// Usage mode.
    pub mode: ElemMode,
}

/// How a data segment is used.
#[derive(Debug, Clone, PartialEq)]
pub enum DataMode {
    /// Available to `memory.init`.
    Passive,
    /// Copied into `memory` at `offset` during instantiation.
    Active {
        /// Target memory index.
        memory: u32,
        /// Start offset expression (i32).
        offset: ConstExpr,
    },
}

/// A data segment.
#[derive(Debug, Clone, PartialEq)]
pub struct Data {
    /// Segment bytes.
    pub init: Vec<u8>,
    /// Usage mode.
    pub mode: DataMode,
}

/// A function body.
#[derive(Debug, Clone, PartialEq)]
pub struct FuncBody {
    /// Run-length encoded local declarations.
    pub locals: Vec<(u32, ValType)>,
    /// Instructions including the final `end`.
    pub code: Vec<Instr>,
}

/// A custom section.
#[derive(Debug, Clone, PartialEq)]
pub struct Custom {
    /// Section name.
    pub name: String,
    /// Payload size in bytes.
    pub len: usize,
}

/// A module exactly as decoded from the binary.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RawModule {
    /// Type section.
    pub types: Vec<FuncType>,
    /// Import section.
    pub imports: Vec<Import>,
    /// Function section: a type index per defined function.
    pub funcs: Vec<u32>,
    /// Table section.
    pub tables: Vec<TableType>,
    /// Memory section.
    pub memories: Vec<MemoryType>,
    /// Global section.
    pub globals: Vec<Global>,
    /// Export section.
    pub exports: Vec<Export>,
    /// Start section.
    pub start: Option<u32>,
    /// Element section.
    pub elems: Vec<Elem>,
    /// Data-count section.
    pub data_count: Option<u32>,
    /// Code section.
    pub code: Vec<FuncBody>,
    /// Data section.
    pub datas: Vec<Data>,
    /// Custom sections, in order of appearance.
    pub customs: Vec<Custom>,
}

impl RawModule {
    /// Number of imports of the given kind.
    pub fn imported(&self, kind: ExternKind) -> usize {
        self.imports.iter().filter(|i| i.desc.kind() == kind).count()
    }
}
