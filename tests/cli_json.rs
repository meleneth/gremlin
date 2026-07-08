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

#[test]
fn target_remove_requires_yes_and_removes_root_records_only() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("gremlin.db");
    let root = dir.path().join("root");
    std::fs::create_dir(&root).unwrap();
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

    gremlin()
        .args([
            "--no-config",
            "--db",
            db.to_str().unwrap(),
            "target",
            "remove",
            root.to_str().unwrap(),
        ])
        .assert()
        .success();
    let still_known = command_json(&[
        "--no-config",
        "--db",
        db.to_str().unwrap(),
        "--json",
        "status",
        root.to_str().unwrap(),
    ]);
    assert_eq!(still_known["known"], true);

    gremlin()
        .args([
            "--no-config",
            "--db",
            db.to_str().unwrap(),
            "target",
            "rm",
            root.to_str().unwrap(),
            "--yes",
        ])
        .assert()
        .success();
    assert_eq!(std::fs::read(root.join("hello.txt")).unwrap(), b"hello");
    let removed = command_json(&[
        "--no-config",
        "--db",
        db.to_str().unwrap(),
        "--json",
        "status",
        root.to_str().unwrap(),
    ]);
    assert_eq!(removed["known"], false);
}

#[test]
fn import_events_can_project_into_default_ssh_target() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("gremlin.db");
    let jsonl = dir.path().join("remote.jsonl");
    let event = serde_json::json!({
        "event_kind": "hash_completed",
        "job_id": "job_remote",
        "sequence": 1,
        "created_at": "2026-07-07T00:00:00Z",
        "payload": {
            "type": "hash_completed",
            "relative_path": "folder/a.txt",
            "basename": "a.txt",
            "parent_path": "folder",
            "size_bytes": 5,
            "modified_at": "2026-07-07T00:00:00Z",
            "blake3": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "sha256": "ssssssssssssssssssssssssssssssssssssssssssssssssssssssssssssssss"
        }
    });
    std::fs::write(&jsonl, format!("{}\n", event)).unwrap();

    gremlin()
        .args(["--no-config", "--db", db.to_str().unwrap(), "init"])
        .assert()
        .success();
    gremlin()
        .args([
            "--no-config",
            "--db",
            db.to_str().unwrap(),
            "import-events",
            jsonl.to_str().unwrap(),
            "--target",
            "nas01:",
        ])
        .assert()
        .success();

    let status = command_json(&[
        "--no-config",
        "--db",
        db.to_str().unwrap(),
        "--json",
        "status",
        "nas01:",
    ]);
    assert_eq!(status["known"], true);
    assert_eq!(status["kind"], "ssh");
    assert_eq!(status["path"], "~");
    assert_eq!(status["files"], 1);
    assert_eq!(status["content_objects"], 1);
    assert_eq!(status["latest_job"]["kind"], "import_events");

    let root_listing = gremlin()
        .args([
            "--no-config",
            "--db",
            db.to_str().unwrap(),
            "target",
            "ls",
            "nas01:",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let root_listing = String::from_utf8(root_listing).unwrap();
    assert!(root_listing.contains("dir:\tfolder\tfolder\t1 files\t5 B"));

    let folder_listing = gremlin()
        .args([
            "--no-config",
            "--db",
            db.to_str().unwrap(),
            "target",
            "ls",
            "nas01:",
            "--path",
            "folder",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let folder_listing = String::from_utf8(folder_listing).unwrap();
    assert!(folder_listing.contains("file:\ta.txt\tfolder/a.txt\t5 B\tpresent"));
}

#[test]
fn verify_collection_compares_imported_hashes_to_root() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("gremlin.db");
    let root = dir.path().join("root");
    let jsonl = dir.path().join("checksums.jsonl");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("ok.txt"), b"same").unwrap();
    std::fs::write(root.join("changed.txt"), b"old!").unwrap();
    std::fs::write(root.join("missing.txt"), b"gone").unwrap();

    gremlin()
        .args([
            "worker",
            "hash",
            root.to_str().unwrap(),
            "--jsonl",
            "--out",
            jsonl.to_str().unwrap(),
        ])
        .assert()
        .success();
    gremlin()
        .args(["--no-config", "--db", db.to_str().unwrap(), "init"])
        .assert()
        .success();
    let import_output = gremlin()
        .args([
            "--no-config",
            "--db",
            db.to_str().unwrap(),
            "import-events",
            jsonl.to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let import_output = String::from_utf8(import_output).unwrap();
    let collection_id = import_output
        .split_whitespace()
        .last()
        .expect("collection id in import output");

    std::fs::write(root.join("changed.txt"), b"new!").unwrap();
    std::fs::remove_file(root.join("missing.txt")).unwrap();
    std::fs::write(root.join("extra.txt"), b"extra").unwrap();
    command_json(&[
        "--no-config",
        "--db",
        db.to_str().unwrap(),
        "--json",
        "hash",
        root.to_str().unwrap(),
        "--all",
    ]);

    let summary = command_json(&[
        "--no-config",
        "--db",
        db.to_str().unwrap(),
        "--json",
        "verify-collection",
        collection_id,
        root.to_str().unwrap(),
    ]);
    assert_eq!(summary["entries"], 3);
    assert_eq!(summary["ok"], 1);
    assert_eq!(summary["hash_mismatch"], 1);
    assert_eq!(summary["missing"], 1);
    assert_eq!(summary["extras"], 1);
    assert_eq!(summary["findings"][0]["relative_path"], "changed.txt");
    assert_eq!(summary["extra_files"][0]["relative_path"], "extra.txt");
}

#[test]
fn positional_ssh_target_can_run_without_tui() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("gremlin.db");

    let output = gremlin()
        .args([
            "--no-config",
            "--no-tui",
            "--db",
            db.to_str().unwrap(),
            "nas01:",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("target Ssh"));
    assert!(output.contains("root=temporary"));
    assert!(output.contains("path=~"));
    assert!(!output.contains("warning:\tSSH fastscan failed"));
    assert!(!output.contains("empty:\t."));

    let status = command_json(&[
        "--no-config",
        "--db",
        db.to_str().unwrap(),
        "--json",
        "status",
        "nas01:",
    ]);
    assert_eq!(status["known"], false);
    assert_eq!(status["kind"], "ssh");
    assert_eq!(status["path"], "~");
}
