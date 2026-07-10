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

pub(super) fn start_drop_queued_transfer_confirmation(
    selected_plan: Option<&db::TransferPlanRow>,
    state: &mut AppState,
) {
    let Some(plan) = selected_plan else {
        state.status = "Select a queued transfer resume row to drop".to_string();
        return;
    };
    match plan.status.as_str() {
        "queued" => {
            state.pending_drop_transfer_plan_id = Some(plan.id.clone());
            state.set_status(
                ActivityLevel::Warning,
                format!("Drop queued transfer {}? y confirms", short_id(&plan.id)),
            );
        }
        "running" => {
            state.status = "Transfer is running; press c to request cancellation".to_string();
        }
        "canceled" => {
            state.status = format!("transfer {} is already canceled", short_id(&plan.id));
        }
        status => {
            state.status = format!(
                "transfer {} is {status}; it is not queued",
                short_id(&plan.id)
            );
        }
    }
}

pub(super) fn handle_drop_queued_transfer_confirmation(
    conn: &Connection,
    state: &mut AppState,
    code: KeyCode,
) -> anyhow::Result<()> {
    let Some(plan_id) = state.pending_drop_transfer_plan_id.clone() else {
        return Ok(());
    };
    match code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            state.pending_drop_transfer_plan_id = None;
            let Some(plan) = db::transfer_plan_by_id(conn, &plan_id)? else {
                state.set_status(
                    ActivityLevel::Error,
                    format!("queued transfer {} no longer exists", short_id(&plan_id)),
                );
                return Ok(());
            };
            if plan.status != "queued" {
                state.set_status(
                    ActivityLevel::Warning,
                    format!(
                        "transfer {} is now {}; not dropped",
                        short_id(&plan_id),
                        plan.status
                    ),
                );
                return Ok(());
            }
            db::update_transfer_plan_status(conn, &plan_id, "canceled")?;
            if let Some(plan) = state
                .last_plan
                .as_mut()
                .filter(|plan| plan.plan_id == plan_id)
            {
                plan.status = "canceled".to_string();
            }
            state.set_status(
                ActivityLevel::Warning,
                format!("dropped queued transfer {}", short_id(&plan_id)),
            );
        }
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
            state.pending_drop_transfer_plan_id = None;
            state.status = "drop queued transfer canceled".to_string();
        }
        _ => {
            state.status = "Confirm drop with y, or cancel with n/Esc".to_string();
        }
    }
    Ok(())
}

pub(super) fn request_selected_resume_transfer_cancel(
    conn: &Connection,
    selected_plan: Option<&db::TransferPlanRow>,
    state: &mut AppState,
) -> anyhow::Result<bool> {
    let Some(plan) = selected_plan else {
        return Ok(false);
    };
    match plan.status.as_str() {
        "queued" => {
            start_drop_queued_transfer_confirmation(Some(plan), state);
            Ok(true)
        }
        "running" => {
            let Some(job_id) = plan.job_id.as_deref() else {
                state.set_status(
                    ActivityLevel::Error,
                    format!("running transfer {} has no job id", short_id(&plan.id)),
                );
                return Ok(true);
            };
            if db::request_job_cancel(conn, job_id)? {
                let envelope = crate::events::EventEnvelope {
                    event_kind: crate::events::EventKind::JobCancelRequested,
                    job_id: Some(job_id.to_string()),
                    sequence: None,
                    created_at: crate::util::now_rfc3339(),
                    payload: crate::events::EventPayload::Job {
                        kind: "transfer_copy".to_string(),
                        path: Some(plan.source_path.clone()),
                        message: Some("cancel requested from resume row".to_string()),
                        files_seen: Some(plan.entry_count as u64),
                        errors: None,
                    },
                };
                db::persist_event(conn, &envelope)?;
                state.set_status(
                    ActivityLevel::Warning,
                    format!("cancel requested for transfer {}", short_id(&plan.id)),
                );
            } else {
                state.status = format!("transfer {} is not cancelable", short_id(&plan.id));
            }
            Ok(true)
        }
        "canceled" => {
            state.status = format!("transfer {} is already canceled", short_id(&plan.id));
            Ok(true)
        }
        _ => Ok(false),
    }
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
