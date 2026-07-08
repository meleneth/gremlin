use super::*;
use crate::db::{PathObservationInput, TransferPlanEntryRow};
use rusqlite::Connection;

fn setup() -> (Connection, String, RootRow, RootRow) {
    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
    let source_id = db::ensure_root(&conn, &machine_id, "/tmp/source").unwrap();
    let dest_id = db::ensure_root(&conn, &machine_id, "/tmp/dest").unwrap();
    let source = db::find_root_by_machine_path(&conn, &machine_id, "/tmp/source")
        .unwrap()
        .unwrap();
    let dest = db::find_root_by_machine_path(&conn, &machine_id, "/tmp/dest")
        .unwrap()
        .unwrap();
    assert_eq!(source.id, source_id);
    assert_eq!(dest.id, dest_id);
    (conn, machine_id, source, dest)
}

fn observe(
    conn: &Connection,
    machine_id: &str,
    root_id: &str,
    relative_path: &str,
    size_bytes: u64,
    content_id: Option<&str>,
) {
    db::insert_path_observation(
        conn,
        PathObservationInput {
            machine_id,
            root_id,
            relative_path,
            basename: relative_path.rsplit('/').next().unwrap_or(relative_path),
            parent_path: ".",
            size_bytes,
            modified_at: None,
            content_id,
        },
    )
    .unwrap();
}

fn only_entry(conn: &Connection, plan_id: &str) -> TransferPlanEntryRow {
    let entries = db::transfer_plan_entries(conn, plan_id).unwrap();
    assert_eq!(entries.len(), 1);
    entries.into_iter().next().unwrap()
}

#[test]
fn plans_copy_when_destination_is_missing() {
    let (conn, machine_id, source, dest) = setup();
    observe(&conn, &machine_id, &source.id, "a.txt", 10, None);
    db::toggle_selection_entry(&conn, &source.id, "a.txt").unwrap();

    let result = plan_selected_files(&conn, &source, &dest).unwrap();
    let entry = only_entry(&conn, &result.plan_id);

    assert_eq!(entry.action, "copy");
    assert_eq!(entry.size_bytes, 10);
    let job = db::job_by_id(&conn, &result.job_id).unwrap().unwrap();
    assert_eq!(job.kind, "transfer_plan");
    assert_eq!(job.status, "completed");
    let events = db::events_for_job(&conn, &result.job_id).unwrap();
    assert_eq!(events.first().unwrap().event_kind, "job_created");
    assert_eq!(events.last().unwrap().event_kind, "job_completed");
}

#[test]
fn plans_review_when_destination_root_has_same_content_elsewhere() {
    let (conn, machine_id, source, dest) = setup();
    let content_id = db::ensure_content_object(&conn, 10, "abc", "def").unwrap();
    observe(
        &conn,
        &machine_id,
        &source.id,
        "incoming/foo.png",
        10,
        Some(&content_id),
    );
    observe(
        &conn,
        &machine_id,
        &dest.id,
        "existing/foo.png",
        10,
        Some(&content_id),
    );
    db::toggle_selection_entry(&conn, &source.id, "incoming/foo.png").unwrap();

    let result = plan_selected_files(&conn, &source, &dest).unwrap();
    let entry = only_entry(&conn, &result.plan_id);

    assert_eq!(entry.action, "review");
    assert!(entry.reason.contains("collisions"));
    assert!(entry.metadata_json.contains("existing/foo.png"));
    assert!(entry.metadata_json.contains("hash_collisions"));
}

#[test]
fn plans_review_when_destination_root_has_same_name_size_date_elsewhere() {
    let (conn, machine_id, source, dest) = setup();
    observe(&conn, &machine_id, &source.id, "incoming/foo.png", 10, None);
    observe(&conn, &machine_id, &dest.id, "existing/foo.png", 10, None);
    db::toggle_selection_entry(&conn, &source.id, "incoming/foo.png").unwrap();

    let result = plan_selected_files(&conn, &source, &dest).unwrap();
    let entry = only_entry(&conn, &result.plan_id);

    assert_eq!(entry.action, "review");
    assert!(entry.metadata_json.contains("existing/foo.png"));
    assert!(entry
        .metadata_json
        .contains("filename_size_date_collisions"));
}

