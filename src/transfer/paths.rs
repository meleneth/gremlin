use super::*;
pub(super) fn transfer_temp_path(dest_path: &Path) -> PathBuf {
    let file_name = dest_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("copy");
    dest_path.with_file_name(format!(".{file_name}.gremlin-part"))
}

pub(super) fn remote_path_exists(host: &str, path: &str) -> anyhow::Result<bool> {
    let status = Command::new("ssh")
        .arg(host)
        .arg(format!("test -e {}", remote_shell_path(path)))
        .status()
        .with_context(|| format!("checking remote path {host}:{path}"))?;
    Ok(status.success())
}

pub(super) fn ensure_dest_parent(dest_path: &Path) -> anyhow::Result<Option<PathBuf>> {
    let Some(parent) = dest_path.parent() else {
        return Ok(None);
    };
    std::fs::create_dir_all(parent)
        .with_context(|| format!("creating destination directory {}", parent.display()))?;
    Ok(Some(parent.to_path_buf()))
}

pub(super) fn insert_dest_observation(
    conn: &Connection,
    dest_root: &RootRow,
    entry: &db::TransferPlanEntryRow,
    content_id: Option<&str>,
    modified_at: Option<&str>,
) -> rusqlite::Result<()> {
    let base = basename(Path::new(&entry.dest_relative_path))
        .unwrap_or_else(|_| entry.dest_relative_path.clone());
    db::insert_path_observation(
        conn,
        db::PathObservationInput {
            machine_id: &dest_root.machine_id,
            root_id: &dest_root.id,
            relative_path: &entry.dest_relative_path,
            basename: &base,
            parent_path: &parent_path(&entry.dest_relative_path),
            size_bytes: entry.size_bytes,
            modified_at,
            content_id,
        },
    )
}

pub(super) fn root_transfer_endpoint(
    conn: &Connection,
    root: &RootRow,
) -> anyhow::Result<TransferEndpoint> {
    if root.machine_id == local_machine_id() {
        return Ok(TransferEndpoint::Local(PathBuf::from(&root.path)));
    }
    let machine = db::machine_by_id(conn, &root.machine_id)?
        .ok_or_else(|| anyhow::anyhow!("machine not found for root {}", root.id))?;
    if machine.platform.as_deref() == Some("ssh") {
        let path = ssh_root_remote_path(&machine.label, &root.path);
        return Ok(TransferEndpoint::Ssh {
            host: machine.label,
            path,
        });
    }
    anyhow::bail!(
        "transfer run does not support machine {} ({})",
        machine.id,
        machine.platform.as_deref().unwrap_or("unknown")
    )
}

pub(super) fn ssh_root_remote_path(host: &str, root_path: &str) -> String {
    root_path
        .strip_prefix(&format!("{host}:"))
        .unwrap_or(root_path)
        .to_string()
}

pub(super) fn endpoint_join(
    root: &TransferEndpoint,
    relative_path: &str,
) -> anyhow::Result<TransferEndpoint> {
    match root {
        TransferEndpoint::Local(root) => Ok(TransferEndpoint::Local(safe_join(
            root.to_string_lossy().as_ref(),
            relative_path,
        )?)),
        TransferEndpoint::Ssh { host, path } => Ok(TransferEndpoint::Ssh {
            host: host.clone(),
            path: remote_join(path, relative_path)?,
        }),
    }
}

pub(super) fn probe_destination_observation(
    endpoint: &TransferEndpoint,
    relative_path: &str,
    cache: &mut DestinationProbeCache,
) -> anyhow::Result<Option<DestinationObservation>> {
    for parent in relative_parent_prefixes(relative_path) {
        if cache.missing_dirs.iter().any(|missing| {
            parent == *missing
                || parent
                    .strip_prefix(missing)
                    .is_some_and(|rest| rest.starts_with('/'))
        }) {
            return Ok(None);
        }
        if cache.existing_dirs.contains(&parent) {
            continue;
        }
        match probe_endpoint_path(&endpoint_join(endpoint, &parent)?)? {
            EndpointPathKind::Missing => {
                cache.missing_dirs.insert(parent);
                return Ok(None);
            }
            EndpointPathKind::Directory => {
                cache.existing_dirs.insert(parent);
            }
            EndpointPathKind::File {
                size_bytes,
                modified_at,
            }
            | EndpointPathKind::Other {
                size_bytes,
                modified_at,
            } => {
                return Ok(Some(DestinationObservation {
                    size_bytes,
                    modified_at,
                    content_id: None,
                    source: DestinationObservationSource::Probe,
                    conflict_reason: Some("destination parent path exists but is not a directory"),
                }));
            }
        }
    }

    match probe_endpoint_path(&endpoint_join(endpoint, relative_path)?)? {
        EndpointPathKind::Missing => Ok(None),
        EndpointPathKind::File {
            size_bytes,
            modified_at,
        } => Ok(Some(DestinationObservation {
            size_bytes,
            modified_at,
            content_id: None,
            source: DestinationObservationSource::Probe,
            conflict_reason: None,
        })),
        EndpointPathKind::Directory => Ok(Some(DestinationObservation {
            size_bytes: 0,
            modified_at: None,
            content_id: None,
            source: DestinationObservationSource::Probe,
            conflict_reason: Some("destination path exists as a directory"),
        })),
        EndpointPathKind::Other {
            size_bytes,
            modified_at,
        } => Ok(Some(DestinationObservation {
            size_bytes,
            modified_at,
            content_id: None,
            source: DestinationObservationSource::Probe,
            conflict_reason: Some("destination path exists but is not a regular file"),
        })),
    }
}

