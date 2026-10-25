;; Sieve of Eratosthenes over linear memory: counts primes below n.
(module
  (memory 16 256)
  (func (export "sieve") (param $n i32) (result i32)
    (local $i i32) (local $j i32) (local $count i32) (local $pages i32)
    ;; grow memory so that n bytes fit
    (local.set $pages (i32.add (i32.shr_u (local.get $n) (i32.const 16)) (i32.const 1)))
    (if (i32.gt_u (local.get $pages) (memory.size))
      (then (drop (memory.grow (i32.sub (local.get $pages) (memory.size))))))
    (memory.fill (i32.const 0) (i32.const 1) (local.get $n))
    (local.set $i (i32.const 2))
    (block $outer_done
      (loop $outer
        (br_if $outer_done (i32.gt_u (i32.mul (local.get $i) (local.get $i)) (local.get $n)))
        (if (i32.load8_u (local.get $i))
          (then
            (local.set $j (i32.mul (local.get $i) (local.get $i)))
            (block $inner_done
              (loop $inner
                (br_if $inner_done (i32.ge_u (local.get $j) (local.get $n)))
                (i32.store8 (local.get $j) (i32.const 0))
                (local.set $j (i32.add (local.get $j) (local.get $i)))
                (br $inner)))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $outer)))
    (local.set $i (i32.const 2))
    (block $count_done
      (loop $count_loop
        (br_if $count_done (i32.ge_u (local.get $i) (local.get $n)))
        (local.set $count (i32.add (local.get $count) (i32.load8_u (local.get $i))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $count_loop)))
    (local.get $count)))
