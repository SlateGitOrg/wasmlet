//! Binary decoder: bytes -> [`RawModule`].
//!
//! Implements the binary format of the WebAssembly 2.0 core specification
//! (MVP + multi-value + sign-extension + saturating truncation + bulk memory
//! + reference types), without SIMD. Structural rules that the spec classifies
//! as "malformed" are enforced here; typing rules are left to the validator.

use crate::ast::{
    ConstExpr, Custom, Data, DataMode, Elem, ElemMode, Export, FuncBody, Global, Import, ImportDesc, Instr,
    LoadKind, MemArg, RawModule, StoreKind,
};
use crate::error::DecodeError;
use crate::ops::NumOp;
use crate::reader::{DResult, Reader};
use crate::types::{
    BlockType, ExternKind, FuncType, GlobalType, Limits, MemoryType, Mutability, RefType,
    TableType, ValType,
};

const MAGIC: &[u8; 4] = b"\0asm";
const VERSION: u32 = 1;

/// Upper bound on the total number of locals in one function. The binary
/// format allows up to 2^32-1; this implementation limit keeps the
/// interpreter's frame allocation bounded.
pub const MAX_LOCALS: u64 = 50_000;

/// Decodes a binary module.
pub fn decode(bytes: &[u8]) -> Result<RawModule, DecodeError> {
    let mut r = Reader::new(bytes, 0);
    let magic = r.bytes(4)?;
    if magic != MAGIC {
        return Err(DecodeError::new(0, "magic header not detected"));
    }
    let version = r.bytes(4)?;
    if u32::from_le_bytes([version[0], version[1], version[2], version[3]]) != VERSION {
        return Err(DecodeError::new(4, "unknown binary version"));
    }

    let mut m = RawModule::default();
    let mut last_rank = 0u8;
    let mut code_count: Option<u32> = None;
    let mut uses_data_count = false;
    while !r.is_empty() {
        let id_offset = r.offset();
        let id = r.u8()?;
        let size = r.u32()? as usize;
        if size > r.remaining() {
            return r.err("length out of bounds");
        }
        let mut s = r.sub(size)?;
        if id == 0 {
            let name = s.name()?;
            m.customs.push(Custom {
                name,
                len: s.remaining(),
            });
            continue;
        }
        let rank = section_rank(id)
            .ok_or_else(|| DecodeError::new(id_offset, "malformed section id"))?;
        if rank <= last_rank {
            return Err(DecodeError::new(id_offset, "unexpected content after last section"));
        }
        last_rank = rank;
        match id {
            1 => m.types = vec_of(&mut s, 3, read_func_type)?,
            2 => m.imports = vec_of(&mut s, 3, read_import)?,
            3 => m.funcs = vec_of(&mut s, 1, |s| s.u32())?,
            4 => m.tables = vec_of(&mut s, 2, read_table_type)?,
            5 => m.memories = vec_of(&mut s, 2, read_memory_type)?,
            6 => m.globals = vec_of(&mut s, 3, read_global)?,
            7 => m.exports = vec_of(&mut s, 3, read_export)?,
            8 => m.start = Some(s.u32()?),
            9 => m.elems = vec_of(&mut s, 2, read_elem)?,
            12 => m.data_count = Some(s.u32()?),
            10 => {
                let n = s.count(2)?;
                code_count = Some(n);
                if n as usize != m.funcs.len() {
                    return s.err("function and code section have inconsistent lengths");
                }
                let mut bodies = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    bodies.push(read_body(&mut s, &mut uses_data_count)?);
                }
                m.code = bodies;
            }
            11 => {
                m.datas = vec_of(&mut s, 2, read_data)?;
                if let Some(c) = m.data_count {
                    if c as usize != m.datas.len() {
                        return s.err("data count and data section have inconsistent lengths");
                    }
                }
            }
            _ => unreachable!("section_rank accepted id {id}"),
        }
        if !s.is_empty() {
            return s.err("section size mismatch");
        }
    }
    if code_count.is_none() && !m.funcs.is_empty() {
        return r.err("function and code section have inconsistent lengths");
    }
    if let Some(c) = m.data_count {
        if c as usize != m.datas.len() {
            return r.err("data count and data section have inconsistent lengths");
        }
    }
    if uses_data_count && m.data_count.is_none() {
        return r.err("data count section required");
    }
    Ok(m)
}

