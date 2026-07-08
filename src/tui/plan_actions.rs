use super::*;
pub(super) fn run_current_transfer_plan(
    conn: &Connection,
    db_path: &Path,
    job_tx: mpsc::UnboundedSender<TuiMessage>,
    state: &mut AppState,
) -> anyhow::Result<()> {
    if state.collection_result.is_some() {
        state.status =
            "Collection comparison is shown; load a transfer plan to run copies".to_string();
        return Ok(());
    }
    let Some(plan) = state.last_plan.as_ref() else {
        state.status = "No transfer plan to run".to_string();
        return Ok(());
    };
    let copy_entries = plan_copy_count(plan);
    if copy_entries == 0 {
        state.status = "Plan has no copy entries; review conflicts first".to_string();
        return Ok(());
    }
    let plan_id = plan.plan_id.clone();
    db::update_transfer_plan_status(conn, &plan_id, "queued")?;
    if let Some(plan) = state.last_plan.as_mut() {
        plan.status = "queued".to_string();
    }
    state.focus = FocusPane::Plan;
    state.set_status(
        ActivityLevel::Info,
        format!(
            "queued transfer {} ({} copy entries)",
            short_id(&plan_id),
            copy_entries
        ),
    );
    start_next_queued_transfer(conn, db_path, job_tx, state)?;
    Ok(())
}

pub(super) fn start_next_queued_transfer(
    conn: &Connection,
    db_path: &Path,
    job_tx: mpsc::UnboundedSender<TuiMessage>,
    state: &mut AppState,
) -> anyhow::Result<()> {
    if state.transfer_run_plan_id.is_some() {
        return Ok(());
    }
    let Some(plan) = db::queued_transfer_plans(conn, 1)?.into_iter().next() else {
        return Ok(());
    };
    let copy_entries = db::transfer_plan_entries_filtered(conn, &plan.id, Some("copy"))?.len();
    if copy_entries == 0 {
        db::update_transfer_plan_status(conn, &plan.id, "failed")?;
        state.set_status(
            ActivityLevel::Error,
            format!("queued transfer {} has no copy entries", short_id(&plan.id)),
        );
        return Ok(());
    }
    state.transfer_run_plan_id = Some(plan.id.clone());
    state.background_started(format!(
        "running queued transfer {} ({} copy entries)",
        short_id(&plan.id),
        copy_entries
    ));
    spawn_transfer_runner(db_path.to_path_buf(), plan.id, job_tx);
    Ok(())
}

pub(super) fn decide_current_plan_entry(
    conn: &Connection,
    state: &mut AppState,
    action: &str,
    reason: &str,
) -> anyhow::Result<()> {
    if state.focus != FocusPane::Plan {
        state.status = "Move focus to Plan before deciding entries".to_string();
        return Ok(());
    }
    if state.collection_result.is_some() {
        state.status = "Collection comparison entries cannot be decided".to_string();
        return Ok(());
    }
    let Some(plan) = state.last_plan.as_ref() else {
        state.status = "No transfer plan to decide".to_string();
        return Ok(());
    };
    let Some(entry) = plan.entries.get(state.plan_offset) else {
        state.status = "No plan entry selected".to_string();
        return Ok(());
    };
    if entry.action != "review" {
        state.status = format!(
            "{} is {}; only review entries can be decided",
            entry.relative_path, entry.action
        );
        return Ok(());
    }
    let plan_id = plan.plan_id.clone();
    let relative_path = entry.relative_path.clone();
    let changed = db::decide_review_transfer_plan_entry(
        conn,
        &plan_id,
        &relative_path,
        action,
        reason,
        serde_json::json!({
            "decision": action,
            "decided_at": crate::util::now_rfc3339(),
        }),
    )?;
    if !changed {
        state.status = format!("{} is no longer a review entry", relative_path);
        refresh_last_plan(conn, state, &plan_id)?;
        return Ok(());
    }
    refresh_last_plan(conn, state, &plan_id)?;
    state.status = format!("{} -> {}", relative_path, action);
    Ok(())
}

pub(super) fn start_retarget_current_plan_entry(state: &mut AppState) {
    if state.focus != FocusPane::Plan {
        state.status = "Move focus to Plan before retargeting entries".to_string();
        return;
    }
    if state.collection_result.is_some() {
        state.status = "Collection comparison entries cannot be retargeted".to_string();
        return;
    }
    let Some(plan) = state.last_plan.as_ref() else {
        state.status = "No transfer plan to retarget".to_string();
        return;
    };
    let Some(entry) = plan.entries.get(state.plan_offset) else {
        state.status = "No plan entry selected".to_string();
        return;
    };
    if entry.action != "review" {
        state.status = format!(
            "{} is {}; only review entries can be retargeted",
            entry.relative_path, entry.action
        );
        return;
    }
    state.retarget_draft = Some(RetargetDraft {
        plan_id: plan.plan_id.clone(),
        relative_path: entry.relative_path.clone(),
        value: entry.dest_relative_path.clone(),
    });
    state.status = "Edit destination path, Enter applies, Esc cancels".to_string();
}

pub(super) fn handle_retarget_input(
    conn: &Connection,
    state: &mut AppState,
    code: KeyCode,
) -> anyhow::Result<()> {
    match code {
        KeyCode::Esc => {
            state.retarget_draft = None;
            state.status = "retarget canceled".to_string();
        }
        KeyCode::Enter => {
            let Some(draft) = state.retarget_draft.take() else {
                return Ok(());
            };
            let dest = draft.value.trim().to_string();
            if dest.is_empty() {
                state.status = "Destination path cannot be empty".to_string();
                state.retarget_draft = Some(RetargetDraft {
                    value: dest,
                    ..draft
                });
                return Ok(());
            }
            match db::retarget_review_transfer_plan_entry(
                conn,
                &draft.plan_id,
                &draft.relative_path,
                &dest,
            ) {
                Ok(true) => {
                    refresh_last_plan(conn, state, &draft.plan_id)?;
                    state.status = format!("{} -> {}", draft.relative_path, dest);
                }
                Ok(false) => {
                    refresh_last_plan(conn, state, &draft.plan_id)?;
                    state.status = format!("{} is no longer a review entry", draft.relative_path);
                }
                Err(err) => {
                    state.status = format!("retarget failed: {err}");
                    state.retarget_draft = Some(RetargetDraft {
                        value: dest,
                        ..draft
                    });
                }
            }
        }
        KeyCode::Backspace => {
            if let Some(draft) = state.retarget_draft.as_mut() {
                draft.value.pop();
            }
        }
        KeyCode::Char(value) if !value.is_control() => {
            if let Some(draft) = state.retarget_draft.as_mut() {
                draft.value.push(value);
            }
        }
        _ => {}
    }
    Ok(())
}
