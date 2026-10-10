//! `agg schema` is checked by an INDEPENDENT validator (the `jsonschema` crate), never by agg's own
//! walker: the claim under test is that the published schema says what the parser does.
//!
//! Two directions, both against real files: every config agg SHIPS (the examples, and what
//! `agg init` scaffolds for each agent) validates; and a misspelled key fails the schema AND the
//! parser, so the schema is exactly as strict as `deny_unknown_fields`.

#![cfg(unix)]

use assert_cmd::cargo::CommandCargoExt;
use std::path::Path;
use std::process::Command;

/// The schema exactly as a consumer gets it: the stdout of `agg schema`.
fn published_schema() -> serde_json::Value {
    let out = Command::cargo_bin("agg").unwrap().arg("schema").output().unwrap();
    assert!(out.status.success(), "agg schema failed: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).expect("agg schema prints JSON")
}

fn validator() -> jsonschema::Validator {
    jsonschema::validator_for(&published_schema()).expect("agg schema is itself a valid JSON Schema")
}

/// The validator's complaints about `yaml` (empty = valid).
fn errors(v: &jsonschema::Validator, yaml: &str) -> Vec<String> {
    let doc: serde_json::Value = serde_yaml::from_str(yaml).expect("the document is YAML");
    v.iter_errors(&doc).map(|e| format!("{} at {}", e, e.instance_path())).collect()
}

#[test]
fn the_schema_is_versioned_and_closed() {
    let s = published_schema();
    assert_eq!(s["$schema"], "https://json-schema.org/draft/2020-12/schema");
    assert!(s["$comment"].as_str().unwrap().contains(env!("CARGO_PKG_VERSION")), "the schema names the agg that made it");
    assert_eq!(s["additionalProperties"], false);
}

/// Every config file in the repo that agg ships for people to copy.
#[test]
fn the_shipped_examples_validate() {
    let v = validator();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for rel in ["examples/workflow.yaml", "examples/hello-agg/agg/agg.yaml", "examples/p-vs-np/agg/agg.yaml"] {
        let text = std::fs::read_to_string(root.join(rel)).unwrap();
        let errs = errors(&v, &text);
        assert!(errs.is_empty(), "{rel} must validate against `agg schema`:\n{}", errs.join("\n"));
    }
}

/// `plugin/scaffold/agg.yaml` is a TEMPLATE with `{{…}}` holes, so it is validated the way users
/// receive it — rendered by `agg init`, once per agent (the three renderings differ).
#[test]
fn the_scaffold_validates_for_every_agent() {
    let v = validator();
    for agent in ["claude", "codex", "copilot"] {
        let tmp = tempfile::tempdir().unwrap();
        let out = Command::cargo_bin("agg")
            .unwrap()
            .current_dir(tmp.path())
            .env("HOME", tmp.path())
            .args(["init", "--agent", agent])
            .output()
            .unwrap();
        assert!(out.status.success(), "init --agent {agent}: {}", String::from_utf8_lossy(&out.stderr));
        let text = std::fs::read_to_string(tmp.path().join("agg/agg.yaml")).unwrap();
        let errs = errors(&v, &text);
        assert!(errs.is_empty(), "the {agent} scaffold must validate:\n{}", errs.join("\n"));
    }
}

/// The acceptance criterion's other half, at every depth: a misspelled key is refused by the
/// schema AND by the parser `agg run` uses — the two must never disagree about strictness.
#[test]
fn a_misspelled_key_fails_both_the_schema_and_the_parser() {
    let v = validator();
    for bad in [
        "project: \"p\"\nprojet: \"p\"\n",
        "project: \"p\"\ndefaults: { agnet: \"claude\" }\n",
        "project: \"p\"\nsteps: { w: { judge_model: \"x\" } }\n",
        "project: \"p\"\nsequence: { steps: [{ step: \"w\", tims: 2 }] }\n",
        "project: \"p\"\nsequence: { limits: { token: 5 } }\n",
    ] {
        assert!(!errors(&v, bad).is_empty(), "the schema must reject:\n{bad}");
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("agg.yaml");
        std::fs::write(&p, bad).unwrap();
        let err = agg::core::config::AggConfig::load(&p).expect_err("the parser must reject it too");
        assert!(format!("{err:#}").contains("unknown field"), "{err:#}");
    }
}

/// The good twin of the test above, so a schema that rejects EVERYTHING cannot pass it.
#[test]
fn a_minimal_config_validates() {
    assert!(errors(&validator(), "project: \"p\"\n").is_empty());
}