/// Position of each known section id in the mandated order.
fn section_rank(id: u8) -> Option<u8> {
    Some(match id {
        1..=9 => id,
        12 => 10,
        10 => 11,
        11 => 12,
        _ => return None,
    })
}

fn vec_of<'a, T>(
    r: &mut Reader<'a>,
    min_elem_size: usize,
    mut f: impl FnMut(&mut Reader<'a>) -> DResult<T>,
) -> DResult<Vec<T>> {
    let n = r.count(min_elem_size)?;
    let mut v = Vec::with_capacity(n as usize);
    for _ in 0..n {
        v.push(f(r)?);
    }
    Ok(v)
}

pub(crate) fn read_val_type(r: &mut Reader<'_>) -> DResult<ValType> {
    let b = r.u8()?;
    val_type_from_byte(b).ok_or_else(|| {
        let msg = if b == 0x7b {
            "SIMD (v128) is not supported"
        } else {
            "malformed value type"
        };
        DecodeError::new(r.offset() - 1, msg)
    })
}

fn val_type_from_byte(b: u8) -> Option<ValType> {
    Some(match b {
        0x7f => ValType::I32,
        0x7e => ValType::I64,
        0x7d => ValType::F32,
        0x7c => ValType::F64,
        0x70 => ValType::FUNCREF,
        0x6f => ValType::EXTERNREF,
        _ => return None,
    })
}

fn read_ref_type(r: &mut Reader<'_>) -> DResult<RefType> {
    match r.u8()? {
        0x70 => Ok(RefType::Func),
        0x6f => Ok(RefType::Extern),
        _ => Err(DecodeError::new(r.offset() - 1, "malformed reference type")),
    }
}

fn read_func_type(r: &mut Reader<'_>) -> DResult<FuncType> {
    if r.u8()? != 0x60 {
        return Err(DecodeError::new(r.offset() - 1, "malformed function type"));
    }
    let params = vec_of(r, 1, read_val_type)?;
    let results = vec_of(r, 1, read_val_type)?;
    Ok(FuncType::new(params, results))
}

fn read_limits(r: &mut Reader<'_>) -> DResult<Limits> {
    match r.u8()? {
        0x00 => Ok(Limits {
            min: r.u32()?,
            max: None,
        }),
        0x01 => {
            let min = r.u32()?;
            let max = r.u32()?;
            Ok(Limits {
                min,
                max: Some(max),
            })
        }
        _ => Err(DecodeError::new(r.offset() - 1, "integer too large")),
    }
}

fn read_table_type(r: &mut Reader<'_>) -> DResult<TableType> {
    let elem = read_ref_type(r)?;
    let limits = read_limits(r)?;
    Ok(TableType { elem, limits })
}

fn read_memory_type(r: &mut Reader<'_>) -> DResult<MemoryType> {
    Ok(MemoryType {
        limits: read_limits(r)?,
    })
}

fn read_global_type(r: &mut Reader<'_>) -> DResult<GlobalType> {
    let content = read_val_type(r)?;
    let mutability = match r.u8()? {
        0 => Mutability::Const,
        1 => Mutability::Var,
        _ => return Err(DecodeError::new(r.offset() - 1, "malformed mutability")),
    };
    Ok(GlobalType {
        content,
        mutability,
    })
}

fn read_import(r: &mut Reader<'_>) -> DResult<Import> {
    let module = r.name()?;
    let name = r.name()?;
    let desc = match r.u8()? {
        0x00 => ImportDesc::Func(r.u32()?),
        0x01 => ImportDesc::Table(read_table_type(r)?),
        0x02 => ImportDesc::Memory(read_memory_type(r)?),
        0x03 => ImportDesc::Global(read_global_type(r)?),
        _ => return Err(DecodeError::new(r.offset() - 1, "malformed import kind")),
    };
    Ok(Import { module, name, desc })
}

