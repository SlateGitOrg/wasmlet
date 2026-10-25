//! The interpreter loop.
//!
//! Executes [`Op`] arrays over a single untyped value stack. Wasm-to-wasm
//! calls do not recurse on the native stack: the caller's state is pushed to
//! an explicit frame vector, so deep wasm recursion is bounded only by
//! [`super::Config::max_call_depth`] and ends in a clean
//! [`Trap::CallStackExhausted`].
//!
//! Frame layout on the value stack:
//!
//! ```text
//!   fp                fp+params        fp+params+locals
//!   | params ...      | locals (zeroed) | operand stack ... | <- sp
//! ```

use std::sync::Arc;

use super::num;
use super::store::{grow_memory, grow_table, Caller, FuncInst, InstanceData, Store};
use super::value::{from_slot, to_slot, Value};
use super::Trap;
use crate::ir::{CompiledFunc, DropKeep, Op};

struct Frame {
    func: Arc<CompiledFunc>,
    pc: usize,
    fp: usize,
    inst_idx: u32,
}

/// Calls store function `func` with argument slots; returns result slots.
pub(crate) fn invoke(store: &mut Store, func: u32, args: &[u64]) -> Result<Vec<u64>, Trap> {
    let (inst_idx, code) = match &store.funcs[func as usize] {
        FuncInst::Host { ty, func } => {
            let f = func.clone();
            let ty = store.type_by_id(*ty).clone();
            let vals: Vec<Value> = args
                .iter()
                .zip(ty.params())
                .map(|(&s, &t)| from_slot(s, t))
                .collect();
            let results = f(&mut Caller { store, instance: None }, &vals)?;
            check_host_results(ty.results(), &results)?;
            return Ok(results.into_iter().map(to_slot).collect());
        }
        FuncInst::Wasm { instance, code, .. } => (*instance, Arc::clone(code)),
    };
    let mut stack = std::mem::take(&mut store.stack);
    let mut fuel = store.fuel.unwrap_or(u64::MAX);
    let result = execute(store, &mut stack, &mut fuel, inst_idx, code, args);
    if store.fuel.is_some() {
        store.fuel = Some(fuel);
    }
    store.stack = stack;
    result
}

fn check_host_results(expected: &[crate::types::ValType], got: &[Value]) -> Result<(), Trap> {
    if got.len() != expected.len() || got.iter().zip(expected).any(|(v, t)| v.ty() != *t) {
        return Err(Trap::Host(
            "host function returned values that do not match its type".into(),
        ));
    }
    Ok(())
}

fn ensure_stack(stack: &mut Vec<u64>, need: usize, max: usize) -> Result<(), Trap> {
    if need <= stack.len() {
        return Ok(());
    }
    if need > max {
        return Err(Trap::CallStackExhausted);
    }
    let new_len = need.max(stack.len() * 2).max(1024).min(max);
    stack.resize(new_len, 0);
    Ok(())
}

/// Effective address of an access of `n` bytes, or an out-of-bounds trap.
#[inline(always)]
fn ea(addr: u64, offset: u32, n: usize, len: usize) -> Result<usize, Trap> {
    let a = u64::from(addr as u32) + u64::from(offset);
    if a + n as u64 > len as u64 {
        return Err(Trap::MemoryOutOfBounds);
    }
    Ok(a as usize)
}

#[inline(always)]
fn read<const N: usize>(m: &[u8], a: usize) -> [u8; N] {
    let mut b = [0u8; N];
    b.copy_from_slice(&m[a..a + N]);
    b
}

/// Range check `start + n <= len` without overflow.
#[inline(always)]
fn in_range(start: u32, n: u32, len: usize) -> bool {
    u64::from(start) + u64::from(n) <= len as u64
}

pub(crate) fn table_init(
    store: &mut Store,
    table: u32,
    seg: usize,
    d: u32,
    s: u32,
    n: u32,
) -> Result<(), Trap> {
    let src = &store.elems[seg];
    let dst = &mut store.tables[table as usize].elems;
    if !in_range(s, n, src.len()) || !in_range(d, n, dst.len()) {
        return Err(Trap::TableOutOfBounds);
    }
    dst[d as usize..(d + n) as usize].copy_from_slice(&src[s as usize..(s + n) as usize]);
    Ok(())
}

