use std::io::{BufRead, Write};

use super::engine::hash_request;
use super::protocol::{capabilities, HashRequest, HelperEvent};
use super::PROTOCOL_VERSION;

pub const MAX_REQUEST_LINE_BYTES: usize = 1024 * 1024;

pub fn run_session<R: BufRead, W: Write>(mut reader: R, mut writer: W) -> anyhow::Result<()> {
    write_event(
        &mut writer,
        &HelperEvent::Hello {
            version: PROTOCOL_VERSION,
            capabilities: capabilities(),
        },
    )?;
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            break;
        }
        if read > MAX_REQUEST_LINE_BYTES {
            write_event(
                &mut writer,
                &HelperEvent::Error {
                    id: serde_json::Value::Null,
                    path: None,
                    code: "invalid_request".to_string(),
                    message: "request line exceeds maximum size".to_string(),
                },
            )?;
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        let request: HashRequest = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(err) => {
                write_event(
                    &mut writer,
                    &HelperEvent::Error {
                        id: serde_json::Value::Null,
                        path: None,
                        code: "invalid_request".to_string(),
                        message: err.to_string(),
                    },
                )?;
                continue;
            }
        };
        hash_request(request, |event| write_event(&mut writer, &event))?;
    }
    writer.flush()?;
    Ok(())
}

fn write_event(writer: &mut impl Write, event: &HelperEvent) -> anyhow::Result<()> {
    serde_json::to_writer(&mut *writer, event)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::fs;
    use std::io::Cursor;

    fn run(input: String) -> Vec<Value> {
        let mut output = Vec::new();
        run_session(Cursor::new(input), &mut output).unwrap();
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn emits_version_handshake_before_results() {
        let events = run(String::new());

        assert_eq!(events[0]["type"], "hello");
        assert_eq!(events[0]["version"], PROTOCOL_VERSION);
        assert!(events[0]["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("crc32")));
    }

    #[test]
    fn malformed_json_emits_error_and_later_request_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ok\nfile, \"quoted\".bin");
        fs::write(&path, b"ok").unwrap();
        let good = json!({
            "id": 9,
            "op": "hash",
            "path": path,
            "hashes": ["crc32"]
        });
        let events = run(format!("{{not json}}\n{good}\n"));

        assert!(events
            .iter()
            .any(|event| event["type"] == "error" && event["code"] == "invalid_request"));
        assert!(events
            .iter()
            .any(|event| event["type"] == "result" && event["id"] == 9));
    }

    #[test]
    fn one_failed_request_does_not_abort_following_successful_request() {
        let dir = tempfile::tempdir().unwrap();
        let good_path = dir.path().join("good");
        fs::write(&good_path, b"ok").unwrap();
        let missing_path = dir.path().join("missing");
        let bad = json!({
            "id": 1,
            "op": "hash",
            "path": missing_path,
            "hashes": ["sha256"]
        });
        let good = json!({
            "id": 2,
            "op": "hash",
            "path": good_path,
            "hashes": ["sha256", "crc32"]
        });

        let events = run(format!("{bad}\n{good}\n"));

        assert!(events.iter().any(|event| event["type"] == "error"
            && event["id"] == 1
            && event["code"] == "not_found"));
        assert!(events.iter().any(|event| {
            event["type"] == "result"
                && event["id"] == 2
                && event.get("sha256").is_some()
                && event.get("crc32").is_some()
        }));
    }
}
