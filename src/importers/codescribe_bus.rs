//! Codescribe Transcript Bus as a source of operator speech.
//!
//! The operator talks (or types) to named agents through the Codescribe bus
//! (`~/.codescribe/agent-bridge/buses`). An agent reads each delivery with
//! `cs-bus --read-pending`, so inside the agent's own transcript the operator's
//! words exist only as a tool result — a lane `aicx intents` never reads, and
//! must not, because tool output is not speech. The bus ledger is the record of
//! what was said and to whom: this module replays it and recovers every
//! delivered message, keyed to the agent session that received it.
//!
//! Replay follows the bridge's own rule (`scripts/bus-demux.py`,
//! `EvidenceNormalizer`): `rendered_text` is an immutable full snapshot, so a
//! channel take is the latest snapshot of its session, delivered once the take
//! is closed — a non-open `channel-session` row, a newer open on the same
//! channel, or `session_ended`. A take still open where the ledger ends was not
//! delivered and is only counted. Text is never compared, merged or inferred
//! here. Chunked storage records (`codescribe.bus-chunk.v1`) are counted, not
//! reassembled.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use crate::extraction::{TimelineEntryMeta, build_timeline_entry};
use crate::sanitize;
use crate::timeline::{FrameKind, TimelineEntry};

use super::codescribe::CODESCRIBE_AGENT;

/// Bus root under the operator home; also an approved source root.
pub const BUS_ROOT_RELATIVE: &str = ".codescribe/agent-bridge/buses";

const EVIDENCE_SCHEMA: &str = "codescribe.transcript-evidence.v1";
const CHANNEL_SESSION_SCHEMA: &str = "codescribe.channel-session.v1";
const AGENT_USER_MESSAGE_SCHEMA: &str = "codescribe.agent-user-message.v1";
const AGENT_REPLY_SCHEMA: &str = "codescribe.agent-reply.v1";
const BUS_CHUNK_SCHEMA: &str = "codescribe.bus-chunk.v1";
const TERMINAL_SEAL: &str = "record_ledger_terminal_seal";
const SESSION_ENDED: &str = "session_ended";
const CHANNEL_SESSION_PREFIX: &str = "agent-channel-";
const TIMESTAMP_SOURCE: &str = "codescribe_bus";

/// Largest ledger replayed. Observed generations stay under 7 MiB; a bigger
/// file is skipped rather than read without bound.
const MAX_LEDGER_BYTES: u64 = 64 * 1024 * 1024;

/// One end of a bus delivery: an agent session as the bridge names it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BusParty {
    pub provider: String,
    pub session: String,
    pub name: Option<String>,
}