#[test]
fn plans_skip_when_content_matches() {
    let (conn, machine_id, source, dest) = setup();
    let content_id = db::ensure_content_object(&conn, 10, "abc", "def").unwrap();
    observe(
        &conn,
        &machine_id,
        &source.id,
        "a.txt",
        10,
        Some(&content_id),
    );
    observe(&conn, &machine_id, &dest.id, "a.txt", 10, Some(&content_id));
    db::toggle_selection_entry(&conn, &source.id, "a.txt").unwrap();

    let result = plan_selected_files(&conn, &source, &dest).unwrap();
    assert_eq!(only_entry(&conn, &result.plan_id).action, "skip");
}

#[test]
fn plans_verify_needed_when_size_matches_without_hash_proof() {
    let (conn, machine_id, source, dest) = setup();
    observe(&conn, &machine_id, &source.id, "a.txt", 10, None);
    observe(&conn, &machine_id, &dest.id, "a.txt", 10, None);
    db::toggle_selection_entry(&conn, &source.id, "a.txt").unwrap();

    let result = plan_selected_files(&conn, &source, &dest).unwrap();
    assert_eq!(only_entry(&conn, &result.plan_id).action, "verify_needed");
}

#[test]
fn plans_conflict_when_destination_differs() {
    let (conn, machine_id, source, dest) = setup();
    observe(&conn, &machine_id, &source.id, "a.txt", 10, None);
    observe(&conn, &machine_id, &dest.id, "a.txt", 20, None);
    db::toggle_selection_entry(&conn, &source.id, "a.txt").unwrap();

    let result = plan_selected_files(&conn, &source, &dest).unwrap();
    assert_eq!(only_entry(&conn, &result.plan_id).action, "conflict");
}

#[test]
fn plans_review_when_unindexed_destination_path_exists_with_same_size() {
    let source_dir = tempfile::tempdir().unwrap();
    let dest_dir = tempfile::tempdir().unwrap();
    std::fs::write(dest_dir.path().join("a.txt"), b"1234567890").unwrap();

    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
    let source_id =
        db::ensure_root(&conn, &machine_id, &source_dir.path().to_string_lossy()).unwrap();
    let dest_id = db::ensure_root(&conn, &machine_id, &dest_dir.path().to_string_lossy()).unwrap();
    observe(&conn, &machine_id, &source_id, "a.txt", 10, None);
    db::toggle_selection_entry(&conn, &source_id, "a.txt").unwrap();
    let source = db::root_by_id(&conn, &source_id).unwrap().unwrap();
    let dest = db::root_by_id(&conn, &dest_id).unwrap().unwrap();

    let result = plan_selected_files(&conn, &source, &dest).unwrap();
    let entry = only_entry(&conn, &result.plan_id);

    assert_eq!(entry.action, "review");
    assert!(entry.reason.contains("not indexed"));
    assert!(entry
        .metadata_json
        .contains("\"dest_observation_source\":\"probe\""));
}

#[test]
fn plans_conflict_when_unindexed_destination_path_exists_with_different_size() {
    let source_dir = tempfile::tempdir().unwrap();
    let dest_dir = tempfile::tempdir().unwrap();
    std::fs::write(dest_dir.path().join("a.txt"), b"different-size").unwrap();

    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
    let source_id =
        db::ensure_root(&conn, &machine_id, &source_dir.path().to_string_lossy()).unwrap();
    let dest_id = db::ensure_root(&conn, &machine_id, &dest_dir.path().to_string_lossy()).unwrap();
    observe(&conn, &machine_id, &source_id, "a.txt", 10, None);
    db::toggle_selection_entry(&conn, &source_id, "a.txt").unwrap();
    let source = db::root_by_id(&conn, &source_id).unwrap().unwrap();
    let dest = db::root_by_id(&conn, &dest_id).unwrap().unwrap();

    let result = plan_selected_files(&conn, &source, &dest).unwrap();
    let entry = only_entry(&conn, &result.plan_id);

    assert_eq!(entry.action, "conflict");
    assert!(entry.reason.contains("exists"));
    assert!(entry
        .metadata_json
        .contains("\"dest_observation_source\":\"probe\""));
}

