use assert_cmd::Command;
use serde_json::Value;

fn gremlin() -> Command {
    Command::cargo_bin("gremlin").expect("gremlin binary")
}

fn command_json(args: &[&str]) -> Value {
    let output = gremlin().args(args).assert().success().get_output().clone();
    serde_json::from_slice(&output.stdout).expect("valid json stdout")
}

#[test]
fn scan_hash_verify_emit_json_summaries() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("gremlin.db");
    let root = dir.path().join("root");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("hello.txt"), b"hello").unwrap();

    gremlin()
        .args(["--no-config", "--db", db.to_str().unwrap(), "init"])
        .assert()
        .success();

    let scan = command_json(&[
        "--no-config",
        "--db",
        db.to_str().unwrap(),
        "--json",
        "scan",
        root.to_str().unwrap(),
    ]);
    assert_eq!(scan["files_seen"], 1);
    assert_eq!(scan["new_count"], 1);
    assert_eq!(scan["deltas"][0]["kind"], "new");

    let hash = command_json(&[
        "--no-config",
        "--db",
        db.to_str().unwrap(),
        "--json",
        "hash",
        root.to_str().unwrap(),
    ]);
    assert_eq!(hash["files_hashed"], 1);
    assert_eq!(hash["errors"], 0);
    assert_eq!(hash["hashed_paths"][0], "hello.txt");

    let verify = command_json(&[
        "--no-config",
        "--db",
        db.to_str().unwrap(),
        "--json",
        "verify",
        root.to_str().unwrap(),
    ]);
    assert_eq!(verify["ok"], 1);
    assert_eq!(verify["changed"], 0);
    assert_eq!(verify["findings"][0]["kind"], "ok");
}

#[test]
fn status_emits_json_for_known_and_unknown_targets() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("gremlin.db");
    let root = dir.path().join("root");
    let unknown = dir.path().join("unknown");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&unknown).unwrap();
    std::fs::write(root.join("hello.txt"), b"hello").unwrap();

    gremlin()
        .args(["--no-config", "--db", db.to_str().unwrap(), "init"])
        .assert()
        .success();
    command_json(&[
        "--no-config",
        "--db",
        db.to_str().unwrap(),
        "--json",
        "scan",
        root.to_str().unwrap(),
    ]);

    let known = command_json(&[
        "--no-config",
        "--db",
        db.to_str().unwrap(),
        "--json",
        "status",
        root.to_str().unwrap(),
    ]);
    assert_eq!(known["known"], true);
    assert_eq!(known["kind"], "local_path");
    assert_eq!(known["files"], 1);
    assert_eq!(known["latest_job"]["kind"], "scan");

    let unknown = command_json(&[
        "--no-config",
        "--db",
        db.to_str().unwrap(),
        "--json",
        "status",
        unknown.to_str().unwrap(),
    ]);
    assert_eq!(unknown["known"], false);
    assert_eq!(unknown["kind"], "local_path");
    assert!(unknown["next"].as_str().unwrap().contains("target add"));
}
