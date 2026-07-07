use rusqlite::Connection;

use crate::db::{self, RootRow, TransferPlanActionSummary};

#[derive(Debug, Clone)]
pub struct TransferPlanResult {
    pub plan_id: String,
    pub selection_set_id: String,
    pub marked_count: i64,
    pub marked_bytes: i64,
    pub summary: Vec<TransferPlanActionSummary>,
}

pub fn plan_selected_files(
    conn: &Connection,
    source_root: &RootRow,
    dest_root: &RootRow,
) -> anyhow::Result<TransferPlanResult> {
    if source_root.id == dest_root.id {
        anyhow::bail!("source and destination roots are the same root");
    }

    let selection = db::selection_summary_for_root(conn, &source_root.id)?;
    let selected = db::selected_paths_for_root(conn, &source_root.id)?;
    if selected.is_empty() {
        anyhow::bail!("source root has no marked files; mark files in the TUI with Space first");
    }

    let plan_id = db::create_transfer_plan(
        conn,
        &source_root.id,
        &dest_root.id,
        Some(&selection.set_id),
        serde_json::json!({
            "source_root_id": source_root.id,
            "source_path": source_root.path,
            "dest_root_id": dest_root.id,
            "dest_path": dest_root.path,
            "selection_set_id": selection.set_id,
        }),
    )?;

    for relative_path in selected {
        let source = db::path_observation_for_root_path(conn, &source_root.id, &relative_path)?;
        let Some(source) = source else {
            db::insert_transfer_plan_entry(
                conn,
                db::TransferPlanEntryInput {
                    plan_id: &plan_id,
                    relative_path: &relative_path,
                    size_bytes: 0,
                    source_content_id: None,
                    dest_content_id: None,
                    action: "unavailable",
                    reason: "marked path is no longer indexed on source root",
                    metadata_json: serde_json::json!({}),
                },
            )?;
            continue;
        };

        let dest = db::path_observation_for_root_path(conn, &dest_root.id, &relative_path)?;
        let (action, reason, dest_content_id) = match dest.as_ref() {
            None => ("copy", "destination path is not indexed", None),
            Some(dest) => {
                match (
                    source.content_id.as_deref(),
                    dest.content_id.as_deref(),
                    source.size_bytes == dest.size_bytes,
                ) {
                    (Some(source_id), Some(dest_id), _) if source_id == dest_id => {
                        ("skip", "destination content already matches", Some(dest_id))
                    }
                    (_, _, true) => (
                        "verify_needed",
                        "destination has the same size but lacks matching hash proof",
                        dest.content_id.as_deref(),
                    ),
                    _ => (
                        "conflict",
                        "destination path exists with different projected content",
                        dest.content_id.as_deref(),
                    ),
                }
            }
        };

        db::insert_transfer_plan_entry(
            conn,
            db::TransferPlanEntryInput {
                plan_id: &plan_id,
                relative_path: &source.relative_path,
                size_bytes: source.size_bytes,
                source_content_id: source.content_id.as_deref(),
                dest_content_id,
                action,
                reason,
                metadata_json: serde_json::json!({
                    "source_modified_at": source.modified_at,
                    "dest_modified_at": dest.as_ref().and_then(|row| row.modified_at.clone()),
                }),
            },
        )?;
    }

    let summary = db::transfer_plan_action_summary(conn, &plan_id)?;
    Ok(TransferPlanResult {
        plan_id,
        selection_set_id: selection.set_id,
        marked_count: selection.marked_count,
        marked_bytes: selection.marked_bytes,
        summary,
    })
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::db::{PathObservationInput, TransferPlanEntryRow};

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
}
