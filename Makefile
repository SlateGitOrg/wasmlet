# wasmlet — every gate behind one command: `make check`.
#
# CARGO_TARGET_DIR and CARGO_BUILD_JOBS are taken from the environment when
# set (cargo reads them directly), so a shared target dir works unchanged.

CARGO ?= cargo
# Line-coverage floor for the core library (src/), measured over the unit,
# integration, CLI and spec-suite tests.
COVERAGE_MIN ?= 90
SEED ?= 1
FUZZ_DECODE ?= 2000000
FUZZ_DIFF ?= 50000
COV_PKGS = --workspace --exclude wasmlet-bench --exclude wasmlet-fuzz
COV_IGNORE = --ignore-filename-regex '(crates/|tests/)'

.PHONY: check fmt fmt-check lint doc test test-fuzz coverage coverage-report \
        spec spec-plain fuzz bench demo verify-spec fixtures clean

## check: format, lint, docs, all tests and the coverage floor
check: fmt-check lint doc coverage test-fuzz verify-spec
	@echo "make check: all gates passed"

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all -- --check

## lint: clippy over every crate and target, warnings are errors
lint:
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings

## doc: rustdoc for the library with warnings as errors, plus doctests
doc:
	RUSTDOCFLAGS="-D warnings" $(CARGO) doc --no-deps --locked -p wasmlet
	$(CARGO) test --doc --locked -p wasmlet

## test: all tests without coverage instrumentation
test:
	$(CARGO) test --workspace --locked

## coverage: run the tests instrumented and fail under COVERAGE_MIN% lines in src/
coverage:
	$(CARGO) llvm-cov $(COV_PKGS) --locked $(COV_IGNORE) --summary-only \
	  --fail-under-lines $(COVERAGE_MIN)

## coverage-report: per-file coverage table for the whole workspace
coverage-report:
	$(CARGO) llvm-cov $(COV_PKGS) --locked --summary-only

## test-fuzz: the fixed-seed fuzz smoke tests (decoder + differential)
test-fuzz:
	$(CARGO) test --locked -p wasmlet-fuzz

## spec: per-file pass counts on the pinned spec testsuite, checked against the baseline
spec:
	$(CARGO) run --release --locked -p wasmlet-spec -- --expect tests/spec/expected.tsv tests/spec/core

## spec-plain: the same, compiled without superinstructions
spec-plain:
	$(CARGO) run --release --locked -p wasmlet-spec -- --no-fuse --expect tests/spec/expected.tsv tests/spec/core

## fuzz: a long fuzz campaign (override SEED, FUZZ_DECODE, FUZZ_DIFF)
fuzz:
	$(CARGO) run --release --locked -p wasmlet-fuzz -- --seed $(SEED) \
	  --decode $(FUZZ_DECODE) --diff $(FUZZ_DIFF) --artifacts fuzz-artifacts

## bench: wasmlet vs wasmi on six workloads (release build, median of 7)
bench:
	$(CARGO) run --release --locked -p wasmlet-bench -- --runs 7

## demo: offline end-to-end demo on the bundled fixtures
demo:
	./scripts/demo.sh

verify-spec:
	./scripts/verify-spec.sh

## fixtures: rebuild the rustc guests (needs the wasm32-wasip1 target; not part of check)
fixtures:
	./scripts/build-fixtures.sh

clean:
	$(CARGO) clean
