//! A short, fixed-seed slice of both fuzz campaigns, run on every `make check`.
//! Longer campaigns: `make fuzz` (see the README for the numbers).

use std::path::Path;

use wasmlet_fuzz::{decode_campaign, differential_campaign, load_corpus};

fn root() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

#[test]
fn decoder_and_validator_survive_mutated_inputs() {
    let corpus = load_corpus(root());
    assert!(corpus.len() > 3_000, "spec modules and fixtures are loaded");
    let stats = decode_campaign(0xdec0de, 20_000, &corpus);
    assert_eq!(stats.inputs, 20_000);
    assert!(stats.accepted > 50, "some mutants stay valid: {stats:?}");
    assert!(stats.findings.is_empty(), "{:#?}", stats.findings);
}

#[test]
fn wasmlet_agrees_with_wasmi_on_generated_modules() {
    let stats = differential_campaign(0xd1ff, 600);
    assert!(stats.compared > 400, "{stats:?}");
    assert!(stats.findings.is_empty(), "{:#?}", stats.findings);
}
