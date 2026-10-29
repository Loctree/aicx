//! Read-only materializer for native Antigravity CLI conversation databases.
//!
//! The parser crate never opens SQLite. This module opens each `.db` with
//! `mode=ro` (not `immutable`, so the WAL is visible), `query_only`, and a
//! short busy timeout. It writes nothing and never checkpoints the WAL.
//! The bytes it returns are the source the parser hashes.

use std::path::Path;
use std::time::Duration;

use aicx_parser::adapters::agy::{self, AgyHeaderPreview};
use rusqlite::{Connection, OpenFlags};

const MAX_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOTAL_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
const MAX_STEP_ROWS: usize = 100_000;

#[derive(Default)]
struct ReadBudget {
    bytes: usize,
    rows: usize,
}

impl ReadBudget {
    fn consume(&mut self, bytes: usize, is_step: bool) -> anyhow::Result<()> {
        anyhow::ensure!(
            bytes <= MAX_PAYLOAD_BYTES,
            "AGY payload exceeds the {MAX_PAYLOAD_BYTES}-byte limit"
        );
        anyhow::ensure!(
            bytes <= MAX_TOTAL_PAYLOAD_BYTES - self.bytes,
            "AGY conversation exceeds the {MAX_TOTAL_PAYLOAD_BYTES}-byte payload limit"
        );
        if is_step {
            anyhow::ensure!(
                self.rows < MAX_STEP_ROWS,
                "AGY conversation exceeds the {MAX_STEP_ROWS}-step limit"
            );
            self.rows += 1;
        }
        self.bytes += bytes;
        Ok(())
    }
}

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

/// Same read gate as `SourceArtifact::validated_file`: traversal rejection,
/// canonicalize (so a symlink cannot escape), allowlist, then a regular file.
pub(crate) fn validated_db_path(path: &Path) -> anyhow::Result<std::path::PathBuf> {
    let path = aicx_parser::sanitize::validate_read_path(path)
        .map_err(|error| anyhow::anyhow!("invalid source artifact: {error}"))?;
    if !path.is_file() {
        anyhow::bail!("invalid source artifact: selected artifact is not a regular file");
    }
    Ok(path)
}

pub fn materialize_agy_db(path: &Path) -> anyhow::Result<Vec<u8>> {
    let conn = open_ro(path)?;
    let mut budget = ReadBudget::default();
    let metadata = metadata_blob(&conn, &mut budget)?;
    let steps = load_steps(&conn, &mut budget)?;
    Ok(render_stream(metadata.as_deref(), &steps))
}

