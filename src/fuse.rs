//! Peephole fusion of common op sequences into superinstructions.
//!
//! Runs once per function after lowering. The interpreter's cost is
//! dominated by dispatch (one `match` per op) and operand-stack traffic, so
//! replacing frequent short sequences by a single op that reads its operands
//! directly pays off in tight loops. Patterns, tried longest first at each
//! position:
//!
//! | sequence                                          | becomes              |
//! |---------------------------------------------------|----------------------|
//! | `local.get s; const c; i32/i64.add/sub; local.set d` | `I{32,64}LocalAddImm` |
//! | `<i32/i64 compare>; br_if` (nothing to unwind)     | `BrIf<cmp>`          |
//! | `<i32/i64 compare>; if` (inverted compare)         | `BrIf<!cmp>`         |
//! | `i32.eqz`/`i64.eqz` followed by `br_if`/`if`       | `BrIfEqz`/`BrIfNez`/`BrIfI64*` |
//! | `const c; <add/sub/mul/and/shl/shr>`               | `<op>Imm(c)`         |
//! | `local.get a; local.get b`                         | `LocalGet2(a, b)`    |
//!
//! A window is only fused if none of its ops except the first is a branch
//! target, so control can never enter the middle of a fused op. Branch
//! targets are then renumbered through an old-index -> new-index map.
//! `br_table` and its entries are never part of a window, so they stay
//! contiguous.
//!
//! Fuel accounting is unchanged: every fused op containing a branch burns
//! exactly the one unit its branch would have burned.
//!
//! Complexity: O(n) time and O(n) extra space in the function's op count.

use crate::ir::{DropKeep, Op};

/// Fuses `code` and returns the shorter program.
pub(crate) fn fuse(code: Vec<Op>) -> Vec<Op> {
    let n = code.len();
    let mut is_target = vec![false; n + 1];
    for op in &code {
        if let Some(t) = op.branch_target() {
            is_target[t as usize] = true;
        }
    }
    let mut out = Vec::with_capacity(n);
    let mut map = vec![u32::MAX; n + 1];
    let mut i = 0;
    while i < n {
        let window = &code[i..];
        // Interior ops of a window must not be jump targets.
        let free = |k: usize| window.len() >= k && !is_target[i + 1..i + k].contains(&true);
        let (op, len) = match_at(window, free);
        map[i] = out.len() as u32;
        out.push(op);
        i += len;
    }
    map[n] = out.len() as u32;
    for op in &mut out {
        if let Some(t) = op.branch_target_mut() {
            let new = map[*t as usize];
            debug_assert_ne!(new, u32::MAX, "branch into the middle of a fused op");
            *t = new;
        }
    }
    out
}

fn match_at(w: &[Op], free: impl Fn(usize) -> bool) -> (Op, usize) {
    if free(4) {
        if let Some(op) = local_add_imm(&w[..4]) {
            return (op, 4);
        }
    }
    if free(2) {
        // Leave `local.get` alone when it starts a local-add window next.
        let next_is_add = w.len() >= 5 && local_add_imm(&w[1..5]).is_some();
        let fused = compare_branch(w[0], w[1]).or_else(|| match next_is_add {
            true => None,
            false => pair(w[0], w[1]),
        });
        if let Some(op) = fused {
            return (op, 2);
        }
    }
    (w[0], 1)
}

fn local_add_imm(w: &[Op]) -> Option<Op> {
    let (Op::LocalGet(src), Op::Const(c), op, Op::LocalSet(dst)) = (w[0], w[1], w[2], w[3]) else {
        return None;
    };
    match op {
        Op::I32Add => Some(Op::I32LocalAddImm {
            dst,
            src,
            imm: c as u32 as i32,
        }),
        Op::I32Sub => Some(Op::I32LocalAddImm {
            dst,
            src,
            imm: (c as u32 as i32).wrapping_neg(),
        }),
        Op::I64Add | Op::I64Sub => {
            let v = c as i64;
            let v = if op == Op::I64Sub {
                v.wrapping_neg()
            } else {
                v
            };
            let imm = i32::try_from(v).ok()?;
            Some(Op::I64LocalAddImm { dst, src, imm })
        }
        _ => None,
    }
}