impl BusParty {
    /// Catalog agent label of the provider holding this session.
    pub fn catalog_agent(&self) -> Option<&'static str> {
        match self.provider.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-code" => Some("claude"),
            "codex" => Some("codex"),
            "gemini" | "gemini-cli" => Some("gemini"),
            "grok" => Some("grok"),
            "junie" => Some("junie"),
            "kimi" => Some("kimi"),
            "cursor" | "cursor-agent" => Some("cursor"),
            "copilot" | "copilot-cli" => Some("copilot"),
            _ => None,
        }
    }

    fn from_object(object: &Map<String, Value>) -> Option<Self> {
        let provider = str_field(object, "provider")?.trim();
        let session = str_field(object, "provider_session_id")?.trim();
        if provider.is_empty() || session.is_empty() {
            return None;
        }
        Some(Self {
            provider: provider.to_owned(),
            session: session.to_owned(),
            name: str_field(object, "name")
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
        })
    }

    fn same_session(&self, other: &Self) -> bool {
        self.provider == other.provider && self.session == other.session
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusMessageKind {
    /// A sealed voice take. `certified` is false when the acoustic ledger
    /// refused terminal coverage: the words were delivered, the take was not
    /// certified (`coverage: refused` on the bridge envelope).
    Spoken { certified: bool },
    /// Text the operator typed into the bus.
    Typed,
    /// An agent's own reply on the bus.
    AgentReply,
}

impl BusMessageKind {
    pub const fn is_human(self) -> bool {
        matches!(self, Self::Spoken { .. } | Self::Typed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusMessage {
    pub kind: BusMessageKind,
    pub at: DateTime<Utc>,
    pub text: String,
    /// Sessions the message was addressed to.
    pub recipients: Vec<BusParty>,
    /// Author of an agent reply.
    pub sender: Option<BusParty>,
}

/// Everything one ledger delivered, with the records it could not account
/// for counted instead of dropped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BusReplay {
    pub messages: Vec<BusMessage>,
    /// Lines that were not a JSON object.
    pub malformed_lines: usize,
    /// Chunked storage records, counted and not reassembled.
    pub chunk_records: usize,
    /// Delivered messages without a readable timestamp.
    pub undated_messages: usize,
    /// Channel takes still open where the ledger ends: not delivered.
    pub open_takes: usize,
}

impl BusReplay {
    /// Records the replay saw but could not turn into messages.
    pub fn skipped_records(&self) -> usize {
        self.malformed_lines + self.chunk_records + self.undated_messages
    }
}

struct TakeSnapshot {
    revision: i64,
    text: String,
    started_at: Option<DateTime<Utc>>,
    recipients: Vec<BusParty>,
    terminal: bool,
}

#[derive(Default)]
struct Replayer {
    replay: BusReplay,
    takes: HashMap<String, TakeSnapshot>,
    settled: HashSet<String>,
    open_by_channel: HashMap<String, (String, DateTime<Utc>)>,
}

impl Replayer {
    fn push(
        &mut self,
        kind: BusMessageKind,
        at: Option<DateTime<Utc>>,
        text: &str,
        recipients: Vec<BusParty>,
        sender: Option<BusParty>,
    ) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let Some(at) = at else {
            self.replay.undated_messages += 1;
            return;
        };
        self.replay.messages.push(BusMessage {
            kind,
            at,
            text: text.to_owned(),
            recipients,
            sender,
        });
    }

    fn close_take(&mut self, session: &str) {
        if !self.settled.insert(session.to_owned()) {
            return;
        }
        if let Some(take) = self.takes.remove(session) {
            self.push(
                BusMessageKind::Spoken {
                    certified: take.terminal,
                },
                take.started_at,
                &take.text,
                take.recipients,
                None,
            );
        }
    }

    fn evidence(&mut self, row: &Map<String, Value>) {
        let session = str_field(row, "session_id").unwrap_or_default().to_owned();
        let action = str_field(row, "reducer_action");
        if action == Some(SESSION_ENDED) {
            self.close_take(&session);
            return;
        }
        let Some(text) = str_field(row, "rendered_text") else {
            return;
        };
        let addressed = str_field(row, "audience").is_some_and(|audience| !audience.is_empty());
        if channel_of_session(&session).is_some() && addressed {
            if self.settled.contains(&session) {
                return;
            }
            let revision = row
                .get("reducer_revision")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let emitted = timestamp_field(row, "emitted_at");
            let recipients = parties(row.get("recipients"));
            let terminal = action == Some(TERMINAL_SEAL);
            let take = self.takes.entry(session).or_insert_with(|| TakeSnapshot {
                revision: i64::MIN,
                text: String::new(),
                started_at: None,
                recipients: Vec::new(),
                terminal: false,
            });
            take.started_at = match (take.started_at, emitted) {
                (Some(known), Some(seen)) => Some(known.min(seen)),
                (known, seen) => known.or(seen),
            };
            if revision >= take.revision {
                take.revision = revision;
                take.text = text.to_owned();
                take.terminal = terminal;
                if !recipients.is_empty() {
                    take.recipients = recipients;
                }
            }
        } else if action == Some(TERMINAL_SEAL) && self.settled.insert(session) {
            // A named (non-channel) utterance: its terminal row is the delivery.
            self.push(
                BusMessageKind::Spoken { certified: true },
                timestamp_field(row, "emitted_at"),
                text,
                parties(row.get("recipients")),
                None,
            );
        }
    }

    fn channel_session(&mut self, row: &Map<String, Value>) {
        let session = str_field(row, "session_id").unwrap_or_default();
        let channel = str_field(row, "channel").unwrap_or_default();
        if session.is_empty() || channel_of_session(session) != Some(channel) {
            return;
        }
        if str_field(row, "state") != Some("open") {
            let session = session.to_owned();
            self.close_take(&session);
            return;
        }
        let Some(opened) = timestamp_field(row, "opened_at") else {
            return;
        };
        let previous = self
            .open_by_channel
            .insert(channel.to_owned(), (session.to_owned(), opened));
        if let Some((previous, previous_opened)) = previous
            && previous != session
            && previous_opened < opened
        {
            self.close_take(&previous);
        }
    }

    fn finish(mut self) -> BusReplay {
        self.replay.open_takes = self.takes.len();
        self.replay.messages.sort_by_key(|message| message.at);
        self.replay
    }
}

/// Replay one bus ledger body (a generation file or a live channel file).
pub fn replay_bus_ledger(body: &str) -> BusReplay {
    let mut replayer = Replayer::default();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(Value::Object(row)) = serde_json::from_str::<Value>(line) else {
            replayer.replay.malformed_lines += 1;
            continue;
        };
        match str_field(&row, "schema").unwrap_or_default() {
            BUS_CHUNK_SCHEMA => replayer.replay.chunk_records += 1,
            EVIDENCE_SCHEMA => replayer.evidence(&row),
            CHANNEL_SESSION_SCHEMA => replayer.channel_session(&row),
            AGENT_USER_MESSAGE_SCHEMA => {
                let mut recipients = parties(row.get("recipients"));
                if recipients.is_empty() {
                    recipients.extend(BusParty::from_object(&row));
                }
                replayer.push(
                    BusMessageKind::Typed,
                    timestamp_field(&row, "emitted_at"),
                    str_field(&row, "text").unwrap_or_default(),
                    recipients,
                    None,
                );
            }
            AGENT_REPLY_SCHEMA => {
                let sender = row
                    .get("sender")
                    .and_then(Value::as_object)
                    .and_then(BusParty::from_object)
                    .or_else(|| BusParty::from_object(&row));
                replayer.push(
                    BusMessageKind::AgentReply,
                    timestamp_field(&row, "emitted_at"),
                    str_field(&row, "text").unwrap_or_default(),
                    parties(row.get("recipients")),
                    sender,
                );
            }
            _ => {}
        }
    }
    replayer.finish()
}

/// Bus root for one operator home.
pub fn bus_root(user_home: &Path) -> PathBuf {
    user_home.join(BUS_ROOT_RELATIVE)
}

/// Every bus ledger on disk: live channel files and archived generations.
pub fn discover_bus_ledgers(user_home: &Path) -> Vec<PathBuf> {
    let root = bus_root(user_home);
    let mut ledgers = Vec::new();
    collect_ledgers(&root, &mut ledgers);
    if let Ok(days) = std::fs::read_dir(root.join("events")) {
        for day in days.flatten() {
            let path = day.path();
            if path.is_dir() {
                collect_ledgers(&path, &mut ledgers);
            }
        }
    }
    ledgers.sort();
    ledgers
}

fn collect_ledgers(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && ledger_stem(&path).is_some() {
            out.push(path);
        }
    }
}