pub(super) fn relative_parent_prefixes(relative_path: &str) -> Vec<String> {
    let parts = Path::new(relative_path)
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().to_string()),
            Component::CurDir => None,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => None,
        })
        .collect::<Vec<_>>();
    if parts.len() <= 1 {
        return Vec::new();
    }
    (1..parts.len()).map(|len| parts[..len].join("/")).collect()
}

pub(super) fn probe_endpoint_path(endpoint: &TransferEndpoint) -> anyhow::Result<EndpointPathKind> {
    match endpoint {
        TransferEndpoint::Local(path) => probe_local_path(path),
        TransferEndpoint::Ssh { host, path } => probe_remote_path(host, path),
    }
}

pub(super) fn probe_local_path(path: &Path) -> anyhow::Result<EndpointPathKind> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(EndpointPathKind::Directory),
        Ok(metadata) if metadata.is_file() => Ok(EndpointPathKind::File {
            size_bytes: metadata.len(),
            modified_at: metadata.modified().ok().map(system_time_rfc3339),
        }),
        Ok(metadata) => Ok(EndpointPathKind::Other {
            size_bytes: metadata.len(),
            modified_at: metadata.modified().ok().map(system_time_rfc3339),
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(EndpointPathKind::Missing),
        Err(err) => Err(err).with_context(|| format!("checking destination {}", path.display())),
    }
}

pub(super) fn probe_remote_path(host: &str, path: &str) -> anyhow::Result<EndpointPathKind> {
    let command = format!(
        "if test ! -e {path}; then exit 3; elif test -d {path}; then printf 'dir\\n'; elif test -f {path}; then printf 'file '; stat -c '%s' {path}; else printf 'other '; stat -c '%s' {path}; fi",
        path = remote_shell_path(path)
    );
    let output = Command::new("ssh")
        .arg(host)
        .arg(command)
        .output()
        .with_context(|| format!("checking remote destination {host}:{path}"))?;
    if output.status.code() == Some(3) {
        return Ok(EndpointPathKind::Missing);
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "remote destination probe failed for {host}:{path}: {}",
            stderr.trim()
        );
    }
    let stdout = String::from_utf8(output.stdout).context("remote stat output was not UTF-8")?;
    let mut fields = stdout.split_whitespace();
    match fields.next() {
        Some("dir") => Ok(EndpointPathKind::Directory),
        Some("file") => Ok(EndpointPathKind::File {
            size_bytes: parse_remote_stat_size(fields.next(), host, path)?,
            modified_at: None,
        }),
        Some("other") => Ok(EndpointPathKind::Other {
            size_bytes: parse_remote_stat_size(fields.next(), host, path)?,
            modified_at: None,
        }),
        _ => anyhow::bail!("remote stat produced invalid output for {host}:{path}"),
    }
}

pub(super) fn parse_remote_stat_size(
    value: Option<&str>,
    host: &str,
    path: &str,
) -> anyhow::Result<u64> {
    value
        .ok_or_else(|| anyhow::anyhow!("remote stat produced no size for {host}:{path}"))?
        .parse::<u64>()
        .with_context(|| format!("remote stat produced invalid size for {host}:{path}"))
}

pub(super) fn remote_join(root: &str, relative_path: &str) -> anyhow::Result<String> {
    let rel = Path::new(relative_path);
    if rel.is_absolute() {
        anyhow::bail!("refusing absolute transfer path: {relative_path}");
    }
    let mut parts = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(value) => parts.push(value.to_string_lossy().to_string()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("refusing unsafe transfer path: {relative_path}");
            }
        }
    }
    if parts.is_empty() {
        anyhow::bail!("empty transfer path");
    }
    let suffix = parts.join("/");
    let root = root.trim_end_matches('/');
    if root.is_empty() || root == "." {
        Ok(suffix)
    } else {
        Ok(format!("{root}/{suffix}"))
    }
}

pub(super) fn remote_parent(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(parent, _)| {
            if parent.is_empty() {
                "/".to_string()
            } else {
                parent.to_string()
            }
        })
        .unwrap_or_else(|| ".".to_string())
}

pub(super) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(super) fn remote_shell_path(path: &str) -> String {
    if path == "~" {
        "$HOME".to_string()
    } else if let Some(rest) = path.strip_prefix("~/") {
        format!("$HOME/{}", shell_quote(rest))
    } else {
        shell_quote(path)
    }
}

pub(super) fn rate_per_second(bytes: u64, started_at: Instant) -> f64 {
    let elapsed = started_at.elapsed().as_secs_f64();
    if elapsed <= f64::EPSILON {
        0.0
    } else {
        bytes as f64 / elapsed
    }
}

pub(super) fn safe_join(root: &str, relative_path: &str) -> anyhow::Result<PathBuf> {
    let rel = Path::new(relative_path);
    if rel.is_absolute() {
        anyhow::bail!("refusing absolute transfer path: {relative_path}");
    }
    for component in rel.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("refusing unsafe transfer path: {relative_path}");
            }
        }
    }
    Ok(Path::new(root).join(rel))
}