#[test]
fn plans_copy_when_unindexed_destination_parent_is_missing() {
    let source_dir = tempfile::tempdir().unwrap();
    let dest_dir = tempfile::tempdir().unwrap();

    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
    let source_id =
        db::ensure_root(&conn, &machine_id, &source_dir.path().to_string_lossy()).unwrap();
    let dest_id = db::ensure_root(&conn, &machine_id, &dest_dir.path().to_string_lossy()).unwrap();
    observe(
        &conn,
        &machine_id,
        &source_id,
        "missing/dir/a.txt",
        10,
        None,
    );
    observe(
        &conn,
        &machine_id,
        &source_id,
        "missing/dir/b.txt",
        20,
        None,
    );
    db::toggle_selection_entry(&conn, &source_id, "missing/dir/a.txt").unwrap();
    db::toggle_selection_entry(&conn, &source_id, "missing/dir/b.txt").unwrap();
    let source = db::root_by_id(&conn, &source_id).unwrap().unwrap();
    let dest = db::root_by_id(&conn, &dest_id).unwrap().unwrap();

    let result = plan_selected_files(&conn, &source, &dest).unwrap();
    let entries = db::transfer_plan_entries(&conn, &result.plan_id).unwrap();

    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|entry| entry.action == "copy"));
    assert!(!dest_dir.path().join("missing").exists());
}

#[test]
fn runs_copy_entries_and_updates_destination_projection() {
    let source_dir = tempfile::tempdir().unwrap();
    let dest_dir = tempfile::tempdir().unwrap();
    let source_file = source_dir.path().join("a.txt");
    let source_mtime = filetime::FileTime::from_unix_time(1_700_000_000, 123_000_000);
    std::fs::write(&source_file, b"hello").unwrap();
    filetime::set_file_mtime(&source_file, source_mtime).unwrap();

    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
    let source_path = source_dir.path().to_string_lossy().to_string();
    let dest_path = dest_dir.path().to_string_lossy().to_string();
    let source_id = db::ensure_root(&conn, &machine_id, &source_path).unwrap();
    let dest_id = db::ensure_root(&conn, &machine_id, &dest_path).unwrap();
    let content_id = db::ensure_content_object(
        &conn,
        5,
        blake3::hash(b"hello").to_hex().as_ref(),
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
    )
    .unwrap();
    observe(
        &conn,
        &machine_id,
        &source_id,
        "a.txt",
        5,
        Some(&content_id),
    );
    db::toggle_selection_entry(&conn, &source_id, "a.txt").unwrap();
    let source = db::root_by_id(&conn, &source_id).unwrap().unwrap();
    let dest = db::root_by_id(&conn, &dest_id).unwrap().unwrap();
    let plan = plan_selected_files(&conn, &source, &dest).unwrap();

    let result = run_transfer_plan(&conn, &plan.plan_id, false).unwrap();

    assert_eq!(result.copied, 1);
    assert_eq!(result.errors, 0);
    assert_eq!(
        std::fs::read(dest_dir.path().join("a.txt")).unwrap(),
        b"hello"
    );
    let dest_mtime = filetime::FileTime::from_last_modification_time(
        &std::fs::metadata(dest_dir.path().join("a.txt")).unwrap(),
    );
    assert_eq!(dest_mtime.unix_seconds(), source_mtime.unix_seconds());
    assert_eq!(dest_mtime.nanoseconds(), source_mtime.nanoseconds());
    let dest_obs = db::path_observation_for_root_path(&conn, &dest_id, "a.txt")
        .unwrap()
        .unwrap();
    assert_eq!(dest_obs.size_bytes, 5);
    assert_eq!(dest_obs.content_id.as_deref(), Some(content_id.as_str()));
    assert_eq!(
        dest_obs.modified_at.as_deref(),
        Some("2023-11-14T22:13:20.123+00:00")
    );
    let job = db::job_by_id(&conn, &result.job_id).unwrap().unwrap();
    assert_eq!(job.kind, "transfer_copy");
    assert_eq!(job.status, "completed");
}