fn read_export(r: &mut Reader<'_>) -> DResult<Export> {
    let name = r.name()?;
    let kind = match r.u8()? {
        0x00 => ExternKind::Func,
        0x01 => ExternKind::Table,
        0x02 => ExternKind::Memory,
        0x03 => ExternKind::Global,
        _ => return Err(DecodeError::new(r.offset() - 1, "malformed export kind")),
    };
    let index = r.u32()?;
    Ok(Export { name, kind, index })
}

fn read_global(r: &mut Reader<'_>) -> DResult<Global> {
    let ty = read_global_type(r)?;
    let init = read_const_expr(r)?;
    Ok(Global { ty, init })
}

fn read_const_expr(r: &mut Reader<'_>) -> DResult<ConstExpr> {
    let mut instrs = Vec::new();
    let mut ignored = false;
    loop {
        let i = read_instr(r, &mut ignored)?;
        if i == Instr::End {
            return Ok(ConstExpr(instrs));
        }
        instrs.push(i);
    }
}

fn read_elem_kind(r: &mut Reader<'_>) -> DResult<RefType> {
    match r.u8()? {
        0x00 => Ok(RefType::Func),
        _ => Err(DecodeError::new(r.offset() - 1, "malformed element kind")),
    }
}

fn func_index_exprs(r: &mut Reader<'_>) -> DResult<Vec<ConstExpr>> {
    vec_of(r, 1, |r| Ok(ConstExpr(vec![Instr::RefFunc(r.u32()?)])))
}

fn read_elem(r: &mut Reader<'_>) -> DResult<Elem> {
    let flags = r.u32()?;
    let elem = match flags {
        0 => {
            let offset = read_const_expr(r)?;
            Elem {
                ty: RefType::Func,
                init: func_index_exprs(r)?,
                mode: ElemMode::Active { table: 0, offset },
            }
        }
        1 | 3 => {
            let ty = read_elem_kind(r)?;
            Elem {
                ty,
                init: func_index_exprs(r)?,
                mode: if flags == 1 {
                    ElemMode::Passive
                } else {
                    ElemMode::Declarative
                },
            }
        }
        2 => {
            let table = r.u32()?;
            let offset = read_const_expr(r)?;
            let ty = read_elem_kind(r)?;
            Elem {
                ty,
                init: func_index_exprs(r)?,
                mode: ElemMode::Active { table, offset },
            }
        }
        4 => {
            let offset = read_const_expr(r)?;
            Elem {
                ty: RefType::Func,
                init: vec_of(r, 1, read_const_expr)?,
                mode: ElemMode::Active { table: 0, offset },
            }
        }
        5 | 7 => {
            let ty = read_ref_type(r)?;
            Elem {
                ty,
                init: vec_of(r, 1, read_const_expr)?,
                mode: if flags == 5 {
                    ElemMode::Passive
                } else {
                    ElemMode::Declarative
                },
            }
        }
        6 => {
            let table = r.u32()?;
            let offset = read_const_expr(r)?;
            let ty = read_ref_type(r)?;
            Elem {
                ty,
                init: vec_of(r, 1, read_const_expr)?,
                mode: ElemMode::Active { table, offset },
            }
        }
        _ => return Err(DecodeError::new(r.offset(), "malformed elements segment kind")),
    };
    Ok(elem)
}

fn read_data(r: &mut Reader<'_>) -> DResult<Data> {
    let flags = r.u32()?;
    let mode = match flags {
        0 => DataMode::Active {
            memory: 0,
            offset: read_const_expr(r)?,
        },
        1 => DataMode::Passive,
        2 => {
            let memory = r.u32()?;
            DataMode::Active {
                memory,
                offset: read_const_expr(r)?,
            }
        }
        _ => return Err(DecodeError::new(r.offset(), "malformed data segment kind")),
    };
    let len = r.u32()? as usize;
    if len > r.remaining() {
        return r.err("unexpected end of section or function");
    }
    let init = r.bytes(len)?.to_vec();
    Ok(Data { init, mode })
}

