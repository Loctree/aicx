#![allow(unused_imports)]
use crate::extraction::*;
use anyhow::Result;
use chrono::{Duration, NaiveDate, NaiveTime, TimeZone};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::io::BufReader;

use crate::importers::operator_markdown::split_operator_frontmatter;
use crate::timeline::FrameKind;

pub(crate) const CODESCRIBE_AGENT: &str = "codescribe";
const CODESCRIBE_TRANSCRIPT_KIND: &str = "transcript";
const CODESCRIBE_NO_SPEECH_MARKERS: &[&str] = &[
    "no reliable speech detected",
    "no speech detected",
    "vad_no_speech_detected",
];

/// A discovered Codescribe transcript under `$HOME/.codescribe/transcriptions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodescribeTranscript {
    pub path: PathBuf,
    pub date: NaiveDate,
}

#[derive(Debug, Clone)]
struct CodescribeSegment {
    start_ms: u64,
    duration_ms: Option<u64>,
    speaker: Option<String>,
    text: String,
}

#[derive(Debug, Clone, Default)]
struct CodescribeLexicon {
    entries: Vec<CodescribeLexiconEntry>,
}

#[derive(Debug, Clone)]
struct CodescribeLexiconEntry {
    speaker: Option<String>,
    keywords: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawCodescribeLexiconEntry {
    #[serde(default)]
    speaker: Option<String>,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    term: Option<String>,
    #[serde(default)]
    mispronunciations: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct WhisperTranscript {
    #[serde(default)]
    segments: Vec<WhisperSegment>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WhisperSegment {
    #[serde(default)]
    start: Option<f64>,
    #[serde(default)]
    end: Option<f64>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    speaker: Option<String>,
}

pub fn discover_codescribe_transcripts(home: &Path) -> Vec<CodescribeTranscript> {
    discover_codescribe_transcripts_at(&home.join(".codescribe").join("transcriptions"))
}

/// Discover Codescribe transcript files under an explicit transcriptions root.
pub fn discover_codescribe_transcripts_at(root: &Path) -> Vec<CodescribeTranscript> {
    if !root.is_dir() {
        return Vec::new();
    }

    let mut entries = Vec::new();
    let Ok(day_dirs) = fs::read_dir(root) else {
        return entries;
    };

    for day_dir in day_dirs.flatten() {
        let day_path = day_dir.path();
        if !day_path.is_dir() {
            continue;
        }
        let Some(date) = day_path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(parse_codescribe_day)
        else {
            continue;
        };
        let Ok(files) = fs::read_dir(&day_path) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if is_codescribe_transcript_file(&path) {
                entries.push(CodescribeTranscript { path, date });
            }
        }
    }

    entries.sort_by(|left, right| left.path.cmp(&right.path));
    entries
}

fn parse_codescribe_day(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()
}

fn is_codescribe_transcript_file(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return false;
    };
    if !matches!(ext, "txt" | "md" | "json") {
        return false;
    }

    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    !name.ends_with(".truth.json")
}

fn codescribe_base_time(path: &Path, date: NaiveDate) -> DateTime<Utc> {
    let time = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.get(0..6))
        .and_then(|prefix| NaiveTime::parse_from_str(prefix, "%H%M%S").ok())
        .unwrap_or(NaiveTime::MIN);
    Utc.from_utc_datetime(&date.and_time(time))
}

fn codescribe_timestamp(path: &Path, date: NaiveDate, start_ms: u64) -> DateTime<Utc> {
    codescribe_base_time(path, date) + Duration::milliseconds(start_ms.min(i64::MAX as u64) as i64)
}