fn ledger_stem(ledger: &Path) -> Option<&str> {
    let name = ledger.file_name()?.to_str()?;
    let stem = name.strip_suffix(".jsonl")?;
    Some(stem.strip_suffix(".compressed").unwrap_or(stem))
}

/// Read and replay one ledger under the size cap.
pub fn read_bus_ledger(ledger: &Path) -> anyhow::Result<BusReplay> {
    let file = sanitize::open_file_validated(ledger)?;
    let mut bytes = Vec::new();
    file.take(MAX_LEDGER_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_LEDGER_BYTES {
        anyhow::bail!(
            "bus ledger {} exceeds {} bytes",
            ledger.display(),
            MAX_LEDGER_BYTES
        );
    }
    Ok(replay_bus_ledger(&String::from_utf8_lossy(&bytes)))
}

/// Catalog identity of the messages one ledger delivered to one session.
pub fn bus_session_id(ledger: &Path, recipient: &BusParty) -> Option<String> {
    let agent = recipient.catalog_agent()?;
    let stem = ledger_stem(ledger)?;
    Some(format!(
        "bus-{}-{agent}-{}",
        id_part(stem),
        id_part(&recipient.session)
    ))
}

fn id_part(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// One catalog session to admit: a ledger's human messages to one recipient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusSessionSeed {
    pub session_id: String,
    pub recipient: BusParty,
    /// Catalog agent of the recipient session (`claude`, `codex`, …).
    pub recipient_agent: &'static str,
    pub source_path: PathBuf,
    pub human_messages: usize,
    pub last_at: DateTime<Utc>,
}

/// Seed one catalog session per ledger and recipient that received human
/// speech. `modified_since_ns` skips ledgers untouched since a hot-window
/// cutoff; `None` replays every ledger (full rebuild).
pub fn bus_session_seeds(user_home: &Path, modified_since_ns: Option<u128>) -> Vec<BusSessionSeed> {
    let mut seeds = Vec::new();
    for ledger in discover_bus_ledgers(user_home) {
        if let Some(cutoff) = modified_since_ns {
            let modified = std::fs::metadata(&ledger)
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|age| age.as_nanos());
            if modified.is_some_and(|modified| modified < cutoff) {
                continue;
            }
        }
        let Ok(replay) = read_bus_ledger(&ledger) else {
            continue;
        };
        seeds.extend(seeds_for_ledger(&ledger, &replay));
    }
    seeds
}