pub fn read_listing(path: &Path) -> anyhow::Result<AgyListing> {
    let conn = open_ro(path)?;
    let mut budget = ReadBudget::default();
    let metadata = metadata_blob(&conn, &mut budget)?;
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
    validate_step_totals(&conn, &budget, true)?;
    let mut statement = conn.prepare(
        "SELECT step_type, step_format, step_payload FROM steps WHERE step_type IN (14, 15) ORDER BY idx ASC",
    )?;
    let mut rows = statement.query([])?;
    let mut listing = AgyListing {
        cwd: header.cwd,
        project: header.project_name,
        title: None,
        started_at: header.started_at.clone(),
        updated_at: header.started_at,
        user_messages: 0,
        agent_messages: 0,
    };
    while let Some(row) = rows.next()? {
        let step_type = row.get::<_, i64>(0)?;
        let step_format = row.get::<_, Option<i64>>(1)?;
        let payload = row.get_ref(2)?.as_blob_or_null()?.unwrap_or(&[]);
        budget.consume(payload.len(), true)?;
        if step_format.is_some_and(|format| format != 0) {
            continue;
        }
        let preview = agy::preview_step(step_type, payload);
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

fn metadata_blob(conn: &Connection, budget: &mut ReadBudget) -> anyhow::Result<Option<Vec<u8>>> {
    let mut statement = conn.prepare(&format!(
        "SELECT CASE WHEN length(data) <= {MAX_PAYLOAD_BYTES} THEN data ELSE NULL END,
                COALESCE(length(data), 0)
         FROM trajectory_metadata_blob WHERE id = 'main' LIMIT 1"
    ))?;
    let mut rows = statement.query([])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    budget.consume(row.get(1)?, false)?;
    let payload = row.get_ref(0)?.as_blob_or_null()?;
    Ok(payload.map(<[u8]>::to_vec))
}

fn load_steps(conn: &Connection, budget: &mut ReadBudget) -> anyhow::Result<Vec<StepRow>> {
    validate_step_totals(conn, budget, false)?;
    let mut statement = conn.prepare(
        "SELECT idx, step_type, status, step_format, step_payload FROM steps ORDER BY idx ASC",
    )?;
    let mut rows = statement.query([])?;
    let mut steps = Vec::new();
    while let Some(row) = rows.next()? {
        let payload = row.get_ref(4)?.as_blob_or_null()?;
        budget.consume(payload.map_or(0, <[u8]>::len), true)?;
        steps.push(StepRow {
            idx: row.get(0)?,
            step_type: row.get(1)?,
            status: row.get(2)?,
            step_format: row.get(3)?,
            payload: payload.map(<[u8]>::to_vec),
        });
    }
    Ok(steps)
}

fn validate_step_totals(
    conn: &Connection,
    budget: &ReadBudget,
    speech_only: bool,
) -> anyhow::Result<()> {
    // Inspect blob lengths before SQLite can materialize or sort the payloads.
    let filter = if speech_only {
        " WHERE step_type IN (14, 15)"
    } else {
        ""
    };
    let (rows, bytes, largest): (usize, usize, usize) = conn.query_row(
        &format!(
            "SELECT COUNT(*), COALESCE(SUM(length(step_payload)), 0),
                    COALESCE(MAX(length(step_payload)), 0) FROM steps{filter}"
        ),
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    anyhow::ensure!(
        largest <= MAX_PAYLOAD_BYTES,
        "AGY payload exceeds the {MAX_PAYLOAD_BYTES}-byte limit"
    );
    anyhow::ensure!(
        bytes <= MAX_TOTAL_PAYLOAD_BYTES - budget.bytes,
        "AGY conversation exceeds the {MAX_TOTAL_PAYLOAD_BYTES}-byte payload limit"
    );
    anyhow::ensure!(
        rows <= MAX_STEP_ROWS - budget.rows,
        "AGY conversation exceeds the {MAX_STEP_ROWS}-step limit"
    );
    Ok(())
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
    let path = validated_db_path(path)?;
    let uri = sqlite_ro_uri(&path);
    let conn = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    conn.busy_timeout(Duration::from_millis(1500))?;
    conn.execute_batch("PRAGMA query_only = ON; BEGIN DEFERRED;")?;
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

    use super::*;

    fn steps_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE steps (
                idx INTEGER, step_type INTEGER, status INTEGER,
                step_format INTEGER, step_payload BLOB
             );
             INSERT INTO steps VALUES (2, 14, NULL, 0, X'01020304');
             INSERT INTO steps VALUES (0, 14, NULL, 0, X'01020304');
             INSERT INTO steps VALUES (1, 15, NULL, 0, X'01020304');",
        )
        .unwrap();
        conn
    }

    #[test]
    fn step_loading_preserves_order_at_the_exact_budget() {
        let conn = steps_db();
        let mut budget = ReadBudget {
            bytes: MAX_TOTAL_PAYLOAD_BYTES - 12,
            rows: MAX_STEP_ROWS - 3,
        };
        let steps = load_steps(&conn, &mut budget).unwrap();
        assert_eq!(
            steps.iter().map(|step| step.idx).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(budget.bytes, MAX_TOTAL_PAYLOAD_BYTES);
        assert_eq!(budget.rows, MAX_STEP_ROWS);
        assert!(
            String::from_utf8(render_stream(None, &steps))
                .unwrap()
                .contains("\"payload_b64\":\"AQIDBA==\"")
        );
    }

    #[test]
    fn step_loading_rejects_cumulative_payloads() {
        let mut budget = ReadBudget {
            bytes: MAX_TOTAL_PAYLOAD_BYTES - 8,
            rows: 0,
        };
        let error = load_steps(&steps_db(), &mut budget).err().unwrap();
        assert!(error.to_string().contains("payload limit"));
        assert_eq!(budget.rows, 0);
    }

    #[test]
    fn step_loading_rejects_excess_rows_even_with_small_payloads() {
        let mut budget = ReadBudget {
            bytes: 0,
            rows: MAX_STEP_ROWS - 2,
        };
        let error = load_steps(&steps_db(), &mut budget).err().unwrap();
        assert!(error.to_string().contains("step limit"));
        assert_eq!(budget.bytes, 0);
    }

    #[test]
    fn metadata_counts_towards_the_shared_payload_budget() {
        let conn = steps_db();
        conn.execute_batch(
            "CREATE TABLE trajectory_metadata_blob (id TEXT, data BLOB);
             INSERT INTO trajectory_metadata_blob VALUES ('main', X'01020304');",
        )
        .unwrap();
        let mut budget = ReadBudget {
            bytes: MAX_TOTAL_PAYLOAD_BYTES - 12,
            rows: 0,
        };
        assert_eq!(
            metadata_blob(&conn, &mut budget).unwrap(),
            Some(vec![1, 2, 3, 4])
        );
        assert!(load_steps(&conn, &mut budget).is_err());
        assert_eq!(budget.rows, 0);
    }

    #[test]
    fn oversized_sqlite_payload_is_rejected_before_copying() {
        let conn = steps_db();
        conn.execute(
            "UPDATE steps SET step_payload = zeroblob(?1) WHERE idx = 0",
            [MAX_PAYLOAD_BYTES + 1],
        )
        .unwrap();
        let mut budget = ReadBudget::default();
        let error = load_steps(&conn, &mut budget).err().unwrap();
        assert!(error.to_string().contains("AGY payload exceeds"));
        assert_eq!(budget.bytes, 0);
        assert_eq!(budget.rows, 0);
    }

    #[test]
    fn oversized_metadata_is_rejected_before_copying() {
        let conn = steps_db();
        conn.execute_batch("CREATE TABLE trajectory_metadata_blob (id TEXT, data BLOB);")
            .unwrap();
        conn.execute(
            "INSERT INTO trajectory_metadata_blob VALUES ('main', zeroblob(?1))",
            [MAX_PAYLOAD_BYTES + 1],
        )
        .unwrap();
        let mut budget = ReadBudget::default();
        let error = metadata_blob(&conn, &mut budget).unwrap_err();
        assert!(error.to_string().contains("AGY payload exceeds"));
        assert_eq!(budget.bytes, 0);
    }

    fn uri_for(path: &str) -> String {
        sqlite_ro_uri_from(&strip_verbatim_prefix(Path::new(path)).to_string_lossy())
    }

    #[test]
    fn materialize_rejects_a_database_outside_the_read_allowlist() {
        let outside = if cfg!(windows) {
            Path::new(r"C:\Windows\System32\drivers\etc\hosts")
        } else {
            Path::new("/etc/passwd")
        };
        let error = materialize_agy_db(outside).expect_err("outside path");
        let text = error.to_string();
        assert!(
            text.contains("outside allowed directories"),
            "listing and materialize share open_ro, got {text}"
        );
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