fn load_codescribe_lexicon(home: &Path) -> CodescribeLexicon {
    let path = home.join(".codescribe").join("lexicon.custom.jsonl");
    let Ok(file) = sanitize::open_file_validated(&path) else {
        return CodescribeLexicon::default();
    };

    let mut entries = Vec::new();
    let mut reader = BufReader::new(file);
    while let Ok(Some(line)) = sanitize::read_line_capped(&mut reader, MAX_LINE_BYTES) {
        if line.exceeded {
            continue;
        }
        let line = line.line;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(raw) = serde_json::from_str::<RawCodescribeLexiconEntry>(&line) else {
            continue;
        };
        let mut keywords = raw.keywords;
        if let Some(term) = raw.term {
            keywords.push(term);
        }
        keywords.extend(raw.mispronunciations);
        keywords.retain(|keyword| !keyword.trim().is_empty());
        if !keywords.is_empty() {
            entries.push(CodescribeLexiconEntry {
                speaker: raw.speaker,
                keywords,
            });
        }
    }

    CodescribeLexicon { entries }
}

impl CodescribeLexicon {
    fn speaker_hint(&self, explicit: Option<&str>, text: &str) -> String {
        if let Some(speaker) = explicit.and_then(normalize_speaker_hint) {
            return speaker;
        }

        let text = text.to_lowercase();
        let mut scores: HashMap<String, usize> = HashMap::new();
        for entry in &self.entries {
            let Some(speaker) = entry.speaker.as_deref().and_then(normalize_speaker_hint) else {
                continue;
            };
            for keyword in &entry.keywords {
                if text.contains(&keyword.to_lowercase()) {
                    *scores.entry(speaker.clone()).or_default() += 1;
                }
            }
        }

        scores
            .into_iter()
            .max_by_key(|(_, score)| *score)
            .map(|(speaker, _)| speaker)
            .unwrap_or_else(|| "unknown".to_string())
    }
}

fn normalize_speaker_hint(value: &str) -> Option<String> {
    let normalized = value
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect::<String>();
    (!normalized.is_empty()).then_some(normalized)
}

fn parse_plain_codescribe_text(content: &str) -> Vec<CodescribeSegment> {
    let text = content.trim();
    if text.is_empty() || is_codescribe_no_speech(text) {
        return Vec::new();
    }

    vec![CodescribeSegment {
        start_ms: 0,
        duration_ms: None,
        speaker: None,
        text: text.to_string(),
    }]
}

fn parse_codescribe_markdown(content: &str) -> Vec<CodescribeSegment> {
    let mut segments = Vec::new();
    let mut current_speaker: Option<String> = None;
    let mut current = String::new();

    for line in content.lines() {
        if let Some(speaker) = parse_markdown_speaker_heading(line) {
            push_codescribe_markdown_segment(&mut segments, current_speaker.take(), &mut current);
            current_speaker = Some(speaker);
            continue;
        }

        current.push_str(line);
        current.push('\n');
    }

    push_codescribe_markdown_segment(&mut segments, current_speaker, &mut current);
    if segments.is_empty() {
        return parse_plain_codescribe_text(content);
    }
    segments
}

fn parse_markdown_speaker_heading(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let heading = trimmed.strip_prefix('#')?.trim_start_matches('#').trim();
    let lower = heading.to_lowercase();
    // Speaker turns use the `### <Name>:` convention; accept the explicit
    // "speaker" prefix or any colon-terminated heading as a speaker label.
    if !(lower.starts_with("speaker") || heading.ends_with(':')) {
        return None;
    }
    Some(
        heading
            .trim_end_matches(':')
            .split(':')
            .next()
            .unwrap_or(heading)
            .trim()
            .to_string(),
    )
}

fn push_codescribe_markdown_segment(
    segments: &mut Vec<CodescribeSegment>,
    speaker: Option<String>,
    current: &mut String,
) {
    let text = current.trim();
    if !text.is_empty() && !is_codescribe_no_speech(text) {
        segments.push(CodescribeSegment {
            start_ms: 0,
            duration_ms: None,
            speaker,
            text: text.to_string(),
        });
    }
    current.clear();
}

