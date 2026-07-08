use super::*;

pub(super) fn verify_latest_collection_for_root(
    conn: &Connection,
    root: Option<&db::RootRow>,
    state: &mut AppState,
) -> anyhow::Result<()> {
    let Some(root) = root else {
        state.set_status(
            ActivityLevel::Warning,
            "No root selected for collection compare",
        );
        return Ok(());
    };
    let Some(collection) = db::latest_checksum_collection_for_root(conn, &root.id)? else {
        state.set_status(
            ActivityLevel::Warning,
            "No checksum collections found for this root",
        );
        return Ok(());
    };
    let summary = collections::verify_collection_against_root(conn, &collection.id, root)?;
    let ok = summary.ok;
    let problems =
        summary.missing + summary.size_mismatch + summary.hash_mismatch + summary.unverified;
    let snapshot = CollectionSnapshot::from(summary);
    state.collection_result = Some(snapshot);
    state.plan_offset = 0;
    state.focus = FocusPane::Plan;
    state.set_status(
        if problems == 0 {
            ActivityLevel::Success
        } else {
            ActivityLevel::Warning
        },
        format!(
            "collection {} ({}, {}) compared: ok {} issues {} extra {}",
            short_id(&collection.id),
            truncate(&collection.name, 18),
            collection_scope_label(&collection),
            ok,
            problems,
            state
                .collection_result
                .as_ref()
                .map(|collection| collection.extras)
                .unwrap_or(0)
        ),
    );
    Ok(())
}

fn collection_scope_label(collection: &db::RecentChecksumCollectionRow) -> String {
    let scope = if collection.root_id.is_some() {
        "root"
    } else {
        "unattached"
    };
    let imported = collection.imported_at.as_deref().unwrap_or("-");
    format!("{} {}", collection.source_kind, truncate(imported, 10)).replace('\n', " ")
        + &format!(" {scope}")
}