fn read_body(r: &mut Reader<'_>, uses_data_count: &mut bool) -> DResult<FuncBody> {
    let size = r.u32()? as usize;
    if size > r.remaining() {
        return r.err("unexpected end of section or function");
    }
    let mut b = r.sub(size)?;
    let groups = b.count(2)?;
    let mut locals = Vec::with_capacity(groups as usize);
    let mut total: u64 = 0;
    for _ in 0..groups {
        let n = b.u32()?;
        total += u64::from(n);
        if total > u64::from(u32::MAX) {
            return b.err("too many locals");
        }
        let t = read_val_type(&mut b)?;
        locals.push((n, t));
    }
    if total > MAX_LOCALS {
        return b.err("too many locals");
    }
    let mut code = Vec::new();
    let mut depth: u32 = 0;
    loop {
        if b.is_empty() {
            return b.err("END opcode expected");
        }
        let i = read_instr(&mut b, uses_data_count)?;
        match i {
            Instr::Block(_) | Instr::Loop(_) | Instr::If(_) => depth += 1,
            Instr::End => {
                if depth == 0 {
                    code.push(i);
                    break;
                }
                depth -= 1;
            }
            _ => {}
        }
        code.push(i);
    }
    if !b.is_empty() {
        return b.err("section size mismatch");
    }
    Ok(FuncBody { locals, code })
}

fn read_block_type(r: &mut Reader<'_>) -> DResult<BlockType> {
    let b = r.peek().ok_or_else(|| DecodeError::new(r.offset(), "unexpected end"))?;
    if b == 0x40 {
        r.u8()?;
        return Ok(BlockType::Empty);
    }
    if b == 0x7b {
        return Err(DecodeError::new(r.offset(), "SIMD (v128) is not supported"));
    }
    if let Some(t) = val_type_from_byte(b) {
        r.u8()?;
        return Ok(BlockType::Value(t));
    }
    let start = r.offset();
    let idx = r.s33()?;
    if idx < 0 || idx > i64::from(u32::MAX) {
        return Err(DecodeError::new(start, "malformed block type"));
    }
    Ok(BlockType::Func(idx as u32))
}

fn read_memarg(r: &mut Reader<'_>) -> DResult<MemArg> {
    let at = r.offset();
    let align = r.u32()?;
    if align >= 32 {
        return Err(DecodeError::new(at, "malformed memop flags"));
    }
    let offset = r.u32()?;
    Ok(MemArg { align, offset })
}

fn zero_byte(r: &mut Reader<'_>) -> DResult<()> {
    if r.u8()? != 0 {
        return Err(DecodeError::new(r.offset() - 1, "zero byte expected"));
    }
    Ok(())
}