fn parse_codescribe_json(content: &str) -> Result<Vec<CodescribeSegment>> {
    let transcript: WhisperTranscript = serde_json::from_str(content)?;
    let mut segments = Vec::new();

    for segment in transcript.segments {
        let text = segment.text.unwrap_or_default().trim().to_string();
        if text.is_empty() || is_codescribe_no_speech(&text) {
            continue;
        }
        let start_ms = seconds_to_ms(segment.start.unwrap_or_default());
        let duration_ms = match (segment.start, segment.end) {
            (Some(start), Some(end)) if end > start => Some(seconds_to_ms(end - start)),
            _ => None,
        };
        segments.push(CodescribeSegment {
            start_ms,
            duration_ms,
            speaker: segment.speaker,
            text,
        });
    }

    if segments.is_empty()
        && let Some(text) = transcript.text
    {
        segments = parse_plain_codescribe_text(&text);
    }

    Ok(segments)
}

fn seconds_to_ms(seconds: f64) -> u64 {
    if seconds.is_finite() && seconds > 0.0 {
        (seconds * 1000.0).round() as u64
    } else {
        0
    }
}

fn is_codescribe_no_speech(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    CODESCRIBE_NO_SPEECH_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

/// Parse one Codescribe transcript file into timeline entries.
pub fn parse_codescribe_transcript(
    path: &Path,
    date: NaiveDate,
    config: &ExtractionConfig,
) -> Result<Vec<TimelineEntry>> {
    let home = crate::os_user_home().context("No home dir")?;
    let lexicon = load_codescribe_lexicon(&home);
    parse_codescribe_transcript_with_lexicon(path, date, config, &lexicon, &home)
}

fn parse_codescribe_transcript_with_lexicon(
    path: &Path,
    date: NaiveDate,
    config: &ExtractionConfig,
    lexicon: &CodescribeLexicon,
    home: &Path,
) -> Result<Vec<TimelineEntry>> {
    let content = sanitize::read_to_string_validated(path)?;
    let (source_path, source_sha256) = source_path_and_sha256(path);
    let source_line_span = Some((1, content.lines().count().max(1) as u64));

    let mut project_hint = None;
    let (frontmatter, body) = split_operator_frontmatter(&content);
    if let Some(project) = frontmatter
        .project
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        project_hint = Some(project.trim().to_string());
    } else {
        let lower_body = body.to_ascii_lowercase();
        for filter in &config.project_filter {
            let repo = filter
                .split_once('/')
                .map(|(_, r)| r)
                .unwrap_or(filter)
                .to_ascii_lowercase();
            // Use token-equality (not raw substring) so `-p vista` does
            // not falsely attribute a transcript that mentions
            // `vista-portal`. This addresses the gemini-code-assist
            // MEDIUM comment on PR #8: substring `lower_body.contains`
            // re-introduced the suffix-leak shape that the strict
            // identity matchers in this PR were specifically removing.
            if body_mentions_repo_token(&lower_body, &repo) {
                project_hint = Some(filter.clone());
                break;
            }
        }
    }

    if project_hint.is_none() {
        // TODO: codescribe transcript has no embedded identity in frontmatter or content.
        // Keeping the global filter as a fallback for that path-only case.
        if config.project_filter.len() == 1 {
            project_hint = config.project_filter.first().cloned();
        }
    }

    let cwd_hint_owned = resolve_codescribe_cwd_hint(home, project_hint.as_deref());
    let cwd_hint = cwd_hint_owned.as_deref();
    let segments = match path.extension().and_then(|ext| ext.to_str()) {
        Some("json") => parse_codescribe_json(&content)?,
        Some("md") => parse_codescribe_markdown(&content),
        _ => parse_plain_codescribe_text(&content),
    };

    let session_id = format!(
        "{}-{}-codescribe-{}",
        codescribe_path_fingerprint(path),
        path.file_stem()
            .map(|stem| stem.to_string_lossy())
            .unwrap_or_else(|| "unknown".into()),
        date.format("%Y-%m-%d")
    );
    let source_file = path.display();

    let mut entries = Vec::new();
    for segment in segments {
        let timestamp = codescribe_timestamp(path, date, segment.start_ms);
        if timestamp < config.cutoff || config.watermark.is_some_and(|w| timestamp < w) {
            continue;
        }

        let speaker_hint = lexicon.speaker_hint(segment.speaker.as_deref(), &segment.text);
        let duration = segment
            .duration_ms
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let message = format!(
            "kind: {CODESCRIBE_TRANSCRIPT_KIND}\nspeaker_hint: {speaker_hint}\nsource_file: {source_file}\naudio_offset_ms: {}\nduration_ms: {duration}\n\n{}",
            segment.start_ms, segment.text
        );

        entries.push(build_timeline_entry(
            timestamp,
            CODESCRIBE_AGENT,
            &session_id,
            "user",
            message,
            TimelineEntryMeta {
                cwd: cwd_hint.map(ToOwned::to_owned),
                frame_kind: Some(FrameKind::UserMsg),
                source_path: Some(source_path.clone()),
                source_sha256: source_sha256.clone(),
                source_line_span,
                ..TimelineEntryMeta::default()
            },
        ));
    }

    Ok(entries)
}

