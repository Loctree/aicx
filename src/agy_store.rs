//! Read-only materializer for native Antigravity CLI conversation databases.
//!
//! The parser crate never opens SQLite. This module opens each `.db` with
//! `mode=ro` (not `immutable`, so the WAL is visible), `query_only`, and a
//! short busy timeout. It writes nothing and never checkpoints the WAL.
//! The bytes it returns are the source the parser hashes.

use std::path::Path;
use std::time::Duration;

use aicx_parser::adapters::agy::{self, AgyHeaderPreview};
use rusqlite::{Connection, OpenFlags, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgyListing {
    pub cwd: Option<String>,
    pub project: Option<String>,
    pub title: Option<String>,
    pub started_at: Option<String>,
    pub updated_at: Option<String>,
    pub user_messages: usize,
    pub agent_messages: usize,
}

struct StepRow {
    idx: i64,
    step_type: i64,
    status: Option<i64>,
    step_format: Option<i64>,
    payload: Option<Vec<u8>>,
}

pub fn materialize_agy_db(path: &Path) -> anyhow::Result<Vec<u8>> {
    let conn = open_ro(path)?;
    let metadata = metadata_blob(&conn)?;
    let mut steps = load_steps(&conn)?;
    steps.sort_by_key(|step| step.idx);
    Ok(render_stream(metadata.as_deref(), &steps))
}

pub fn read_listing(path: &Path) -> anyhow::Result<AgyListing> {
    let conn = open_ro(path)?;
    let metadata = metadata_blob(&conn)?;
    let header = metadata
        .as_deref()
        .map(agy::preview_header)
        .unwrap_or(AgyHeaderPreview {
            cascade_id: None,
            trajectory_id: None,
            cwd: None,
            project_name: None,
            started_at: None,
        });
    let mut statement = conn.prepare(
        "SELECT step_type, step_format, step_payload FROM steps WHERE step_type IN (14, 15) ORDER BY idx ASC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Option<i64>>(1)?,
            row.get::<_, Option<Vec<u8>>>(2)?,
        ))
    })?;
    let mut listing = AgyListing {
        cwd: header.cwd,
        project: header.project_name,
        title: None,
        started_at: header.started_at.clone(),
        updated_at: header.started_at,
        user_messages: 0,
        agent_messages: 0,
    };
    for row in rows {
        let (step_type, step_format, payload) = row?;
        if step_format.is_some_and(|format| format != 0) {
            continue;
        }
        let preview = agy::preview_step(step_type, payload.as_deref().unwrap_or(&[]));
        if let Some(text) = preview.human_text {
            listing.user_messages += 1;
            if listing.title.is_none() {
                listing.title = Some(text);
            }
        }
        if preview.assistant_text.is_some() {
            listing.agent_messages += 1;
        }
        if let Some(stamp) = preview.timestamp {
            if listing.started_at.is_none() {
                listing.started_at = Some(stamp.clone());
            }
            listing.updated_at = Some(stamp);
        }
    }
    Ok(listing)
}

fn metadata_blob(conn: &Connection) -> anyhow::Result<Option<Vec<u8>>> {
    Ok(conn
        .query_row(
            "SELECT data FROM trajectory_metadata_blob WHERE id = 'main' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?)
}

fn load_steps(conn: &Connection) -> anyhow::Result<Vec<StepRow>> {
    let mut statement = conn.prepare(
        "SELECT idx, step_type, status, step_format, step_payload FROM steps ORDER BY idx ASC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(StepRow {
            idx: row.get(0)?,
            step_type: row.get(1)?,
            status: row.get(2)?,
            step_format: row.get(3)?,
            payload: row.get(4)?,
        })
    })?;
    let mut steps = Vec::new();
    for row in rows {
        steps.push(row?);
    }
    Ok(steps)
}

fn render_stream(metadata: Option<&[u8]>, steps: &[StepRow]) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(bytes) = metadata {
        let line = format!(
            "{{\"metadata_b64\":\"{}\",\"record\":\"header\",\"v\":1}}\n",
            agy::encode_b64(bytes)
        );
        out.extend_from_slice(line.as_bytes());
    }
    for step in steps {
        let payload = step.payload.as_deref().unwrap_or(&[]);
        let line = format!(
            "{{\"idx\":{},\"payload_b64\":\"{}\",\"record\":\"step\",\"status\":{},\"step_format\":{},\"step_type\":{},\"v\":1}}\n",
            step.idx,
            agy::encode_b64(payload),
            json_i64(step.status),
            json_i64(step.step_format),
            step.step_type
        );
        out.extend_from_slice(line.as_bytes());
    }
    out
}

fn json_i64(value: Option<i64>) -> String {
    value
        .map(|number| number.to_string())
        .unwrap_or_else(|| "null".to_owned())
}

fn open_ro(path: &Path) -> anyhow::Result<Connection> {
    let uri = sqlite_ro_uri(path);
    let conn = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    conn.busy_timeout(Duration::from_millis(1500))?;
    conn.execute_batch("PRAGMA query_only = ON;")?;
    Ok(conn)
}

fn sqlite_ro_uri(path: &Path) -> String {
    let absolute = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    // canonicalize() is verbatim on Windows (`\\?\C:\…`); SQLite reads that `?`
    // as a URI authority and refuses the open.
    let absolute = aicx_parser::sanitize::strip_verbatim_prefix(&absolute);
    sqlite_ro_uri_from(&absolute.to_string_lossy())
}

fn sqlite_ro_uri_from(path: &str) -> String {
    let text = path.replace('\\', "/");
    let mut encoded = String::from("file:");
    // A drive path (`C:/…`) or a UNC share (`//server/…`) needs an empty
    // authority in front of it, as SQLite documents for Windows URIs.
    if text.as_bytes().get(1) == Some(&b':') || text.starts_with("//") {
        encoded.push_str("//");
        if !text.starts_with('/') {
            encoded.push('/');
        }
    }
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'.' | b'_' | b'-' | b':' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded.push_str("?mode=ro");
    encoded
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use aicx_parser::sanitize::strip_verbatim_prefix;

    use super::sqlite_ro_uri_from;

    fn uri_for(path: &str) -> String {
        sqlite_ro_uri_from(&strip_verbatim_prefix(Path::new(path)).to_string_lossy())
    }

    #[test]
    fn posix_paths_keep_the_plain_file_uri() {
        assert_eq!(
            uri_for("/Users/x/.gemini/antigravity-cli/conversations/a b.db"),
            "file:/Users/x/.gemini/antigravity-cli/conversations/a%20b.db?mode=ro"
        );
    }

    #[test]
    fn windows_verbatim_drive_paths_open_without_a_question_mark_authority() {
        assert_eq!(
            uri_for(r"\\?\C:\Users\runner\.gemini\antigravity\conversations\1.db"),
            "file:///C:/Users/runner/.gemini/antigravity/conversations/1.db?mode=ro"
        );
    }

    #[test]
    fn windows_verbatim_unc_paths_keep_an_empty_authority() {
        assert_eq!(
            uri_for(r"\\?\UNC\server\share\conversations\1.db"),
            "file:////server/share/conversations/1.db?mode=ro"
        );
    }
}
