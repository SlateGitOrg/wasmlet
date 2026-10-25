//! Runs the vendored WebAssembly 2.0 spec testsuite and checks every file
//! against the committed baseline `tests/spec/expected.tsv`.

use std::path::Path;

use wasmlet_spec::run_file;

#[test]
fn spec_suite_matches_baseline() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/spec");
    let baseline = std::fs::read_to_string(root.join("expected.tsv")).expect("baseline file");
    let mut checked = 0;
    let mut total_passed = 0;
    for line in baseline.lines().filter(|l| !l.starts_with('#')) {
        let cols: Vec<&str> = line.split('\t').collect();
        let n = |i: usize| cols[i].parse::<usize>().expect("numeric column");
        let report = run_file(&root.join("core").join(cols[0])).expect("readable script");
        assert!(
            report.parse_error.is_none(),
            "{}: {:?}",
            cols[0],
            report.parse_error
        );
        assert_eq!(
            (report.passed, report.failed, report.skipped),
            (n(1), n(2), n(3)),
            "{}: failures {:#?}",
            cols[0],
            report.failures
        );
        total_passed += report.passed;
        checked += 1;
    }
    assert_eq!(checked, 90, "every vendored script is in the baseline");
    assert!(total_passed > 27_000);
}