fn codescribe_path_fingerprint(path: &Path) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

/// Extract Codescribe transcript entries from `$HOME/.codescribe/transcriptions`.
pub fn extract_codescribe(config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let home = crate::os_user_home().context("No home dir")?;
    extract_codescribe_from_home(&home, config)
}

/// Extract Codescribe transcript entries using an explicit home directory.
pub fn extract_codescribe_from_home(
    home: &Path,
    config: &ExtractionConfig,
) -> Result<Vec<TimelineEntry>> {
    let lexicon = load_codescribe_lexicon(home);
    let mut entries = Vec::new();

    for transcript in discover_codescribe_transcripts(home) {
        match parse_codescribe_transcript_with_lexicon(
            &transcript.path,
            transcript.date,
            config,
            &lexicon,
            home,
        ) {
            Ok(mut parsed) => entries.append(&mut parsed),
            Err(e) => eprintln!(
                "Codescribe transcript extraction warning ({}): {}",
                transcript.path.display(),
                e
            ),
        }
    }

    if !config.project_filter.is_empty() {
        entries.retain(|entry| {
            if let Some(cwd) = &entry.cwd {
                project_filter_matches_path(cwd, &config.project_filter)
            } else {
                false
            }
        });
    }

    entries.sort_by_key(|entry| entry.timestamp);
    Ok(entries)
}

fn resolve_codescribe_cwd_hint(home: &Path, project_hint: Option<&str>) -> Option<String> {
    let project = project_hint?.trim();
    if project.is_empty() {
        return None;
    }

    let (org, repo) = project.split_once('/').unwrap_or(("", project));

    let candidates = if !org.is_empty() {
        vec![
            home.join(org).join(repo),
            home.join("Libraxis").join(org).join(repo),
            home.join("Libraxis")
                .join("01_deployed_libraxis_vm")
                .join(org)
                .join(repo),
            home.join("Libraxis")
                .join("vc-runtime")
                .join(org)
                .join(repo),
            home.join("hosted").join(org).join(repo),
            home.join("vc-workspace").join(org).join(repo),
        ]
    } else {
        vec![
            home.join(repo),
            home.join("Libraxis").join(repo),
            home.join("Libraxis")
                .join("01_deployed_libraxis_vm")
                .join(repo),
            home.join("Libraxis").join("vc-runtime").join(repo),
            home.join("hosted").join("Vetcoders").join(repo),
            home.join("vc-workspace").join("Vetcoders").join(repo),
        ]
    };

    candidates
        .into_iter()
        .find(|candidate| candidate.is_dir())
        .map(|candidate| candidate.display().to_string())
}