#[test]
fn copy_entries_preserve_source_relative_subdirectories() {
    let source_dir = tempfile::tempdir().unwrap();
    let dest_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(source_dir.path().join("some")).unwrap();
    std::fs::write(source_dir.path().join("some/file.png"), b"hello").unwrap();

    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
    let source_path = source_dir.path().to_string_lossy().to_string();
    let dest_path = dest_dir.path().to_string_lossy().to_string();
    let source_id = db::ensure_root(&conn, &machine_id, &source_path).unwrap();
    let dest_id = db::ensure_root(&conn, &machine_id, &dest_path).unwrap();
    let content_id = db::ensure_content_object(
        &conn,
        5,
        blake3::hash(b"hello").to_hex().as_ref(),
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
    )
    .unwrap();
    observe(
        &conn,
        &machine_id,
        &source_id,
        "some/file.png",
        5,
        Some(&content_id),
    );
    db::toggle_selection_entry(&conn, &source_id, "some/file.png").unwrap();
    let source = db::root_by_id(&conn, &source_id).unwrap().unwrap();
    let dest = db::root_by_id(&conn, &dest_id).unwrap().unwrap();
    let plan = plan_selected_files(&conn, &source, &dest).unwrap();
    let entry = only_entry(&conn, &plan.plan_id);
    assert_eq!(entry.relative_path, "some/file.png");
    assert_eq!(entry.dest_relative_path, "some/file.png");

    let result = run_transfer_plan(&conn, &plan.plan_id, false).unwrap();

    assert_eq!(result.copied, 1);
    assert_eq!(
        std::fs::read(dest_dir.path().join("some/file.png")).unwrap(),
        b"hello"
    );
    assert!(!dest_dir.path().join("file.png").exists());
}

#[test]
fn transfer_copy_checkpoint_honors_cancel_request() {
    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
    let source_id = db::ensure_root(&conn, &machine_id, "/tmp/source").unwrap();
    let dest_id = db::ensure_root(&conn, &machine_id, "/tmp/dest").unwrap();
    let plan_id = db::create_transfer_plan(
        &conn,
        None,
        &source_id,
        &dest_id,
        None,
        serde_json::json!({}),
    )
    .unwrap();
    let job_id = db::create_job(
        &conn,
        "transfer_copy",
        Some(&machine_id),
        Some(&source_id),
        serde_json::json!({ "plan_id": plan_id }),
    )
    .unwrap();
    db::start_job(&conn, &job_id).unwrap();
    assert!(db::request_job_cancel(&conn, &job_id).unwrap());
    let mut result = TransferRunResult {
        job_id: job_id.clone(),
        plan_id: plan_id.clone(),
        copied: 1,
        bytes_copied: 5,
        ..TransferRunResult::default()
    };

    assert!(complete_transfer_if_canceled(
        &conn,
        &job_id,
        &plan_id,
        "/tmp/source",
        TransferCancelProgress {
            total_files: 2,
            total_bytes: 10,
            bytes_done: 5,
        },
        &mut result
    )
    .unwrap());

    assert!(result.canceled);
    let job = db::job_by_id(&conn, &job_id).unwrap().unwrap();
    assert_eq!(job.status, "canceled");
    let plan = db::transfer_plan_by_id(&conn, &plan_id).unwrap().unwrap();
    assert_eq!(plan.status, "canceled");
    let events = db::events_for_job(&conn, &job_id).unwrap();
    assert!(events
        .iter()
        .any(|event| event.event_kind == "job_canceled"));
}

#[test]
fn transfer_progress_counts_completed_entries_across_files() {
    let source_dir = tempfile::tempdir().unwrap();
    let dest_dir = tempfile::tempdir().unwrap();
    std::fs::write(source_dir.path().join("a.txt"), b"hello").unwrap();
    std::fs::write(source_dir.path().join("b.txt"), b"world!").unwrap();

    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
    let source_path = source_dir.path().to_string_lossy().to_string();
    let dest_path = dest_dir.path().to_string_lossy().to_string();
    let source_id = db::ensure_root(&conn, &machine_id, &source_path).unwrap();
    let dest_id = db::ensure_root(&conn, &machine_id, &dest_path).unwrap();
    let a_content_id = db::ensure_content_object(
        &conn,
        5,
        blake3::hash(b"hello").to_hex().as_ref(),
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
    )
    .unwrap();
    let b_content_id = db::ensure_content_object(
        &conn,
        6,
        blake3::hash(b"world!").to_hex().as_ref(),
        "711e9609339e92b03ddc0a211827dba421f38f9ed8b9d806e1ffdd8c15ffa03d",
    )
    .unwrap();
    observe(
        &conn,
        &machine_id,
        &source_id,
        "a.txt",
        5,
        Some(&a_content_id),
    );
    observe(
        &conn,
        &machine_id,
        &source_id,
        "b.txt",
        6,
        Some(&b_content_id),
    );
    db::toggle_selection_entry(&conn, &source_id, "a.txt").unwrap();
    db::toggle_selection_entry(&conn, &source_id, "b.txt").unwrap();
    let source = db::root_by_id(&conn, &source_id).unwrap().unwrap();
    let dest = db::root_by_id(&conn, &dest_id).unwrap().unwrap();
    let plan = plan_selected_files(&conn, &source, &dest).unwrap();

    let result = run_transfer_plan(&conn, &plan.plan_id, false).unwrap();

    assert_eq!(result.copied, 2);
    assert_eq!(result.bytes_copied, 11);
    let events = db::events_for_job(&conn, &result.job_id).unwrap();
    let progress = events
        .iter()
        .rev()
        .find_map(|event| {
            let payload: serde_json::Value = serde_json::from_str(&event.payload_json).ok()?;
            (payload.get("type")?.as_str()? == "job_progress").then_some(payload)
        })
        .unwrap();
    assert_eq!(progress.get("bytes_done").unwrap().as_u64(), Some(11));
    assert_eq!(progress.get("bytes_total").unwrap().as_u64(), Some(11));
}