pub(crate) fn memory_init(
    store: &mut Store,
    mem: u32,
    seg: usize,
    d: u32,
    s: u32,
    n: u32,
) -> Result<(), Trap> {
    let src = &store.datas[seg];
    let dst = &mut store.memories[mem as usize].data;
    if !in_range(s, n, src.len()) || !in_range(d, n, dst.len()) {
        return Err(Trap::MemoryOutOfBounds);
    }
    dst[d as usize..(d + n) as usize].copy_from_slice(&src[s as usize..(s + n) as usize]);
    Ok(())
}

fn table_copy(store: &mut Store, dst: u32, src: u32, d: u32, s: u32, n: u32) -> Result<(), Trap> {
    let (dl, sl) = (
        store.tables[dst as usize].elems.len(),
        store.tables[src as usize].elems.len(),
    );
    if !in_range(s, n, sl) || !in_range(d, n, dl) {
        return Err(Trap::TableOutOfBounds);
    }
    let (d, s, n) = (d as usize, s as usize, n as usize);
    if dst == src {
        store.tables[dst as usize].elems.copy_within(s..s + n, d);
    } else {
        let tmp: Vec<u64> = store.tables[src as usize].elems[s..s + n].to_vec();
        store.tables[dst as usize].elems[d..d + n].copy_from_slice(&tmp);
    }
    Ok(())
}

// Slot <-> typed conversions. i32 values may carry garbage in the upper 32
// bits of a slot; every i32 read truncates, so producers need not clear them.
#[inline(always)]
fn i32_(s: u64) -> i32 {
    s as u32 as i32
}
#[inline(always)]
fn u32_(s: u64) -> u32 {
    s as u32
}
#[inline(always)]
fn i64_(s: u64) -> i64 {
    s as i64
}
#[inline(always)]
fn u64_(s: u64) -> u64 {
    s
}
#[inline(always)]
fn f32_(s: u64) -> f32 {
    f32::from_bits(s as u32)
}
#[inline(always)]
fn f64_(s: u64) -> f64 {
    f64::from_bits(s)
}
#[inline(always)]
fn fi32(v: i32) -> u64 {
    u64::from(v as u32)
}
#[inline(always)]
fn fu32(v: u32) -> u64 {
    u64::from(v)
}
#[inline(always)]
fn fi64(v: i64) -> u64 {
    v as u64
}
#[inline(always)]
fn fu64(v: u64) -> u64 {
    v
}
#[inline(always)]
fn ff32(v: f32) -> u64 {
    u64::from(v.to_bits())
}
#[inline(always)]
fn ff64(v: f64) -> u64 {
    v.to_bits()
}
#[inline(always)]
fn fbool(v: bool) -> u64 {
    u64::from(v)
}

