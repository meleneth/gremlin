use std::path::PathBuf;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

pub const DEFAULT_SSH_PATH: &str = "~";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    LocalPath,
    FileUrl,
    Ssh,
    Url,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedTarget {
    pub original: String,
    pub kind: TargetKind,
    pub machine_hint: Option<String>,
    pub path: String,
    pub scheme: Option<String>,
}

impl ParsedTarget {
    pub fn display_machine_label(&self) -> String {
        self.machine_hint
            .clone()
            .unwrap_or_else(|| "local".to_string())
    }

    pub fn local_path(&self) -> Option<PathBuf> {
        match self.kind {
            TargetKind::LocalPath | TargetKind::FileUrl => Some(PathBuf::from(&self.path)),
            TargetKind::Ssh | TargetKind::Url => None,
        }
    }
}

pub fn parse_target(
    input: &str,
    explicit_kind: Option<TargetKind>,
) -> anyhow::Result<ParsedTarget> {
    let kind = match explicit_kind {
        Some(kind) => kind,
        None => infer_kind(input),
    };

    match kind {
        TargetKind::FileUrl => parse_file_url(input),
        TargetKind::Ssh => parse_ssh_target(input),
        TargetKind::Url => parse_url(input),
        TargetKind::LocalPath => Ok(ParsedTarget {
            original: input.to_string(),
            kind,
            machine_hint: None,
            path: input.to_string(),
            scheme: None,
        }),
    }
}

fn infer_kind(input: &str) -> TargetKind {
    if input.starts_with("file://") {
        return TargetKind::FileUrl;
    }
    if has_url_scheme(input) {
        return TargetKind::Url;
    }
    if looks_like_ssh_target(input) {
        return TargetKind::Ssh;
    }
    TargetKind::LocalPath
}

fn parse_file_url(input: &str) -> anyhow::Result<ParsedTarget> {
    let rest = input
        .strip_prefix("file://")
        .ok_or_else(|| anyhow::anyhow!("file URL must start with file://"))?;
    if rest.is_empty() {
        anyhow::bail!("file URL has no path");
    }
    let path = if rest.starts_with('/') {
        rest.to_string()
    } else if let Some((host, path)) = rest.split_once('/') {
        if host.is_empty() || host == "localhost" {
            format!("/{path}")
        } else {
            anyhow::bail!("non-local file URL host is not supported yet: {host}");
        }
    } else {
        anyhow::bail!("file URL has no absolute path");
    };
    Ok(ParsedTarget {
        original: input.to_string(),
        kind: TargetKind::FileUrl,
        machine_hint: None,
        path,
        scheme: Some("file".to_string()),
    })
}

fn parse_ssh_target(input: &str) -> anyhow::Result<ParsedTarget> {
    let Some((machine, path)) = input.split_once(':') else {
        anyhow::bail!("SSH target must look like host:/path or host:");
    };
    if machine.is_empty() {
        anyhow::bail!("SSH target must include a host");
    }
    let path = if path.is_empty() {
        DEFAULT_SSH_PATH
    } else {
        path
    };
    Ok(ParsedTarget {
        original: input.to_string(),
        kind: TargetKind::Ssh,
        machine_hint: Some(machine.to_string()),
        path: path.to_string(),
        scheme: Some("ssh".to_string()),
    })
}

fn parse_url(input: &str) -> anyhow::Result<ParsedTarget> {
    let Some((scheme, rest)) = input.split_once("://") else {
        anyhow::bail!("URL target must include a scheme");
    };
    if scheme.is_empty() || rest.is_empty() {
        anyhow::bail!("URL target must include a scheme and authority/path");
    }
    Ok(ParsedTarget {
        original: input.to_string(),
        kind: TargetKind::Url,
        machine_hint: None,
        path: rest.to_string(),
        scheme: Some(scheme.to_string()),
    })
}

fn has_url_scheme(input: &str) -> bool {
    let Some((scheme, _)) = input.split_once("://") else {
        return false;
    };
    !scheme.is_empty()
        && scheme
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
}

fn looks_like_ssh_target(input: &str) -> bool {
    if input.starts_with('/') || input.starts_with("./") || input.starts_with("../") {
        return false;
    }
    let Some((host, _path)) = input.split_once(':') else {
        return false;
    };
    !host.is_empty()
        && !host.contains('/')
        && host
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | '@'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_local_paths() {
        let target = parse_target("/tmp/archive", None).unwrap();
        assert_eq!(target.kind, TargetKind::LocalPath);
        assert_eq!(target.path, "/tmp/archive");
    }

    #[test]
    fn infers_file_urls() {
        let target = parse_target("file:///tmp/archive", None).unwrap();
        assert_eq!(target.kind, TargetKind::FileUrl);
        assert_eq!(target.path, "/tmp/archive");
    }

    #[test]
    fn infers_ssh_targets() {
        let target = parse_target("nas01:/mnt/archive", None).unwrap();
        assert_eq!(target.kind, TargetKind::Ssh);
        assert_eq!(target.machine_hint.as_deref(), Some("nas01"));
        assert_eq!(target.path, "/mnt/archive");
    }

    #[test]
    fn infers_ssh_default_location() {
        let target = parse_target("nas01:", None).unwrap();
        assert_eq!(target.kind, TargetKind::Ssh);
        assert_eq!(target.machine_hint.as_deref(), Some("nas01"));
        assert_eq!(target.path, DEFAULT_SSH_PATH);
    }

    #[test]
    fn explicit_kind_overrides_inference() {
        let target = parse_target("nas01:/mnt/archive", Some(TargetKind::LocalPath)).unwrap();
        assert_eq!(target.kind, TargetKind::LocalPath);
        assert_eq!(target.path, "nas01:/mnt/archive");
    }
}
