use std::path::PathBuf;
use std::time::{Duration, Instant};

use tracing_subscriber::EnvFilter;

pub struct DiagnosticsGuard {
    _writer_guard: tracing_appender::non_blocking::WorkerGuard,
}

pub fn init() -> anyhow::Result<(DiagnosticsGuard, PathBuf)> {
    let directory = log_directory()?;
    std::fs::create_dir_all(&directory)?;
    let appender = tracing_appender::rolling::daily(&directory, "gremlin.jsonl");
    let (writer, writer_guard) = tracing_appender::non_blocking(appender);
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("gremlin=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .json()
        .flatten_event(true)
        .with_current_span(true)
        .with_span_list(true)
        .try_init()
        .map_err(|err| anyhow::anyhow!("initializing diagnostics: {err}"))?;
    install_panic_hook();
    Ok((
        DiagnosticsGuard {
            _writer_guard: writer_guard,
        },
        directory,
    ))
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        let location = panic.location();
        let message = panic
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| panic.payload().downcast_ref::<String>().map(String::as_str))
            .unwrap_or("non-string panic payload");
        tracing::error!(
            message,
            file = location.map(|value| value.file()),
            line = location.map(|value| value.line()),
            column = location.map(|value| value.column()),
            "process panicked"
        );
        previous(panic);
    }));
}

pub fn log_directory() -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("GREMLIN_LOG_DIR") {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(path).join("gremlin"));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| anyhow::anyhow!("HOME is unavailable; set GREMLIN_LOG_DIR"))?;
    Ok(PathBuf::from(home).join(".local/state/gremlin"))
}

pub struct OperationTimer {
    operation: &'static str,
    started: Instant,
    warn_after: Duration,
}

impl OperationTimer {
    pub fn start(operation: &'static str, warn_after: Duration) -> Self {
        tracing::debug!(operation, "operation started");
        Self {
            operation,
            started: Instant::now(),
            warn_after,
        }
    }

    pub fn finish(self) -> Duration {
        let elapsed = self.started.elapsed();
        let elapsed_ms = elapsed.as_secs_f64() * 1_000.0;
        if elapsed >= self.warn_after {
            tracing::warn!(
                operation = self.operation,
                elapsed_ms,
                threshold_ms = self.warn_after.as_secs_f64() * 1_000.0,
                "slow operation"
            );
        } else {
            tracing::debug!(operation = self.operation, elapsed_ms, "operation finished");
        }
        elapsed
    }
}
