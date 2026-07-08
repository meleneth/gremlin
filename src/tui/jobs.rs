use super::*;
pub(super) fn refresh_last_plan(
    conn: &Connection,
    state: &mut AppState,
    plan_id: &str,
) -> anyhow::Result<()> {
    if let Some(plan) = state
        .last_plan
        .as_mut()
        .filter(|plan| plan.plan_id == plan_id)
    {
        if let Some(row) = db::transfer_plan_by_id(conn, plan_id)? {
            plan.status = row.status;
        }
        plan.summary = db::transfer_plan_action_summary(conn, plan_id)?;
        plan.entries = db::transfer_plan_entries(conn, plan_id)?;
        if state.plan_offset >= plan.entries.len() {
            state.plan_offset = plan.entries.len().saturating_sub(1);
        }
    }
    Ok(())
}

pub(super) fn spawn_job_runner(
    db_path: PathBuf,
    job_id: String,
    kind: String,
    machine_label: Option<String>,
    job_tx: mpsc::UnboundedSender<TuiMessage>,
) {
    task::spawn_blocking(move || {
        let result = (|| -> anyhow::Result<()> {
            let conn = db::open_existing(&db_path)?;
            fswork::run_queued_job(
                &conn,
                &job_id,
                &db_path,
                machine_label.as_deref(),
                OutputOptions {
                    quiet: true,
                    ..OutputOptions::default()
                },
            )
        })();
        let message = match result {
            Ok(()) => format!("completed {kind} job {job_id}"),
            Err(err) => format!("failed {kind} job {job_id}: {err}"),
        };
        let _ = job_tx.send(TuiMessage::Status(message));
    });
}

pub(super) fn spawn_transfer_runner(
    db_path: PathBuf,
    plan_id: String,
    job_tx: mpsc::UnboundedSender<TuiMessage>,
) {
    task::spawn_blocking(move || {
        let result = (|| -> anyhow::Result<transfer::TransferRunResult> {
            let conn = db::open_existing(&db_path)?;
            transfer::run_transfer_plan(&conn, &plan_id, false)
        })();
        let status = match result {
            Ok(result) if result.canceled => {
                format!(
                    "canceled transfer {}: copied {} ({}) skipped {} errors {}",
                    short_id(&result.plan_id),
                    result.copied,
                    human_size(result.bytes_copied),
                    result.skipped,
                    result.errors
                )
            }
            Ok(result) => {
                format!(
                    "completed transfer {}: copied {} ({}) skipped {} errors {}",
                    short_id(&result.plan_id),
                    result.copied,
                    human_size(result.bytes_copied),
                    result.skipped,
                    result.errors
                )
            }
            Err(err) => format!("failed transfer {}: {err}", short_id(&plan_id)),
        };
        let _ = job_tx.send(TuiMessage::TransferFinished { plan_id, status });
    });
}