/// `<cmp>; br_if` jumps when the comparison holds; `<cmp>; if` (lowered to
/// `BrIfEqz`) jumps when it does not.
fn compare_branch(cmp: Op, br: Op) -> Option<Op> {
    let (target, when_true) = match br {
        Op::BrIfNez {
            target,
            dk: DropKeep { drop: 0, .. },
        } => (target, true),
        Op::BrIfEqz { target } => (target, false),
        _ => return None,
    };
    // (op if the comparison holds, op if it does not)
    let (yes, no) = match cmp {
        Op::I32Eqz => (
            Op::BrIfEqz { target },
            Op::BrIfNez {
                target,
                dk: DropKeep::default(),
            },
        ),
        Op::I64Eqz => (Op::BrIfI64Eqz { target }, Op::BrIfI64Nez { target }),
        Op::I32Eq => (Op::BrIfI32Eq { target }, Op::BrIfI32Ne { target }),
        Op::I32Ne => (Op::BrIfI32Ne { target }, Op::BrIfI32Eq { target }),
        Op::I32LtS => (Op::BrIfI32LtS { target }, Op::BrIfI32GeS { target }),
        Op::I32LtU => (Op::BrIfI32LtU { target }, Op::BrIfI32GeU { target }),
        Op::I32GtS => (Op::BrIfI32GtS { target }, Op::BrIfI32LeS { target }),
        Op::I32GtU => (Op::BrIfI32GtU { target }, Op::BrIfI32LeU { target }),
        Op::I32LeS => (Op::BrIfI32LeS { target }, Op::BrIfI32GtS { target }),
        Op::I32LeU => (Op::BrIfI32LeU { target }, Op::BrIfI32GtU { target }),
        Op::I32GeS => (Op::BrIfI32GeS { target }, Op::BrIfI32LtS { target }),
        Op::I32GeU => (Op::BrIfI32GeU { target }, Op::BrIfI32LtU { target }),
        Op::I64Eq => (Op::BrIfI64Eq { target }, Op::BrIfI64Ne { target }),
        Op::I64Ne => (Op::BrIfI64Ne { target }, Op::BrIfI64Eq { target }),
        Op::I64LtS => (Op::BrIfI64LtS { target }, Op::BrIfI64GeS { target }),
        Op::I64LtU => (Op::BrIfI64LtU { target }, Op::BrIfI64GeU { target }),
        Op::I64GtS => (Op::BrIfI64GtS { target }, Op::BrIfI64LeS { target }),
        Op::I64GtU => (Op::BrIfI64GtU { target }, Op::BrIfI64LeU { target }),
        Op::I64LeS => (Op::BrIfI64LeS { target }, Op::BrIfI64GtS { target }),
        Op::I64LeU => (Op::BrIfI64LeU { target }, Op::BrIfI64GtU { target }),
        Op::I64GeS => (Op::BrIfI64GeS { target }, Op::BrIfI64LtS { target }),
        Op::I64GeU => (Op::BrIfI64GeU { target }, Op::BrIfI64LtU { target }),
        _ => return None,
    };
    Some(if when_true { yes } else { no })
}

