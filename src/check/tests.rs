use super::*;

/// A throwaway `agg/` folder holding `agg.yaml` = `body` and one script judge per name.
fn project(body: &str, judges: &[&str]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("judges")).unwrap();
    for n in judges {
        std::fs::write(tmp.path().join(format!("judges/{n}.sh")), "#!/bin/sh\necho '{\"met\":true}'\n").unwrap();
    }
    std::fs::write(tmp.path().join("agg.yaml"), body).unwrap();
    tmp
}

fn check(body: &str, judges: &[&str]) -> Validation {
    let tmp = project(body, judges);
    validate(tmp.path(), &tmp.path().join("agg.yaml"))
}

fn paths(v: &Validation) -> Vec<&str> {
    v.problems.iter().map(|p| p.path.as_str()).collect()
}

#[test]
fn a_missing_config_is_one_problem_at_the_root() {
    let tmp = tempfile::tempdir().unwrap();
    let v = validate(tmp.path(), &tmp.path().join("agg.yaml"));
    assert_eq!(paths(&v), [""]);
    assert!(v.problems[0].message.contains("agg init"), "{}", v.problems[0].message);
    assert!(v.cfg.is_none());
}

/// THE acceptance criterion: problems from EVERY stage, in one pass — two unknown keys (parse), an
/// unbounded `until:` and a judge with no file (assembly), and an unknown step agent (capability).
#[test]
fn every_stage_reports_in_one_pass() {
    let v = check(
        "project: p\ndefaults: { agnet: claude }\nsteps: { w: { agent: robot, judge_x: 1 } }\n\
         sequence:\n  steps: [w, { step: w, until: goal }]\n  done_if: \"ghost\"\n",
        &["goal"],
    );
    let ps = paths(&v);
    for want in ["defaults.agnet", "steps.w.judge_x", "sequence.steps[1].until", "sequence.done_if", "steps.w.agent"] {
        assert!(ps.contains(&want), "missing `{want}` in {ps:?}");
    }
    let ghost = v.problems.iter().find(|p| p.path == "sequence.done_if").unwrap();
    assert!(ghost.message.contains("ghost"), "the judge problem names the judge: {}", ghost.message);
    assert!(v.cfg.is_some(), "unknown keys are set aside, so the later stages still ran");
    assert!(!v.report(Path::new("x")).ok);
}

/// A type error is not an unknown key: setting the unknown keys aside must still surface it — and
/// every further refused value after it, with the later stages still run on what is left.
#[test]
fn type_errors_beside_unknown_keys_are_all_reported() {
    let v = check(
        "project: p\nbogus: 1\njudge: { timeout: \"soon\" }\nheartbeat_secs: \"often\"\nsteps: { w: {} }\nsequence: { steps: [w], done_if: \"ghost\" }\n",
        &[],
    );
    assert_eq!(&paths(&v)[..4], ["bogus", "judge.timeout", "heartbeat_secs", "sequence.done_if"]);
    assert!(v.problems[1].message.contains("invalid type"), "{}", v.problems[1].message);
    assert!(v.cfg.is_some(), "the refused values fall back to their defaults so checking goes on");
}

/// A refusal at a spot no key can be removed from (a missing required field) ends the parse.
#[test]
fn a_missing_required_field_ends_the_parse() {
    let v = check("defaults: { agent: claude }\n", &[]);
    assert_eq!(paths(&v), [""]);
    assert!(v.problems[0].message.contains("missing field `project`"), "{}", v.problems[0].message);
    let v = check("bogus: 1\n", &[]);
    assert_eq!(paths(&v), ["bogus", ""]);
}

#[test]
fn a_file_that_is_not_yaml_is_one_problem() {
    let v = check("project: [unclosed\n", &[]);
    assert_eq!(v.problems.len(), 1);
    assert!(v.cfg.is_none());
}

/// With no unknown key to set aside, the parse problem is serde's own startup message, located.
#[test]
fn a_lone_type_error_keeps_the_startup_message() {
    let v = check("project: p\njudge: { timeout: \"soon\" }\n", &[]);
    assert_eq!(paths(&v), ["judge.timeout"]);
    assert!(v.problems[0].message.contains("at line 2"), "the startup text carries the location: {}", v.problems[0].message);
}

#[test]
fn a_removed_key_stops_the_check_with_the_migration_message() {
    let v = check("project: p\nsequence: { limits: { wall_hours: 8 } }\n", &[]);
    assert_eq!(paths(&v), ["sequence.limits.wall_hours"]);
    assert!(v.problems[0].message.contains("28800"), "{}", v.problems[0].message);
    let v = check("project: p\nsequence: { abort_if: \"wall_hours >= 8\" }\n", &[]);
    assert_eq!(paths(&v), ["sequence.abort_if"]);
}

#[test]
fn a_valid_config_assembles_and_reports_ok_json() {
    let v = check("project: p\nsummary: { enabled: false }\nsteps: { w: {} }\nsequence: { steps: [w], done_if: \"goal\" }\n", &["goal"]);
    // the agent CLI may be absent on this host; that is the ONLY problem a valid config may have.
    assert!(v.problems.iter().all(|p| p.path == "defaults.agent"), "{:?}", v.problems);
    assert_eq!(v.assembly.as_ref().map(|a| a.engine.judges.len()), Some(1));
    let json = serde_json::to_value(v.report(Path::new("agg/agg.yaml"))).unwrap();
    assert_eq!(json["schema_version"], SCHEMA_VERSION);
    assert_eq!(json["config"], "agg/agg.yaml");
    assert!(json["warnings"].as_array().unwrap().is_empty());
}

#[test]
fn agent_sites_name_each_agent_once_at_its_first_key() {
    let cfg: AggConfig = serde_yaml::from_str(
        "project: p\ndefaults: { agent: claude }\njudge: { agent: claude }\nsteps: { a: { agent: codex }, b: { agent: codex } }\n",
    )
    .unwrap();
    let sites = agent_sites(&cfg);
    assert_eq!(sites, [("defaults.agent".to_string(), "claude".to_string()), ("steps.a.agent".into(), "codex".into())]);
}

#[test]
fn remove_at_follows_keys_and_indices_and_ignores_dead_ends() {
    let mut doc: Yaml = serde_yaml::from_str("a: 1\ns: { steps: [x, { step: w, tims: 2 }] }\n").unwrap();
    assert!(remove_at(&mut doc, "s.steps[1].tims"));
    assert!(remove_at(&mut doc, "a"));
    assert!(!remove_at(&mut doc, "nope.deeper.key"));
    assert!(!remove_at(&mut doc, "s.steps[9].x"));
    assert!(!remove_at(&mut doc, ""), "the root is not a key");
    assert_eq!(doc, serde_yaml::from_str::<Yaml>("s: { steps: [x, { step: w }] }\n").unwrap());
}

#[test]
fn removed_key_path_falls_back_to_the_sequence() {
    let doc: Yaml = serde_yaml::from_str("sequence: { done_if: \"x\" }\n").unwrap();
    assert_eq!(removed_key_path(&doc), "sequence");
}
