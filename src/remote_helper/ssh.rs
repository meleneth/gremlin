use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

use anyhow::{Context, Result};

use super::framing::write_helper_frame;
use super::protocol::{HashRequest, HelperEvent};
use super::PROTOCOL_VERSION;

#[derive(Debug, thiserror::Error)]
pub enum SshHelperError {
    #[error("{0}")]
    Unavailable(String),
    #[error(transparent)]
    Session(#[from] anyhow::Error),
}

pub fn stream_hash_requests(
    host: &str,
    requests: &[HashRequest],
    mut on_event: impl FnMut(HelperEvent) -> Result<()>,
) -> std::result::Result<(), SshHelperError> {
    if requests.is_empty() {
        return Ok(());
    }
    ensure_python_memfd(host)?;
    let helper_path = helper_executable_path()?;
    let helper_bytes = std::fs::read(&helper_path).map_err(|err| {
        SshHelperError::Unavailable(format!(
            "remote helper executable unavailable at {}: {err}",
            helper_path.display()
        ))
    })?;
    let request_lines = requests
        .iter()
        .map(serde_json::to_string)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|err| SshHelperError::Session(err.into()))?
        .join("\n")
        + "\n";

    run_helper_session(
        host,
        helper_bytes,
        request_lines.into_bytes(),
        &mut on_event,
    )?;
    Ok(())
}

fn ensure_python_memfd(host: &str) -> std::result::Result<(), SshHelperError> {
    let output = Command::new("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=2")
        .arg(host)
        .arg("python3 -c 'import os,sys; sys.exit(0 if hasattr(os,\"memfd_create\") else 1)'")
        .output()
        .map_err(|err| {
            SshHelperError::Unavailable(format!("checking remote helper capability failed: {err}"))
        })?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(SshHelperError::Unavailable(format!(
        "remote helper unavailable on {host}: python3 memfd_create check failed: {}",
        stderr.trim()
    )))
}

fn helper_executable_path() -> std::result::Result<PathBuf, SshHelperError> {
    if let Some(path) = std::env::var_os("GREMLIN_REMOTE_HELPER") {
        return Ok(PathBuf::from(path));
    }
    let current = std::env::current_exe().map_err(|err| {
        SshHelperError::Unavailable(format!("cannot locate current executable: {err}"))
    })?;
    let sibling = current
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(helper_binary_name());
    if sibling.is_file() {
        return Ok(sibling);
    }
    Err(SshHelperError::Unavailable(format!(
        "remote helper executable not found next to {} (set GREMLIN_REMOTE_HELPER or install gremlin-remote-helper)",
        current.display()
    )))
}

fn helper_binary_name() -> &'static str {
    if cfg!(windows) {
        "gremlin-remote-helper.exe"
    } else {
        "gremlin-remote-helper"
    }
}

fn run_helper_session(
    host: &str,
    helper_bytes: Vec<u8>,
    request_bytes: Vec<u8>,
    on_event: &mut impl FnMut(HelperEvent) -> Result<()>,
) -> Result<()> {
    let mut child = Command::new("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=2")
        .arg(host)
        .arg(memfd_bootstrap_command())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting remote helper session on {host}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture ssh stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture ssh stdout"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture ssh stderr"))?;
    let stderr_reader = thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let writer = thread::spawn(move || -> Result<()> {
        write_helper_frame(&mut stdin, &helper_bytes)?;
        stdin.write_all(&request_bytes)?;
        stdin.flush()?;
        drop(stdin);
        Ok(())
    });

    let mut saw_hello = false;
    for line in BufReader::new(stdout).lines() {
        let line = line.with_context(|| format!("reading remote helper output from {host}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let event: HelperEvent = serde_json::from_str(&line)
            .with_context(|| format!("invalid JSONL emitted by remote helper on {host}"))?;
        match &event {
            HelperEvent::Hello { version, .. } => {
                if *version != PROTOCOL_VERSION {
                    anyhow::bail!(
                        "remote helper protocol mismatch: local {}, remote {}",
                        PROTOCOL_VERSION,
                        version
                    );
                }
                saw_hello = true;
            }
            _ if !saw_hello => {
                anyhow::bail!("remote helper emitted data before protocol hello");
            }
            _ => on_event(event)?,
        }
    }
    writer
        .join()
        .map_err(|_| anyhow::anyhow!("remote helper writer thread panicked"))??;
    let status = child
        .wait()
        .with_context(|| format!("waiting for remote helper session on {host}"))?;
    let stderr = stderr_reader.join().unwrap_or_default();
    if !status.success() {
        anyhow::bail!("remote helper session failed on {host}: {}", stderr.trim());
    }
    if !saw_hello {
        anyhow::bail!("remote helper exited before protocol hello");
    }
    Ok(())
}

fn memfd_bootstrap_command() -> &'static str {
    "python3 -c 'import os,struct,sys\n\
def read_exact(n):\n\
 data=bytearray()\n\
 while len(data)<n:\n\
  chunk=sys.stdin.buffer.read(n-len(data))\n\
  if not chunk:\n\
   raise SystemExit(\"truncated gremlin helper frame\")\n\
  data.extend(chunk)\n\
 return bytes(data)\n\
hdr=read_exact(8)\n\
length=struct.unpack(\">Q\",hdr)[0]\n\
fd=os.memfd_create(\"gremlin-remote-helper\",0)\n\
remaining=length\n\
while remaining:\n\
 chunk=sys.stdin.buffer.read(min(1048576,remaining))\n\
 if not chunk:\n\
  raise SystemExit(\"truncated gremlin helper executable\")\n\
 os.write(fd,chunk)\n\
 remaining-=len(chunk)\n\
os.fchmod(fd,0o700)\n\
os.execve(f\"/proc/self/fd/{fd}\",[\"gremlin-remote-helper\"],os.environ)'"
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bootstrap_documents_big_endian_frame_unpacking() {
        let command = memfd_bootstrap_command();

        assert!(command.contains("struct.unpack(\">Q\",hdr)"));
        assert!(command.contains("read_exact(8)"));
        assert!(command.contains("os.memfd_create"));
        assert!(command.contains("/proc/self/fd/{fd}"));
    }

    #[test]
    fn helper_requests_serialize_paths_inside_json() {
        let request = HashRequest {
            id: json!(42),
            op: "hash".to_string(),
            path: "/tmp/a,b \"quoted\"\nfile".to_string(),
            hashes: vec!["crc32".to_string(), "sha256".to_string()],
            chunk_size: Some(64 * 1024 * 1024),
        };

        let line = serde_json::to_string(&request).unwrap();

        assert!(line.contains("\"path\":\"/tmp/a,b \\\"quoted\\\"\\nfile\""));
        assert!(!line.contains("sha256sum"));
    }
}