#[test]
fn transfer_progress_counts_skipped_entries_as_completed_work() {
    let source_dir = tempfile::tempdir().unwrap();
    let dest_dir = tempfile::tempdir().unwrap();
    std::fs::write(source_dir.path().join("a.txt"), b"hello").unwrap();
    std::fs::write(source_dir.path().join("b.txt"), b"world!").unwrap();
    std::fs::write(dest_dir.path().join("a.txt"), b"hello").unwrap();

    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
    let source_path = source_dir.path().to_string_lossy().to_string();
    let dest_path = dest_dir.path().to_string_lossy().to_string();
    let source_id = db::ensure_root(&conn, &machine_id, &source_path).unwrap();
    let dest_id = db::ensure_root(&conn, &machine_id, &dest_path).unwrap();
    let a_content_id = db::ensure_content_object(
        &conn,
        5,
        blake3::hash(b"hello").to_hex().as_ref(),
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
    )
    .unwrap();
    let b_content_id = db::ensure_content_object(
        &conn,
        6,
        blake3::hash(b"world!").to_hex().as_ref(),
        "711e9609339e92b03ddc0a211827dba421f38f9ed8b9d806e1ffdd8c15ffa03d",
    )
    .unwrap();
    observe(
        &conn,
        &machine_id,
        &source_id,
        "a.txt",
        5,
        Some(&a_content_id),
    );
    observe(
        &conn,
        &machine_id,
        &source_id,
        "b.txt",
        6,
        Some(&b_content_id),
    );
    db::toggle_selection_entry(&conn, &source_id, "a.txt").unwrap();
    db::toggle_selection_entry(&conn, &source_id, "b.txt").unwrap();
    let source = db::root_by_id(&conn, &source_id).unwrap().unwrap();
    let dest = db::root_by_id(&conn, &dest_id).unwrap().unwrap();
    let plan = plan_selected_files(&conn, &source, &dest).unwrap();
    db::insert_transfer_plan_entry(
        &conn,
        db::TransferPlanEntryInput {
            plan_id: &plan.plan_id,
            relative_path: "a.txt",
            dest_relative_path: Some("a.txt"),
            size_bytes: 5,
            source_content_id: Some(&a_content_id),
            dest_content_id: None,
            action: "copy",
            reason: "test forced copy to exercise runner skip",
            metadata_json: serde_json::json!({}),
        },
    )
    .unwrap();

    let result = run_transfer_plan(&conn, &plan.plan_id, false).unwrap();

    assert_eq!(result.copied, 1);
    assert_eq!(result.skipped, 1);
    assert_eq!(result.bytes_copied, 6);
    let events = db::events_for_job(&conn, &result.job_id).unwrap();
    let progress = events
        .iter()
        .rev()
        .find_map(|event| {
            let payload: serde_json::Value = serde_json::from_str(&event.payload_json).ok()?;
            (payload.get("type")?.as_str()? == "job_progress").then_some(payload)
        })
        .unwrap();
    assert_eq!(progress.get("bytes_done").unwrap().as_u64(), Some(11));
    assert_eq!(progress.get("files_skipped").unwrap().as_u64(), Some(1));
}

