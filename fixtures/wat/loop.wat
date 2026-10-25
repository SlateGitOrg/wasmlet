;; Tight arithmetic loop: sum of (i * i) mod 7 for i in [0, n), in i64.
(module
  (func (export "sum") (param $n i64) (result i64)
    (local $i i64) (local $acc i64)
    (block $done
      (loop $next
        (br_if $done (i64.ge_u (local.get $i) (local.get $n)))
        (local.set $acc
          (i64.add (local.get $acc)
            (i64.rem_u (i64.mul (local.get $i) (local.get $i)) (i64.const 7))))
        (local.set $i (i64.add (local.get $i) (i64.const 1)))
        (br $next)))
    (local.get $acc)))
