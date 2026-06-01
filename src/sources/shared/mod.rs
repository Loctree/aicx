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
pub mod diagnostics;
pub mod files;
pub mod json;
pub mod project;
pub mod timeline;

pub use conversation::{ConversationProjection, to_conversation, to_conversation_with_stats};
pub(crate) use diagnostics::*;
pub(crate) use files::{
    MAX_LINE_BYTES, observe_oversized_line, parse_rfc3339_or_naive_utc, short_path_hash,
    walk_files, walk_jsonl_files,
};
pub(crate) use json::*;
pub(crate) use project::*;
pub use project::{
    decode_claude_project_path, detect_project_name, repo_labels_from_entries, repo_name_from_cwd,
};
pub(crate) use timeline::*;