#[test]
fn runs_copy_entries_to_retargeted_destination_paths() {
    let source_dir = tempfile::tempdir().unwrap();
    let dest_dir = tempfile::tempdir().unwrap();
    std::fs::write(source_dir.path().join("a.txt"), b"hello").unwrap();

    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
    let source_path = source_dir.path().to_string_lossy().to_string();
    let dest_path = dest_dir.path().to_string_lossy().to_string();
    let source_id = db::ensure_root(&conn, &machine_id, &source_path).unwrap();
    let dest_id = db::ensure_root(&conn, &machine_id, &dest_path).unwrap();
    let content_id = db::ensure_content_object(
        &conn,
        5,
        blake3::hash(b"hello").to_hex().as_ref(),
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
    )
    .unwrap();
    observe(
        &conn,
        &machine_id,
        &source_id,
        "a.txt",
        5,
        Some(&content_id),
    );
    db::toggle_selection_entry(&conn, &source_id, "a.txt").unwrap();
    let source = db::root_by_id(&conn, &source_id).unwrap().unwrap();
    let dest = db::root_by_id(&conn, &dest_id).unwrap().unwrap();
    let plan = plan_selected_files(&conn, &source, &dest).unwrap();
    db::insert_transfer_plan_entry(
        &conn,
        db::TransferPlanEntryInput {
            plan_id: &plan.plan_id,
            relative_path: "a.txt",
            dest_relative_path: Some("renamed/a-copy.txt"),
            size_bytes: 5,
            source_content_id: Some(&content_id),
            dest_content_id: None,
            action: "copy",
            reason: "review retargeted for copy",
            metadata_json: serde_json::json!({ "decision": "retarget" }),
        },
    )
    .unwrap();

    let result = run_transfer_plan(&conn, &plan.plan_id, false).unwrap();

    assert_eq!(result.copied, 1);
    assert_eq!(
        std::fs::read(dest_dir.path().join("renamed/a-copy.txt")).unwrap(),
        b"hello"
    );
    assert!(!dest_dir.path().join("a.txt").exists());
    let dest_obs = db::path_observation_for_root_path(&conn, &dest_id, "renamed/a-copy.txt")
        .unwrap()
        .unwrap();
    assert_eq!(dest_obs.content_id.as_deref(), Some(content_id.as_str()));
}

#[test]
fn copy_fails_when_stream_hash_does_not_match_planned_content() {
    let source_dir = tempfile::tempdir().unwrap();
    let dest_dir = tempfile::tempdir().unwrap();
    std::fs::write(source_dir.path().join("a.txt"), b"hello").unwrap();

    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
    let source_path = source_dir.path().to_string_lossy().to_string();
    let dest_path = dest_dir.path().to_string_lossy().to_string();
    let source_id = db::ensure_root(&conn, &machine_id, &source_path).unwrap();
    let dest_id = db::ensure_root(&conn, &machine_id, &dest_path).unwrap();
    let bad_content_id = db::ensure_content_object(&conn, 5, "bad", "bad").unwrap();
    observe(
        &conn,
        &machine_id,
        &source_id,
        "a.txt",
        5,
        Some(&bad_content_id),
    );
    db::toggle_selection_entry(&conn, &source_id, "a.txt").unwrap();
    let source = db::root_by_id(&conn, &source_id).unwrap().unwrap();
    let dest = db::root_by_id(&conn, &dest_id).unwrap().unwrap();
    let plan = plan_selected_files(&conn, &source, &dest).unwrap();

    let result = run_transfer_plan(&conn, &plan.plan_id, false).unwrap();

    assert_eq!(result.copied, 0);
    assert_eq!(result.errors, 1);
}

#[test]
fn safe_join_rejects_parent_paths() {
    assert!(safe_join("/tmp/root", "../escape.txt").is_err());
    assert!(safe_join("/tmp/root", "/tmp/absolute.txt").is_err());
    assert_eq!(
        safe_join("/tmp/root", "dir/file.txt").unwrap(),
        std::path::Path::new("/tmp/root").join("dir/file.txt")
    );
}

#[test]
fn remote_join_rejects_parent_paths() {
    assert!(remote_join("/srv/root", "../escape.txt").is_err());
    assert!(remote_join("/srv/root", "/tmp/absolute.txt").is_err());
    assert_eq!(
        remote_join("/srv/root", "dir/file.txt").unwrap(),
        "/srv/root/dir/file.txt"
    );
    assert_eq!(remote_join("~", "dir/file.txt").unwrap(), "~/dir/file.txt");
}