fn execute(
    store: &mut Store,
    stack: &mut Vec<u64>,
    fuel: &mut u64,
    mut inst_idx: u32,
    mut func: Arc<CompiledFunc>,
    args: &[u64],
) -> Result<Vec<u64>, Trap> {
    let max_depth = store.config.max_call_depth;
    let max_stack = store.config.max_value_stack;
    let mem_cap = store.config.max_memory_pages;
    let table_cap = store.config.max_table_elements;
    let mut inst: Arc<InstanceData> = Arc::clone(&store.instances[inst_idx as usize]);
    let mut mem: usize = inst.mem_addrs.first().map_or(usize::MAX, |&m| m as usize);
    let mut frames: Vec<Frame> = Vec::new();

    ensure_stack(stack, args.len() + func.frame_size(), max_stack)?;
    stack[..args.len()].copy_from_slice(args);
    let mut fp: usize = 0;
    let mut sp: usize = args.len();
    stack[sp..sp + func.locals as usize].fill(0);
    sp += func.locals as usize;
    let mut pc: usize = 0;

    macro_rules! pop {
        () => {{
            sp -= 1;
            stack[sp]
        }};
    }
    macro_rules! push {
        ($v:expr) => {{
            let v = $v;
            stack[sp] = v;
            sp += 1;
        }};
    }
    macro_rules! unop {
        ($conv:ident, $out:ident, |$a:ident| $e:expr) => {{
            let $a = $conv(stack[sp - 1]);
            stack[sp - 1] = $out($e);
        }};
    }
    macro_rules! binop {
        ($conv:ident, $out:ident, |$a:ident, $b:ident| $e:expr) => {{
            sp -= 1;
            let $b = $conv(stack[sp]);
            let $a = $conv(stack[sp - 1]);
            stack[sp - 1] = $out($e);
        }};
    }
    macro_rules! burn {
        () => {{
            if *fuel == 0 {
                return Err(Trap::OutOfFuel);
            }
            *fuel -= 1;
        }};
    }
    macro_rules! branch {
        ($target:expr, $dk:expr) => {{
            let DropKeep { drop, keep } = $dk;
            if drop != 0 {
                let keep = keep as usize;
                let dst = sp - keep - drop as usize;
                stack.copy_within(sp - keep..sp, dst);
                sp = dst + keep;
            }
            pc = $target as usize;
        }};
    }
    macro_rules! load {
        ($off:expr, $n:literal, |$b:ident| $e:expr) => {{
            let m = &store.memories[mem].data;
            let a = ea(stack[sp - 1], $off, $n, m.len())?;
            let $b = read::<$n>(m, a);
            stack[sp - 1] = $e;
        }};
    }
    macro_rules! store_ {
        ($off:expr, $n:literal, |$v:ident| $bytes:expr) => {{
            let $v = pop!();
            let addr = pop!();
            let m = &mut store.memories[mem].data;
            let a = ea(addr, $off, $n, m.len())?;
            m[a..a + $n].copy_from_slice(&$bytes);
        }};
    }
    macro_rules! call {
        ($addr:expr) => {{
            let addr = $addr as usize;
            match &store.funcs[addr] {
                FuncInst::Wasm { instance, code, .. } => {
                    let callee = Arc::clone(code);
                    let callee_inst = *instance;
                    if frames.len() >= max_depth {
                        return Err(Trap::CallStackExhausted);
                    }
                    let new_fp = sp - callee.params as usize;
                    ensure_stack(stack, new_fp + callee.params as usize + callee.frame_size(), max_stack)?;
                    let locals = callee.locals as usize;
                    frames.push(Frame {
                        func: std::mem::replace(&mut func, callee),
                        pc,
                        fp,
                        inst_idx,
                    });
                    stack[sp..sp + locals].fill(0);
                    sp += locals;
                    fp = new_fp;
                    pc = 0;
                    if callee_inst != inst_idx {
                        inst_idx = callee_inst;
                        inst = Arc::clone(&store.instances[inst_idx as usize]);
                        mem = inst.mem_addrs.first().map_or(usize::MAX, |&m| m as usize);
                    }
                }
                FuncInst::Host { ty, func: hf } => {
                    let hf = hf.clone();
                    let ty = store.type_by_id(*ty).clone();
                    let n = ty.params().len();
                    sp -= n;
                    let args: Vec<Value> = stack[sp..sp + n]
                        .iter()
                        .zip(ty.params())
                        .map(|(&s, &t)| from_slot(s, t))
                        .collect();
                    let results = hf(
                        &mut Caller {
                            store,
                            instance: Some(inst_idx),
                        },
                        &args,
                    )?;
                    check_host_results(ty.results(), &results)?;
                    for r in results {
                        push!(to_slot(r));
                    }
                }
            }
        }};
    }

    loop {
        let op = func.code[pc];
        pc += 1;
        match op {
            Op::Unreachable => return Err(Trap::Unreachable),
            Op::Br { target, dk } => {
                burn!();
                branch!(target, dk);
            }
            Op::BrIfNez { target, dk } => {
                burn!();
                if pop!() as u32 != 0 {
                    branch!(target, dk);
                }
            }
            Op::BrIfEqz { target } => {
                if pop!() as u32 == 0 {
                    pc = target as usize;
                }
            }
            Op::BrTable { len } => {
                burn!();
                let i = (pop!() as u32).min(len) as usize;
                match func.code[pc + i] {
                    Op::BrTarget { target, dk } => branch!(target, dk),
                    other => unreachable!("br_table entry is {other:?}"),
                }
            }
            Op::BrTarget { .. } => unreachable!("br_table entries are never executed"),
            Op::Return { keep } => {
                let keep = keep as usize;
                if sp - keep != fp {
                    stack.copy_within(sp - keep..sp, fp);
                }
                sp = fp + keep;
                match frames.pop() {
                    None => return Ok(stack[..keep].to_vec()),
                    Some(f) => {
                        func = f.func;
                        pc = f.pc;
                        fp = f.fp;
                        if f.inst_idx != inst_idx {
                            inst_idx = f.inst_idx;
                            inst = Arc::clone(&store.instances[inst_idx as usize]);
                            mem = inst.mem_addrs.first().map_or(usize::MAX, |&m| m as usize);
                        }
                    }
                }
            }
            Op::Call(idx) => {
                burn!();
                call!(inst.func_addrs[idx as usize]);
            }
            Op::CallIndirect { type_idx, table } => {
                burn!();
                let i = pop!() as u32;
                let t = &store.tables[inst.table_addrs[table as usize] as usize];
                let r = *t.elems.get(i as usize).ok_or(Trap::UndefinedElement)?;
                if r == 0 {
                    return Err(Trap::UninitializedElement);
                }
                let addr = (r - 1) as u32;
                if store.funcs[addr as usize].ty() != inst.type_ids[type_idx as usize] {
                    return Err(Trap::IndirectCallTypeMismatch);
                }
                call!(addr);
            }
            Op::Drop => sp -= 1,
            Op::Select => {
                let c = pop!() as u32;
                let b = pop!();
                if c == 0 {
                    stack[sp - 1] = b;
                }
            }
            Op::LocalGet(i) => push!(stack[fp + i as usize]),
            Op::LocalSet(i) => {
                let v = pop!();
                stack[fp + i as usize] = v;
            }
            Op::LocalTee(i) => stack[fp + i as usize] = stack[sp - 1],
            Op::GlobalGet(i) => push!(store.globals[inst.global_addrs[i as usize] as usize].value),
            Op::GlobalSet(i) => {
                let v = pop!();
                store.globals[inst.global_addrs[i as usize] as usize].value = v;
            }
            Op::TableGet(t) => {
                let i = pop!() as u32;
                let tab = &store.tables[inst.table_addrs[t as usize] as usize];
                push!(*tab.elems.get(i as usize).ok_or(Trap::TableOutOfBounds)?);
            }
            Op::TableSet(t) => {
                let v = pop!();
                let i = pop!() as u32;
                let tab = &mut store.tables[inst.table_addrs[t as usize] as usize];
                *tab.elems.get_mut(i as usize).ok_or(Trap::TableOutOfBounds)? = v;
            }
            Op::TableSize(t) => {
                push!(store.tables[inst.table_addrs[t as usize] as usize].elems.len() as u64)
            }
            Op::TableGrow(t) => {
                let n = pop!() as u32;
                let init = pop!();
                let tab = &mut store.tables[inst.table_addrs[t as usize] as usize];
                push!(fu32(grow_table(tab, n, init, table_cap).unwrap_or(u32::MAX)));
            }
            Op::TableFill(t) => {
                let n = pop!() as u32;
                let v = pop!();
                let i = pop!() as u32;
                let tab = &mut store.tables[inst.table_addrs[t as usize] as usize];
                if !in_range(i, n, tab.elems.len()) {
                    return Err(Trap::TableOutOfBounds);
                }
                tab.elems[i as usize..(i + n) as usize].fill(v);
            }
            Op::TableCopy { dst, src } => {
                let n = pop!() as u32;
                let s = pop!() as u32;
                let d = pop!() as u32;
                let (dt, st) = (inst.table_addrs[dst as usize], inst.table_addrs[src as usize]);
                table_copy(store, dt, st, d, s, n)?;
            }
            Op::TableInit { elem, table } => {
                let n = pop!() as u32;
                let s = pop!() as u32;
                let d = pop!() as u32;
                let seg = inst.elem_addrs[elem as usize] as usize;
                table_init(store, inst.table_addrs[table as usize], seg, d, s, n)?;
            }
            Op::ElemDrop(e) => store.elems[inst.elem_addrs[e as usize] as usize] = Vec::new(),
            Op::Load8S(o) => load!(o, 1, |b| fi64(i64::from(b[0] as i8))),
            Op::Load8U(o) => load!(o, 1, |b| u64::from(b[0])),
            Op::Load16S(o) => load!(o, 2, |b| fi64(i64::from(i16::from_le_bytes(b)))),
            Op::Load16U(o) => load!(o, 2, |b| u64::from(u16::from_le_bytes(b))),
            Op::Load32S(o) => load!(o, 4, |b| fi64(i64::from(i32::from_le_bytes(b)))),
            Op::Load32U(o) => load!(o, 4, |b| u64::from(u32::from_le_bytes(b))),
            Op::Load64(o) => load!(o, 8, |b| u64::from_le_bytes(b)),
            Op::Store8(o) => store_!(o, 1, |v| [v as u8]),
            Op::Store16(o) => store_!(o, 2, |v| (v as u16).to_le_bytes()),
            Op::Store32(o) => store_!(o, 4, |v| (v as u32).to_le_bytes()),
            Op::Store64(o) => store_!(o, 8, |v| v.to_le_bytes()),
            Op::MemorySize => push!(u64::from(store.memories[mem].pages())),
            Op::MemoryGrow => {
                let n = pop!() as u32;
                let r = grow_memory(&mut store.memories[mem], n, mem_cap);
                push!(fu32(r.unwrap_or(u32::MAX)));
            }
            Op::MemoryInit(x) => {
                let n = pop!() as u32;
                let s = pop!() as u32;
                let d = pop!() as u32;
                memory_init(store, mem as u32, inst.data_addrs[x as usize] as usize, d, s, n)?;
            }
            Op::DataDrop(x) => store.datas[inst.data_addrs[x as usize] as usize] = Arc::from([]),
            Op::MemoryCopy => {
                let n = pop!() as u32;
                let s = pop!() as u32;
                let d = pop!() as u32;
                let m = &mut store.memories[mem].data;
                if !in_range(s, n, m.len()) || !in_range(d, n, m.len()) {
                    return Err(Trap::MemoryOutOfBounds);
                }
                m.copy_within(s as usize..(s + n) as usize, d as usize);
            }
            Op::MemoryFill => {
                let n = pop!() as u32;
                let v = pop!() as u8;
                let d = pop!() as u32;
                let m = &mut store.memories[mem].data;
                if !in_range(d, n, m.len()) {
                    return Err(Trap::MemoryOutOfBounds);
                }
                m[d as usize..(d + n) as usize].fill(v);
            }
            Op::Const(v) => push!(v),
            Op::RefIsNull => unop!(u64_, fbool, |a| a == 0),
            Op::RefFunc(f) => push!(u64::from(inst.func_addrs[f as usize]) + 1),

            Op::I32Eqz => unop!(u32_, fbool, |a| a == 0),
            Op::I32Eq => binop!(u32_, fbool, |a, b| a == b),
            Op::I32Ne => binop!(u32_, fbool, |a, b| a != b),
            Op::I32LtS => binop!(i32_, fbool, |a, b| a < b),
            Op::I32LtU => binop!(u32_, fbool, |a, b| a < b),
            Op::I32GtS => binop!(i32_, fbool, |a, b| a > b),
            Op::I32GtU => binop!(u32_, fbool, |a, b| a > b),
            Op::I32LeS => binop!(i32_, fbool, |a, b| a <= b),
            Op::I32LeU => binop!(u32_, fbool, |a, b| a <= b),
            Op::I32GeS => binop!(i32_, fbool, |a, b| a >= b),
            Op::I32GeU => binop!(u32_, fbool, |a, b| a >= b),
            Op::I64Eqz => unop!(u64_, fbool, |a| a == 0),
            Op::I64Eq => binop!(u64_, fbool, |a, b| a == b),
            Op::I64Ne => binop!(u64_, fbool, |a, b| a != b),
            Op::I64LtS => binop!(i64_, fbool, |a, b| a < b),
            Op::I64LtU => binop!(u64_, fbool, |a, b| a < b),
            Op::I64GtS => binop!(i64_, fbool, |a, b| a > b),
            Op::I64GtU => binop!(u64_, fbool, |a, b| a > b),
            Op::I64LeS => binop!(i64_, fbool, |a, b| a <= b),
            Op::I64LeU => binop!(u64_, fbool, |a, b| a <= b),
            Op::I64GeS => binop!(i64_, fbool, |a, b| a >= b),
            Op::I64GeU => binop!(u64_, fbool, |a, b| a >= b),
            Op::F32Eq => binop!(f32_, fbool, |a, b| a == b),
            Op::F32Ne => binop!(f32_, fbool, |a, b| a != b),
            Op::F32Lt => binop!(f32_, fbool, |a, b| a < b),
            Op::F32Gt => binop!(f32_, fbool, |a, b| a > b),
            Op::F32Le => binop!(f32_, fbool, |a, b| a <= b),
            Op::F32Ge => binop!(f32_, fbool, |a, b| a >= b),
            Op::F64Eq => binop!(f64_, fbool, |a, b| a == b),
            Op::F64Ne => binop!(f64_, fbool, |a, b| a != b),
            Op::F64Lt => binop!(f64_, fbool, |a, b| a < b),
            Op::F64Gt => binop!(f64_, fbool, |a, b| a > b),
            Op::F64Le => binop!(f64_, fbool, |a, b| a <= b),
            Op::F64Ge => binop!(f64_, fbool, |a, b| a >= b),

            Op::I32Clz => unop!(u32_, fu32, |a| a.leading_zeros()),
            Op::I32Ctz => unop!(u32_, fu32, |a| a.trailing_zeros()),
            Op::I32Popcnt => unop!(u32_, fu32, |a| a.count_ones()),
            Op::I32Add => binop!(u32_, fu32, |a, b| a.wrapping_add(b)),
            Op::I32Sub => binop!(u32_, fu32, |a, b| a.wrapping_sub(b)),
            Op::I32Mul => binop!(u32_, fu32, |a, b| a.wrapping_mul(b)),
            Op::I32DivS => binop!(i32_, fi32, |a, b| num::i32_div_s(a, b)?),
            Op::I32DivU => binop!(u32_, fu32, |a, b| num::i32_div_u(a, b)?),
            Op::I32RemS => binop!(i32_, fi32, |a, b| num::i32_rem_s(a, b)?),
            Op::I32RemU => binop!(u32_, fu32, |a, b| num::i32_rem_u(a, b)?),
            Op::I32And => binop!(u32_, fu32, |a, b| a & b),
            Op::I32Or => binop!(u32_, fu32, |a, b| a | b),
            Op::I32Xor => binop!(u32_, fu32, |a, b| a ^ b),
            Op::I32Shl => binop!(u32_, fu32, |a, b| a.wrapping_shl(b)),
            Op::I32ShrS => binop!(i32_, fi32, |a, b| a.wrapping_shr(b as u32)),
            Op::I32ShrU => binop!(u32_, fu32, |a, b| a.wrapping_shr(b)),
            Op::I32Rotl => binop!(u32_, fu32, |a, b| a.rotate_left(b)),
            Op::I32Rotr => binop!(u32_, fu32, |a, b| a.rotate_right(b)),
            Op::I64Clz => unop!(u64_, fu64, |a| u64::from(a.leading_zeros())),
            Op::I64Ctz => unop!(u64_, fu64, |a| u64::from(a.trailing_zeros())),
            Op::I64Popcnt => unop!(u64_, fu64, |a| u64::from(a.count_ones())),
            Op::I64Add => binop!(u64_, fu64, |a, b| a.wrapping_add(b)),
            Op::I64Sub => binop!(u64_, fu64, |a, b| a.wrapping_sub(b)),
            Op::I64Mul => binop!(u64_, fu64, |a, b| a.wrapping_mul(b)),
            Op::I64DivS => binop!(i64_, fi64, |a, b| num::i64_div_s(a, b)?),
            Op::I64DivU => binop!(u64_, fu64, |a, b| num::i64_div_u(a, b)?),
            Op::I64RemS => binop!(i64_, fi64, |a, b| num::i64_rem_s(a, b)?),
            Op::I64RemU => binop!(u64_, fu64, |a, b| num::i64_rem_u(a, b)?),
            Op::I64And => binop!(u64_, fu64, |a, b| a & b),
            Op::I64Or => binop!(u64_, fu64, |a, b| a | b),
            Op::I64Xor => binop!(u64_, fu64, |a, b| a ^ b),
            Op::I64Shl => binop!(u64_, fu64, |a, b| a.wrapping_shl(b as u32)),
            Op::I64ShrS => binop!(i64_, fi64, |a, b| a.wrapping_shr(b as u32)),
            Op::I64ShrU => binop!(u64_, fu64, |a, b| a.wrapping_shr(b as u32)),
            Op::I64Rotl => binop!(u64_, fu64, |a, b| a.rotate_left((b & 63) as u32)),
            Op::I64Rotr => binop!(u64_, fu64, |a, b| a.rotate_right((b & 63) as u32)),

            Op::F32Abs => unop!(u32_, fu32, |a| num::fabs32(a)),
            Op::F32Neg => unop!(u32_, fu32, |a| num::fneg32(a)),
            Op::F32Ceil => unop!(f32_, ff32, |a| a.ceil()),
            Op::F32Floor => unop!(f32_, ff32, |a| a.floor()),
            Op::F32Trunc => unop!(f32_, ff32, |a| a.trunc()),
            Op::F32Nearest => unop!(f32_, ff32, |a| a.round_ties_even()),
            Op::F32Sqrt => unop!(f32_, ff32, |a| a.sqrt()),
            Op::F32Add => binop!(f32_, ff32, |a, b| a + b),
            Op::F32Sub => binop!(f32_, ff32, |a, b| a - b),
            Op::F32Mul => binop!(f32_, ff32, |a, b| a * b),
            Op::F32Div => binop!(f32_, ff32, |a, b| a / b),
            Op::F32Min => binop!(f32_, ff32, |a, b| num::fmin32(a, b)),
            Op::F32Max => binop!(f32_, ff32, |a, b| num::fmax32(a, b)),
            Op::F32Copysign => binop!(u32_, fu32, |a, b| num::fcopysign32(a, b)),
            Op::F64Abs => unop!(u64_, fu64, |a| num::fabs64(a)),
            Op::F64Neg => unop!(u64_, fu64, |a| num::fneg64(a)),
            Op::F64Ceil => unop!(f64_, ff64, |a| a.ceil()),
            Op::F64Floor => unop!(f64_, ff64, |a| a.floor()),
            Op::F64Trunc => unop!(f64_, ff64, |a| a.trunc()),
            Op::F64Nearest => unop!(f64_, ff64, |a| a.round_ties_even()),
            Op::F64Sqrt => unop!(f64_, ff64, |a| a.sqrt()),
            Op::F64Add => binop!(f64_, ff64, |a, b| a + b),
            Op::F64Sub => binop!(f64_, ff64, |a, b| a - b),
            Op::F64Mul => binop!(f64_, ff64, |a, b| a * b),
            Op::F64Div => binop!(f64_, ff64, |a, b| a / b),
            Op::F64Min => binop!(f64_, ff64, |a, b| num::fmin64(a, b)),
            Op::F64Max => binop!(f64_, ff64, |a, b| num::fmax64(a, b)),
            Op::F64Copysign => binop!(u64_, fu64, |a, b| num::fcopysign64(a, b)),

            Op::I32WrapI64 => unop!(u64_, fu32, |a| a as u32),
            Op::I32TruncF32S => unop!(f32_, fi32, |a| num::i32_trunc_f32_s(a)?),
            Op::I32TruncF32U => unop!(f32_, fu32, |a| num::i32_trunc_f32_u(a)?),
            Op::I32TruncF64S => unop!(f64_, fi32, |a| num::i32_trunc_f64_s(a)?),
            Op::I32TruncF64U => unop!(f64_, fu32, |a| num::i32_trunc_f64_u(a)?),
            Op::I64ExtendI32S => unop!(i32_, fi64, |a| i64::from(a)),
            Op::I64ExtendI32U => unop!(u32_, fu64, |a| u64::from(a)),
            Op::I64TruncF32S => unop!(f32_, fi64, |a| num::i64_trunc_f32_s(a)?),
            Op::I64TruncF32U => unop!(f32_, fu64, |a| num::i64_trunc_f32_u(a)?),
            Op::I64TruncF64S => unop!(f64_, fi64, |a| num::i64_trunc_f64_s(a)?),
            Op::I64TruncF64U => unop!(f64_, fu64, |a| num::i64_trunc_f64_u(a)?),
            Op::F32ConvertI32S => unop!(i32_, ff32, |a| a as f32),
            Op::F32ConvertI32U => unop!(u32_, ff32, |a| a as f32),
            Op::F32ConvertI64S => unop!(i64_, ff32, |a| a as f32),
            Op::F32ConvertI64U => unop!(u64_, ff32, |a| a as f32),
            Op::F32DemoteF64 => unop!(f64_, ff32, |a| a as f32),
            Op::F64ConvertI32S => unop!(i32_, ff64, |a| f64::from(a)),
            Op::F64ConvertI32U => unop!(u32_, ff64, |a| f64::from(a)),
            Op::F64ConvertI64S => unop!(i64_, ff64, |a| a as f64),
            Op::F64ConvertI64U => unop!(u64_, ff64, |a| a as f64),
            Op::F64PromoteF32 => unop!(f32_, ff64, |a| f64::from(a)),
            // Reinterpretations are no-ops on untyped slots (modulo i32
            // truncation, which readers already perform).
            Op::I32ReinterpretF32
            | Op::I64ReinterpretF64
            | Op::F32ReinterpretI32
            | Op::F64ReinterpretI64 => {}
            Op::I32Extend8S => unop!(u32_, fi32, |a| i32::from(a as u8 as i8)),
            Op::I32Extend16S => unop!(u32_, fi32, |a| i32::from(a as u16 as i16)),
            Op::I64Extend8S => unop!(u64_, fi64, |a| i64::from(a as u8 as i8)),
            Op::I64Extend16S => unop!(u64_, fi64, |a| i64::from(a as u16 as i16)),
            Op::I64Extend32S => unop!(u64_, fi64, |a| i64::from(a as u32 as i32)),
            // Rust's float-to-int `as` casts saturate and map NaN to 0,
            // exactly the `trunc_sat` semantics.
            Op::I32TruncSatF32S => unop!(f32_, fi32, |a| a as i32),
            Op::I32TruncSatF32U => unop!(f32_, fu32, |a| a as u32),
            Op::I32TruncSatF64S => unop!(f64_, fi32, |a| a as i32),
            Op::I32TruncSatF64U => unop!(f64_, fu32, |a| a as u32),
            Op::I64TruncSatF32S => unop!(f32_, fi64, |a| a as i64),
            Op::I64TruncSatF32U => unop!(f32_, fu64, |a| a as u64),
            Op::I64TruncSatF64S => unop!(f64_, fi64, |a| a as i64),
            Op::I64TruncSatF64U => unop!(f64_, fu64, |a| a as u64),
        }
    }
}
