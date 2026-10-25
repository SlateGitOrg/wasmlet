;; Dense n x n f64 matrix multiply C = A * B with A[i][j] = i + j and
;; B[i][j] = i - j; returns the sum of C. Exercises f64 loads/stores/arith.
(module
  (memory 4 64)
  (func $idx (param $base i32) (param $n i32) (param $i i32) (param $j i32) (result i32)
    (i32.add (local.get $base)
      (i32.shl (i32.add (i32.mul (local.get $i) (local.get $n)) (local.get $j)) (i32.const 3))))
  (func (export "matmul") (param $n i32) (result f64)
    (local $i i32) (local $j i32) (local $k i32) (local $acc f64) (local $sum f64)
    (local $a i32) (local $b i32) (local $c i32) (local $bytes i32)
    (local.set $bytes (i32.shl (i32.mul (local.get $n) (local.get $n)) (i32.const 3)))
    (local.set $a (i32.const 0))
    (local.set $b (local.get $bytes))
    (local.set $c (i32.mul (local.get $bytes) (i32.const 2)))
    (if (i32.gt_u (i32.mul (local.get $bytes) (i32.const 3)) (i32.shl (memory.size) (i32.const 16)))
      (then (drop (memory.grow (i32.add (i32.shr_u (i32.mul (local.get $bytes) (i32.const 3)) (i32.const 16)) (i32.const 1))))))
    ;; init
    (local.set $i (i32.const 0))
    (loop $li
      (local.set $j (i32.const 0))
      (loop $lj
        (f64.store (call $idx (local.get $a) (local.get $n) (local.get $i) (local.get $j))
          (f64.convert_i32_s (i32.add (local.get $i) (local.get $j))))
        (f64.store (call $idx (local.get $b) (local.get $n) (local.get $i) (local.get $j))
          (f64.convert_i32_s (i32.sub (local.get $i) (local.get $j))))
        (br_if $lj (i32.lt_u (local.tee $j (i32.add (local.get $j) (i32.const 1))) (local.get $n))))
      (br_if $li (i32.lt_u (local.tee $i (i32.add (local.get $i) (i32.const 1))) (local.get $n))))
    ;; multiply
    (local.set $i (i32.const 0))
    (loop $mi
      (local.set $j (i32.const 0))
      (loop $mj
        (local.set $acc (f64.const 0))
        (local.set $k (i32.const 0))
        (loop $mk
          (local.set $acc (f64.add (local.get $acc)
            (f64.mul
              (f64.load (call $idx (local.get $a) (local.get $n) (local.get $i) (local.get $k)))
              (f64.load (call $idx (local.get $b) (local.get $n) (local.get $k) (local.get $j))))))
          (br_if $mk (i32.lt_u (local.tee $k (i32.add (local.get $k) (i32.const 1))) (local.get $n))))
        (f64.store (call $idx (local.get $c) (local.get $n) (local.get $i) (local.get $j)) (local.get $acc))
        (local.set $sum (f64.add (local.get $sum) (local.get $acc)))
        (br_if $mj (i32.lt_u (local.tee $j (i32.add (local.get $j) (i32.const 1))) (local.get $n))))
      (br_if $mi (i32.lt_u (local.tee $i (i32.add (local.get $i) (i32.const 1))) (local.get $n))))
    (local.get $sum)))