#[test]
fn ssh_root_remote_path_strips_matching_host_prefix() {
    assert_eq!(
        ssh_root_remote_path("nas01", "nas01:/srv/archive/photos"),
        "/srv/archive/photos"
    );
    assert_eq!(
        ssh_root_remote_path("nas01", "/srv/archive/photos"),
        "/srv/archive/photos"
    );
    assert_eq!(
        ssh_root_remote_path("nas02", "nas01:/srv/archive/photos"),
        "nas01:/srv/archive/photos"
    );
}

#[test]
fn shell_quote_handles_single_quotes() {
    assert_eq!(shell_quote("/srv/has space"), "'/srv/has space'");
    assert_eq!(shell_quote("/srv/it's"), "'/srv/it'\\''s'");
    assert_eq!(remote_shell_path("~"), "$HOME");
    assert_eq!(remote_shell_path("~/dir/it's"), "$HOME/'dir/it'\\''s'");
}

#[test]
fn transfer_chunks_use_fixed_size_offsets_with_remainder() {
    let chunk_size = crate::fswork::DEFAULT_CHUNK_SIZE_BYTES;
    let chunks = transfer_chunks(chunk_size * 2 + 7);
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].index, 0);
    assert_eq!(chunks[0].offset, 0);
    assert_eq!(chunks[0].size, chunk_size);
    assert_eq!(chunks[1].index, 1);
    assert_eq!(chunks[1].offset, chunk_size);
    assert_eq!(chunks[1].size, chunk_size);
    assert_eq!(chunks[2].index, 2);
    assert_eq!(chunks[2].offset, chunk_size * 2);
    assert_eq!(chunks[2].size, 7);
}

#[test]
fn chunk_progress_message_names_state_and_position() {
    let chunk = TransferChunk {
        index: 1,
        offset: crate::fswork::DEFAULT_CHUNK_SIZE_BYTES,
        size: crate::fswork::DEFAULT_CHUNK_SIZE_BYTES,
    };

    assert_eq!(
        chunk_progress_message(chunk, 4, "reused local checkpoint after MD5 verify"),
        format!(
            "2/4 reused local checkpoint after MD5 verify offset={} size={}",
            crate::fswork::DEFAULT_CHUNK_SIZE_BYTES,
            crate::fswork::DEFAULT_CHUNK_SIZE_BYTES
        )
    );
}

#[test]
fn transfer_copy_chunk_checkpoints_round_trip() {
    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
    let source_id = db::ensure_root(&conn, &machine_id, "/tmp/source").unwrap();
    let dest_id = db::ensure_root(&conn, &machine_id, "/tmp/dest").unwrap();
    let plan_id = db::create_transfer_plan(
        &conn,
        None,
        &source_id,
        &dest_id,
        None,
        serde_json::json!({}),
    )
    .unwrap();
    let entry = db::TransferPlanEntryRow {
        relative_path: "some/file.bin".to_string(),
        dest_relative_path: "some/file.bin".to_string(),
        size_bytes: 7,
        source_content_id: None,
        dest_content_id: None,
        action: "copy".to_string(),
        reason: "test".to_string(),
        metadata_json: "{}".to_string(),
    };
    let chunk = TransferChunk {
        index: 2,
        offset: crate::fswork::DEFAULT_CHUNK_SIZE_BYTES * 2,
        size: 7,
    };

    persist_copy_chunk_checkpoint(&conn, "job_1", &plan_id, &entry, chunk, "abc").unwrap();
    let checkpoint = matching_copy_chunk_checkpoint(&conn, &plan_id, &entry, chunk)
        .unwrap()
        .unwrap();

    assert_eq!(checkpoint.digest, "abc");
    assert_eq!(checkpoint.offset_bytes, chunk.offset);
    assert_eq!(checkpoint.size_bytes, chunk.size);
    assert_eq!(
        db::transfer_copy_chunk_count_for_entry(
            &conn,
            &plan_id,
            &entry.relative_path,
            &entry.dest_relative_path
        )
        .unwrap(),
        1
    );
}

#[test]
fn paranoid_syncs_file_and_parent_directory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    std::fs::write(&path, b"hello").unwrap();
    sync_for_paranoid_readback(&path, Some(dir.path().to_path_buf())).unwrap();
}