fn seeds_for_ledger(ledger: &Path, replay: &BusReplay) -> Vec<BusSessionSeed> {
    let mut by_session: BTreeMap<String, BusSessionSeed> = BTreeMap::new();
    for message in replay.messages.iter().filter(|m| m.kind.is_human()) {
        for recipient in &message.recipients {
            let (Some(session_id), Some(recipient_agent)) =
                (bus_session_id(ledger, recipient), recipient.catalog_agent())
            else {
                continue;
            };
            let seed = by_session
                .entry(session_id.clone())
                .or_insert_with(|| BusSessionSeed {
                    session_id,
                    recipient: recipient.clone(),
                    recipient_agent,
                    source_path: ledger.to_path_buf(),
                    human_messages: 0,
                    last_at: message.at,
                });
            seed.human_messages += 1;
            seed.last_at = seed.last_at.max(message.at);
        }
    }
    by_session.into_values().collect()
}

/// Frames of one bus catalog session: the human messages its recipient
/// received, and that recipient's own replies, in time order.
///
/// `None` when no recipient in the ledger owns `session_id` — a stale
/// catalog row, never a reason to attribute someone else's messages.
pub fn bus_session_frames(
    ledger: &Path,
    replay: &BusReplay,
    session_id: &str,
    cwd: Option<&str>,
) -> Option<Vec<TimelineEntry>> {
    let recipient = replay
        .messages
        .iter()
        .flat_map(|message| &message.recipients)
        .find(|party| bus_session_id(ledger, party).as_deref() == Some(session_id))?
        .clone();
    let source_path = ledger.display().to_string();
    let frames = replay
        .messages
        .iter()
        .filter_map(|message| {
            let (role, frame_kind) = if message.kind.is_human() {
                if !message
                    .recipients
                    .iter()
                    .any(|party| party.same_session(&recipient))
                {
                    return None;
                }
                ("user", FrameKind::UserMsg)
            } else if message
                .sender
                .as_ref()
                .is_some_and(|sender| sender.same_session(&recipient))
            {
                ("assistant", FrameKind::AgentReply)
            } else {
                return None;
            };
            Some(build_timeline_entry(
                message.at,
                CODESCRIBE_AGENT,
                session_id,
                role,
                frame_text(message),
                TimelineEntryMeta {
                    cwd: cwd.map(str::to_owned),
                    frame_kind: Some(frame_kind),
                    timestamp_source: Some(TIMESTAMP_SOURCE.to_owned()),
                    source_path: Some(source_path.clone()),
                    ..TimelineEntryMeta::default()
                },
            ))
        })
        .collect();
    Some(frames)
}

/// Frame body in the shapes the intents classifier already reads.
///
/// Speech is wrapped in the `<codescribe>` envelope dictation carries into
/// agent prompts, so a bus take gets the same `voice_transcript` provenance,
/// `[voice]` marker and ASR garble gate. Typed text stays verbatim: the bus
/// records no boundary between what the operator wrote and what they pasted,
/// so only markup the operator typed (`>` quotes, fences) marks a reference.
fn frame_text(message: &BusMessage) -> String {
    match message.kind {
        BusMessageKind::Spoken { .. } => format!("<codescribe>{}</codescribe>", message.text),
        BusMessageKind::Typed | BusMessageKind::AgentReply => message.text.clone(),
    }
}

fn channel_of_session(session: &str) -> Option<&str> {
    let rest = session.strip_prefix(CHANNEL_SESSION_PREFIX)?;
    let (digits, _) = rest.split_once('-')?;
    (!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())).then_some(digits)
}

fn str_field<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object.get(key).and_then(Value::as_str)
}

