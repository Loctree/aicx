#![allow(unused_imports)]
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::sanitize;
use crate::store::project_filter_matches;
use crate::timeline::FrameKind;
use crate::timeline::{
    CollapseStubKind, ConversationMessage, ExtractionConfig, MessageKind, SourceInfo, TimelineEntry,
};

pub mod conversation;
pub mod files;
mod importer_support;
pub mod list;
pub mod project;

pub use conversation::{
    ConversationProjection, is_harness_injected_noise, to_conversation, to_conversation_with_stats,
};
pub(crate) use conversation::{IntentLineModality, intent_line_modality};
pub(crate) use files::{MAX_LINE_BYTES, walk_jsonl_files};
pub(crate) use importer_support::{
    TimelineEntryMeta, build_timeline_entry, source_path_and_sha256,
};
pub use list::list_available_sources;
pub(crate) use project::*;
pub use project::{
    decode_claude_project_path, detect_project_name, infer_repo_name_from_current_dir,
    repo_labels_from_entries, repo_name_from_cwd,
};