fn pair(a: Op, b: Op) -> Option<Op> {
    match (a, b) {
        (Op::LocalGet(x), Op::LocalGet(y)) => Some(Op::LocalGet2(x, y)),
        (Op::Const(c), op) => {
            let c32 = c as u32 as i32;
            Some(match op {
                Op::I32Add => Op::I32AddImm(c32),
                Op::I32Sub => Op::I32AddImm(c32.wrapping_neg()),
                Op::I32Mul => Op::I32MulImm(c32),
                Op::I32And => Op::I32AndImm(c32),
                Op::I32Shl => Op::I32ShlImm(c as u32 & 31),
                Op::I32ShrS => Op::I32ShrSImm(c as u32 & 31),
                Op::I32ShrU => Op::I32ShrUImm(c as u32 & 31),
                Op::I64Add => Op::I64AddImm(c as i64),
                Op::I64Sub => Op::I64AddImm((c as i64).wrapping_neg()),
                Op::I64Mul => Op::I64MulImm(c as i64),
                Op::I64And => Op::I64AndImm(c as i64),
                Op::I64Shl => Op::I64ShlImm((c & 63) as u32),
                Op::I64ShrS => Op::I64ShrSImm((c & 63) as u32),
                Op::I64ShrU => Op::I64ShrUImm((c & 63) as u32),
                _ => return None,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dk(drop: u32, keep: u32) -> DropKeep {
        DropKeep { drop, keep }
    }

    #[test]
    fn loop_counter_and_compare_branch() {
        // 0: local.get 0; 1: local.get 1; 2: i32.ge_u; 3: br_if @9
        // 4: local.get 0; 5: const 1; 6: i32.add; 7: local.set 0; 8: br @0; 9: return
        let code = vec![
            Op::LocalGet(0),
            Op::LocalGet(1),
            Op::I32GeU,
            Op::BrIfNez {
                target: 9,
                dk: dk(0, 0),
            },
            Op::LocalGet(0),
            Op::Const(1),
            Op::I32Add,
            Op::LocalSet(0),
            Op::Br {
                target: 0,
                dk: dk(0, 0),
            },
            Op::Return { keep: 0 },
        ];
        assert_eq!(
            fuse(code),
            vec![
                Op::LocalGet2(0, 1),
                Op::BrIfI32GeU { target: 4 },
                Op::I32LocalAddImm {
                    dst: 0,
                    src: 0,
                    imm: 1
                },
                Op::Br {
                    target: 0,
                    dk: dk(0, 0)
                },
                Op::Return { keep: 0 },
            ]
        );
    }

    #[test]
    fn interior_branch_targets_block_fusion() {
        // A branch to index 1 forbids fusing ops 0 and 1.
        let code = vec![
            Op::LocalGet(0),
            Op::LocalGet(1),
            Op::Drop,
            Op::Br {
                target: 1,
                dk: dk(0, 0),
            },
        ];
        assert_eq!(fuse(code.clone()), code);
    }

    #[test]
    fn if_inverts_the_comparison_and_unwinding_branches_stay() {
        let code = vec![
            Op::I64LtS,
            Op::BrIfEqz { target: 4 },
            Op::I32Eqz,
            Op::BrIfNez {
                target: 4,
                dk: dk(1, 1),
            },
            Op::I32Eqz,
            Op::BrIfEqz { target: 0 },
            Op::I64Eqz,
            Op::BrIfNez {
                target: 0,
                dk: dk(0, 1),
            },
        ];
        assert_eq!(
            fuse(code),
            vec![
                Op::BrIfI64GeS { target: 3 },
                Op::I32Eqz,
                Op::BrIfNez {
                    target: 3,
                    dk: dk(1, 1)
                },
                Op::BrIfNez {
                    target: 0,
                    dk: DropKeep::default()
                },
                Op::BrIfI64Eqz { target: 0 },
            ]
        );
    }

    #[test]
    fn every_comparison_has_both_polarities() {
        let cmps = [
            Op::I32Eqz,
            Op::I64Eqz,
            Op::I32Eq,
            Op::I32Ne,
            Op::I32LtS,
            Op::I32LtU,
            Op::I32GtS,
            Op::I32GtU,
            Op::I32LeS,
            Op::I32LeU,
            Op::I32GeS,
            Op::I32GeU,
            Op::I64Eq,
            Op::I64Ne,
            Op::I64LtS,
            Op::I64LtU,
            Op::I64GtS,
            Op::I64GtU,
            Op::I64LeS,
            Op::I64LeU,
            Op::I64GeS,
            Op::I64GeU,
        ];
        for c in cmps {
            let yes = compare_branch(
                c,
                Op::BrIfNez {
                    target: 7,
                    dk: dk(0, 2),
                },
            )
            .unwrap();
            let no = compare_branch(c, Op::BrIfEqz { target: 7 }).unwrap();
            assert_ne!(yes, no);
            assert_eq!(
                (yes.branch_target(), no.branch_target()),
                (Some(7), Some(7))
            );
        }
        assert_eq!(compare_branch(Op::F32Lt, Op::BrIfEqz { target: 0 }), None);
        assert_eq!(compare_branch(Op::I32LtS, Op::Drop), None);
    }

    #[test]
    fn immediates_fold_sub_and_mask_shifts() {
        let pairs = [
            (Op::I32Sub, 5u64, Op::I32AddImm(-5)),
            (Op::I32Add, 0xffff_ffff, Op::I32AddImm(-1)),
            (Op::I32Mul, 3, Op::I32MulImm(3)),
            (Op::I32And, 0xff, Op::I32AndImm(0xff)),
            (Op::I32Shl, 33, Op::I32ShlImm(1)),
            (Op::I32ShrS, 2, Op::I32ShrSImm(2)),
            (Op::I32ShrU, 2, Op::I32ShrUImm(2)),
            (Op::I64Add, u64::MAX, Op::I64AddImm(-1)),
            (Op::I64Sub, 1, Op::I64AddImm(-1)),
            (Op::I64Mul, 9, Op::I64MulImm(9)),
            (Op::I64And, 1, Op::I64AndImm(1)),
            (Op::I64Shl, 65, Op::I64ShlImm(1)),
            (Op::I64ShrS, 3, Op::I64ShrSImm(3)),
            (Op::I64ShrU, 3, Op::I64ShrUImm(3)),
        ];
        for (op, c, want) in pairs {
            assert_eq!(fuse(vec![Op::Const(c), op]), vec![want], "{op:?}");
        }
        assert_eq!(fuse(vec![Op::Const(1), Op::I32DivS]).len(), 2);
    }

    #[test]
    fn local_add_imm_variants() {
        let w = |op, c| vec![Op::LocalGet(2), Op::Const(c), op, Op::LocalSet(3)];
        assert_eq!(
            fuse(w(Op::I32Sub, 4)),
            vec![Op::I32LocalAddImm {
                dst: 3,
                src: 2,
                imm: -4
            }]
        );
        assert_eq!(
            fuse(w(Op::I64Sub, 4)),
            vec![Op::I64LocalAddImm {
                dst: 3,
                src: 2,
                imm: -4
            }]
        );
        assert_eq!(
            fuse(w(Op::I64Add, 7)),
            vec![Op::I64LocalAddImm {
                dst: 3,
                src: 2,
                imm: 7
            }]
        );
        // An i64 immediate that does not fit in i32 falls back to smaller fusions.
        assert_eq!(
            fuse(w(Op::I64Add, 1 << 40)),
            vec![Op::LocalGet(2), Op::I64AddImm(1 << 40), Op::LocalSet(3)]
        );
        assert_eq!(fuse(w(Op::I32Mul, 2)).len(), 3);
        // `local.get a` before a local-add window does not steal its first op.
        let mut code = vec![Op::LocalGet(9)];
        code.extend(w(Op::I32Add, 1));
        assert_eq!(
            fuse(code),
            vec![
                Op::LocalGet(9),
                Op::I32LocalAddImm {
                    dst: 3,
                    src: 2,
                    imm: 1
                }
            ]
        );
    }
}