fn timestamp_field(object: &Map<String, Value>, key: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(str_field(object, key)?.trim())
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

fn parties(value: Option<&Value>) -> Vec<BusParty> {
    let mut parties: Vec<BusParty> = value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .filter_map(BusParty::from_object)
        .collect();
    parties.sort();
    parties.dedup_by(|left, right| left.same_session(right));
    parties
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ALPHA: &str = "aaaaaaaa-0000-4000-8000-000000000001";
    const BETA: &str = "bbbbbbbb-0000-4000-8000-000000000002";
    const TAKE: &str = "agent-channel-0-cccccccc-0000-4000-8000-000000000003";
    const SPOKEN: &str = "Let's ship the synthetic voice lane today and keep the gates green";

    fn recipients() -> Value {
        json!([
            {"name": "alpha", "provider": "claude-code", "provider_session_id": ALPHA, "channel": "1"},
            {"name": "beta", "provider": "codex", "provider_session_id": BETA, "channel": "2"},
        ])
    }

    fn evidence(revision: i64, action: &str, text: &str, at: &str) -> Value {
        json!({
            "schema": EVIDENCE_SCHEMA,
            "session_id": TAKE,
            "audience": "*",
            "reducer_revision": revision,
            "reducer_action": action,
            "rendered_text": text,
            "emitted_at": at,
            "recipients": recipients(),
        })
    }

    fn channel_row(state: &str, at: &str) -> Value {
        json!({
            "schema": CHANNEL_SESSION_SCHEMA,
            "kind": "channel_session",
            "session_id": TAKE,
            "channel": "0",
            "state": state,
            "opened_at": "2026-01-02T10:00:00.000000Z",
            "emitted_at": at,
        })
    }

    fn ledger(rows: &[Value]) -> String {
        rows.iter().map(|row| format!("{row}\n")).collect()
    }

    /// Shape of a real broadcast take: growing snapshots, two coverage
    /// revisions of the full text, the close, then the receiver's reply.
    fn broadcast_take() -> String {
        ledger(&[
            channel_row("open", "2026-01-02T10:00:00.000000Z"),
            evidence(
                1,
                "apply_ledger_decision",
                "Let's ship the synthetic",
                "2026-01-02T10:00:05.000000Z",
            ),
            evidence(
                2,
                "apply_ledger_decision",
                "Let's ship the synthetic voice lane",
                "2026-01-02T10:00:09.000000Z",
            ),
            evidence(10, "seal_coverage", SPOKEN, "2026-01-02T10:00:20.000000Z"),
            evidence(11, "seal_coverage", SPOKEN, "2026-01-02T10:00:20.500000Z"),
            channel_row("sealed", "2026-01-02T10:00:23.000000Z"),
            json!({
                "schema": AGENT_REPLY_SCHEMA,
                "kind": "agent_reply",
                "name": "alpha",
                "provider": "claude-code",
                "provider_session_id": ALPHA,
                "text": "Shipping the synthetic lane now.",
                "emitted_at": "2026-01-02T10:00:40Z",
                "recipients": recipients(),
            }),
        ])
    }

    #[test]
    fn closed_take_delivers_its_latest_snapshot_once() {
        let replay = replay_bus_ledger(&broadcast_take());
        let spoken: Vec<_> = replay
            .messages
            .iter()
            .filter(|message| message.kind.is_human())
            .collect();
        assert_eq!(spoken.len(), 1);
        assert_eq!(spoken[0].text, SPOKEN);
        // Ledger refused terminal certification: words delivered, uncertified.
        assert_eq!(spoken[0].kind, BusMessageKind::Spoken { certified: false });
        // The take is dated when the operator started speaking.
        assert_eq!(spoken[0].at.to_rfc3339(), "2026-01-02T10:00:05+00:00");
        assert_eq!(spoken[0].recipients.len(), 2);
        assert_eq!(replay.open_takes, 0);
        assert_eq!(replay.skipped_records(), 0);
    }

    #[test]
    fn open_take_is_counted_not_delivered() {
        let body = ledger(&[
            channel_row("open", "2026-01-02T10:00:00Z"),
            evidence(
                1,
                "apply_ledger_decision",
                "still speaking",
                "2026-01-02T10:00:05Z",
            ),
        ]);
        let replay = replay_bus_ledger(&body);
        assert!(replay.messages.is_empty());
        assert_eq!(replay.open_takes, 1);
    }

    #[test]
    fn terminal_seal_certifies_the_take() {
        let body = ledger(&[
            evidence(4, TERMINAL_SEAL, "make it testable", "2026-01-02T11:00:00Z"),
            channel_row("sealed", "2026-01-02T11:00:02Z"),
        ]);
        let replay = replay_bus_ledger(&body);
        assert_eq!(
            replay.messages[0].kind,
            BusMessageKind::Spoken { certified: true }
        );
    }

    #[test]
    fn typed_messages_and_unreadable_records_are_accounted() {
        let body = format!(
            "{}not json\n{}\n",
            ledger(&[json!({
                "schema": AGENT_USER_MESSAGE_SCHEMA,
                "source": "typed",
                "name": "beta",
                "provider": "codex",
                "provider_session_id": BETA,
                "text": "run the install target",
                "emitted_at": "2026-01-02T12:00:00Z",
            })]),
            json!({"schema": BUS_CHUNK_SCHEMA, "part": 0, "parts": 2}),
        );
        let replay = replay_bus_ledger(&body);
        assert_eq!(replay.messages.len(), 1);
        assert_eq!(replay.messages[0].kind, BusMessageKind::Typed);
        assert_eq!(replay.messages[0].recipients[0].session, BETA);
        assert_eq!(replay.malformed_lines, 1);
        assert_eq!(replay.chunk_records, 1);
    }

    #[test]
    fn each_recipient_gets_its_own_session_with_only_its_messages() {
        let path = Path::new("/bus/events/2026_0102/dddddddd-0000.compressed.jsonl");
        let replay = replay_bus_ledger(&broadcast_take());
        let seeds = seeds_for_ledger(path, &replay);
        let ids: Vec<_> = seeds.iter().map(|seed| seed.session_id.clone()).collect();
        assert_eq!(
            ids,
            [
                format!("bus-dddddddd-0000-claude-{ALPHA}"),
                format!("bus-dddddddd-0000-codex-{BETA}"),
            ]
        );

        let alpha_session = format!("bus-dddddddd-0000-claude-{ALPHA}");
        let frames = bus_session_frames(path, &replay, &alpha_session, Some("/repo/demo")).unwrap();
        let shape: Vec<_> = frames
            .iter()
            .map(|frame| (frame.role.as_str(), frame.frame_kind))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("user", Some(FrameKind::UserMsg)),
                ("assistant", Some(FrameKind::AgentReply)),
            ]
        );
        assert!(frames.iter().all(|frame| frame.agent == CODESCRIBE_AGENT
            && frame.cwd.as_deref() == Some("/repo/demo")
            && frame.timestamp_source.as_deref() == Some(TIMESTAMP_SOURCE)));
        assert_eq!(
            frames[0].message,
            format!("<codescribe>{SPOKEN}</codescribe>")
        );
        assert_eq!(frames[1].message, "Shipping the synthetic lane now.");

        // Beta heard the take but did not send the reply.
        let beta_session = format!("bus-dddddddd-0000-codex-{BETA}");
        let frames = bus_session_frames(path, &replay, &beta_session, None).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].role, "user");

        assert!(bus_session_frames(path, &replay, "bus-unknown", None).is_none());
    }

    #[test]
    fn typed_text_stays_verbatim_and_only_speech_gets_the_voice_envelope() {
        let typed = BusMessage {
            kind: BusMessageKind::Typed,
            at: Utc::now(),
            text:
                "Please do these tasks:\n1. Fix terminal colors.\n2. Preserve my existing session."
                    .to_owned(),
            recipients: Vec::new(),
            sender: None,
        };
        assert_eq!(frame_text(&typed), typed.text);
        let spoken = BusMessage {
            kind: BusMessageKind::Spoken { certified: true },
            text: "make it testable".to_owned(),
            ..typed
        };
        assert_eq!(
            frame_text(&spoken),
            "<codescribe>make it testable</codescribe>"
        );
    }

    #[test]
    fn bus_root_is_an_approved_source_root() {
        assert!(crate::source_path::DEFAULT_SOURCE_ROOT_RELATIVE.contains(&BUS_ROOT_RELATIVE));
    }

    #[test]
    fn channel_session_ids_parse_only_the_channel_shape() {
        assert_eq!(channel_of_session(TAKE), Some("0"));
        assert_eq!(channel_of_session("agent-channel-12-x"), Some("12"));
        assert_eq!(channel_of_session("agent-channel--x"), None);
        assert_eq!(channel_of_session("dictation-1"), None);
    }
}