fn read_instr(r: &mut Reader<'_>, uses_data_count: &mut bool) -> DResult<Instr> {
    use Instr::*;
    let at = r.offset();
    let op = r.u8()?;
    Ok(match op {
        0x00 => Unreachable,
        0x01 => Nop,
        0x02 => Block(read_block_type(r)?),
        0x03 => Loop(read_block_type(r)?),
        0x04 => If(read_block_type(r)?),
        0x05 => Else,
        0x0b => End,
        0x0c => Br(r.u32()?),
        0x0d => BrIf(r.u32()?),
        0x0e => {
            let n = r.count(1)?;
            let mut targets = Vec::with_capacity(n as usize + 1);
            for _ in 0..=n {
                targets.push(r.u32()?);
            }
            BrTable(targets.into_boxed_slice())
        }
        0x0f => Return,
        0x10 => Call(r.u32()?),
        0x11 => {
            let type_idx = r.u32()?;
            let table = r.u32()?;
            CallIndirect { type_idx, table }
        }
        0x1a => Drop,
        0x1b => Select(None),
        0x1c => Select(Some(vec_of(r, 1, read_val_type)?.into_boxed_slice())),
        0x20 => LocalGet(r.u32()?),
        0x21 => LocalSet(r.u32()?),
        0x22 => LocalTee(r.u32()?),
        0x23 => GlobalGet(r.u32()?),
        0x24 => GlobalSet(r.u32()?),
        0x25 => TableGet(r.u32()?),
        0x26 => TableSet(r.u32()?),
        0x28..=0x35 => {
            let kind = match op {
                0x28 => LoadKind::I32,
                0x29 => LoadKind::I64,
                0x2a => LoadKind::F32,
                0x2b => LoadKind::F64,
                0x2c => LoadKind::I32_8S,
                0x2d => LoadKind::I32_8U,
                0x2e => LoadKind::I32_16S,
                0x2f => LoadKind::I32_16U,
                0x30 => LoadKind::I64_8S,
                0x31 => LoadKind::I64_8U,
                0x32 => LoadKind::I64_16S,
                0x33 => LoadKind::I64_16U,
                0x34 => LoadKind::I64_32S,
                _ => LoadKind::I64_32U,
            };
            Load(kind, read_memarg(r)?)
        }
        0x36..=0x3e => {
            let kind = match op {
                0x36 => StoreKind::I32,
                0x37 => StoreKind::I64,
                0x38 => StoreKind::F32,
                0x39 => StoreKind::F64,
                0x3a => StoreKind::I32_8,
                0x3b => StoreKind::I32_16,
                0x3c => StoreKind::I64_8,
                0x3d => StoreKind::I64_16,
                _ => StoreKind::I64_32,
            };
            Store(kind, read_memarg(r)?)
        }
        0x3f => {
            zero_byte(r)?;
            MemorySize
        }
        0x40 => {
            zero_byte(r)?;
            MemoryGrow
        }
        0x41 => I32Const(r.s32()?),
        0x42 => I64Const(r.s64()?),
        0x43 => F32Const(r.f32_bits()?),
        0x44 => F64Const(r.f64_bits()?),
        0xd0 => RefNull(read_ref_type(r)?),
        0xd1 => RefIsNull,
        0xd2 => RefFunc(r.u32()?),
        0xfc => {
            let sub = r.u32()?;
            match sub {
                0..=7 => Num(NumOp::from_opcode(0xfc00 | sub as u16).expect("table covers 0xfc00..=0xfc07")),
                8 => {
                    *uses_data_count = true;
                    let d = r.u32()?;
                    zero_byte(r)?;
                    MemoryInit(d)
                }
                9 => {
                    *uses_data_count = true;
                    DataDrop(r.u32()?)
                }
                10 => {
                    zero_byte(r)?;
                    zero_byte(r)?;
                    MemoryCopy
                }
                11 => {
                    zero_byte(r)?;
                    MemoryFill
                }
                12 => {
                    let elem = r.u32()?;
                    let table = r.u32()?;
                    TableInit { elem, table }
                }
                13 => ElemDrop(r.u32()?),
                14 => {
                    let dst = r.u32()?;
                    let src = r.u32()?;
                    TableCopy { dst, src }
                }
                15 => TableGrow(r.u32()?),
                16 => TableSize(r.u32()?),
                17 => TableFill(r.u32()?),
                _ => return Err(DecodeError::new(at, "illegal opcode")),
            }
        }
        _ => match NumOp::from_opcode(u16::from(op)) {
            Some(n) => Num(n),
            None => return Err(DecodeError::new(at, "illegal opcode")),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(sections: &[u8]) -> Vec<u8> {
        let mut v = b"\0asm\x01\0\0\0".to_vec();
        v.extend_from_slice(sections);
        v
    }

    #[test]
    fn empty_module() {
        let m = decode(&module(&[])).unwrap();
        assert!(m.types.is_empty() && m.code.is_empty());
    }

    #[test]
    fn header_errors() {
        assert_eq!(decode(b"\0as").unwrap_err().message, "unexpected end");
        assert_eq!(decode(b"\0wasm\x01\0\0").unwrap_err().message, "magic header not detected");
        assert_eq!(decode(b"\0asm\x02\0\0\0").unwrap_err().message, "unknown binary version");
        assert_eq!(decode(b"\0asm\x01\0").unwrap_err().message, "unexpected end");
    }

    #[test]
    fn decodes_function() {
        // (func (param i32) (result i32) local.get 0 i32.const 1 i32.add)
        let bytes = module(&[
            1, 6, 1, 0x60, 1, 0x7f, 1, 0x7f, // type
            3, 2, 1, 0, // func
            10, 9, 1, 7, 0, 0x20, 0, 0x41, 1, 0x6a, 0x0b, // code
        ]);
        let m = decode(&bytes).unwrap();
        assert_eq!(m.types[0].params(), &[ValType::I32]);
        assert_eq!(
            m.code[0].code,
            vec![Instr::LocalGet(0), Instr::I32Const(1), Instr::Num(NumOp::I32Add), Instr::End]
        );
    }

    #[test]
    fn section_errors() {
        assert_eq!(decode(&module(&[13, 0])).unwrap_err().message, "malformed section id");
        assert_eq!(
            decode(&module(&[3, 1, 0, 1, 1, 0])).unwrap_err().message,
            "unexpected content after last section"
        );
        assert_eq!(
            decode(&module(&[1, 2, 0, 0])).unwrap_err().message,
            "section size mismatch"
        );
        assert_eq!(decode(&module(&[1, 5, 0])).unwrap_err().message, "length out of bounds");
        assert_eq!(
            decode(&module(&[3, 2, 1, 0])).unwrap_err().message,
            "function and code section have inconsistent lengths"
        );
        assert_eq!(
            decode(&module(&[12, 1, 1])).unwrap_err().message,
            "data count and data section have inconsistent lengths"
        );
        // custom section with a bad UTF-8 name
        assert_eq!(
            decode(&module(&[0, 2, 1, 0xff])).unwrap_err().message,
            "malformed UTF-8 encoding"
        );
        let m = decode(&module(&[0, 4, 1, b'x', 9, 9])).unwrap();
        assert_eq!(m.customs, vec![Custom { name: "x".into(), len: 2 }]);
    }

    #[test]
    fn body_errors() {
        let ty = [1u8, 4, 1, 0x60, 0, 0, 3, 2, 1, 0];
        let with_code = |code: &[u8]| {
            let mut s = ty.to_vec();
            s.push(10);
            s.push(code.len() as u8 + 1);
            s.push(1);
            s.extend_from_slice(code);
            module(&s)
        };
        assert_eq!(
            decode(&with_code(&[2, 0, 0x01])).unwrap_err().message,
            "END opcode expected"
        );
        assert_eq!(decode(&with_code(&[2, 0, 0xff])).unwrap_err().message, "illegal opcode");
        assert_eq!(
            decode(&with_code(&[3, 0, 0x3f, 1, 0x0b])).unwrap_err().message,
            "zero byte expected"
        );
        assert_eq!(
            decode(&with_code(&[5, 0, 0xfc, 9, 0, 0x0b])).unwrap_err().message,
            "data count section required"
        );
        assert_eq!(
            decode(&with_code(&[6, 1, 0xff, 0xff, 0xff, 0x7f, 0x7f])).unwrap_err().message,
            "too many locals"
        );
        assert_eq!(
            decode(&with_code(&[4, 0, 0x02, 0x7b, 0x0b])).unwrap_err().message,
            "SIMD (v128) is not supported"
        );
    }

    #[test]
    fn elem_and_data_forms() {
        // table + passive elem (flag 1) + declarative elem with exprs (flag 7)
        let bytes = module(&[
            1, 4, 1, 0x60, 0, 0, 3, 2, 1, 0, 4, 4, 1, 0x70, 0, 1, //
            9, 10, 2, 1, 0, 1, 0, 7, 0x70, 1, 0xd2, 0, //
            // missing final byte placeholder fixed below
        ]);
        // The declarative expr above lacks its `end`; decoding must fail cleanly.
        assert!(decode(&bytes).is_err());
        let bytes = module(&[
            1, 4, 1, 0x60, 0, 0, 3, 2, 1, 0, 4, 4, 1, 0x70, 0, 1, //
            9, 11, 2, 1, 0, 1, 0, 7, 0x70, 1, 0xd2, 0, 0x0b, //
            10, 4, 1, 2, 0, 0x0b, //
            11, 8, 2, 1, 2, b'h', b'i', 1, 1, b'!',
        ]);
        let m = decode(&bytes).unwrap();
        assert_eq!(m.elems[0].mode, ElemMode::Passive);
        assert_eq!(m.elems[1].mode, ElemMode::Declarative);
        assert_eq!(m.elems[1].init[0].0, vec![Instr::RefFunc(0)]);
        assert_eq!(m.datas[0].init, b"hi");
        assert_eq!(m.datas[1].mode, DataMode::Passive);
    }
}
