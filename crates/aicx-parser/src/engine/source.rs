//! Explicit parser inputs. Source discovery lives outside the parser kernel.

use crate::sanitize::validate_read_path;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

/// Parser adapters supported by the frozen kernel contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Codex,
    Claude,
    Gemini,
    Grok,
    Junie,
}

impl AgentKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Gemini => "gemini",
            Self::Grok => "grok",
            Self::Junie => "junie",
        }
    }
}

/// Physical framing of one explicitly selected source artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceFraming {
    JsonLines,
    WholeDocument,
    Opaque,
}

#[derive(Debug, Clone)]
enum ArtifactInput {
    ValidatedFile(PathBuf),
    Memory(Vec<u8>),
}

/// One named artifact belonging to a selected source.
///
/// Fields are private intentionally: callers must use constructors that either
/// validate a path or provide explicit bytes. There is no directory/container
/// discovery constructor.
#[derive(Debug, Clone)]
pub struct SourceArtifact {
    name: String,
    framing: SourceFraming,
    input: ArtifactInput,
}

impl SourceArtifact {
    pub fn validated_file(
        name: impl Into<String>,
        path: impl AsRef<Path>,
        framing: SourceFraming,
    ) -> Result<Self, SourceError> {
        let name = validate_component("artifact name", name.into())?;
        let path = validate_read_path(path.as_ref()).map_err(|error| SourceError::InvalidPath {
            path: path.as_ref().to_path_buf(),
            message: error.to_string(),
        })?;
        if !path.is_file() {
            return Err(SourceError::InvalidPath {
                path,
                message: "selected artifact is not a regular file".to_owned(),
            });
        }
        Ok(Self {
            name,
            framing,
            input: ArtifactInput::ValidatedFile(path),
        })
    }

    pub fn memory(
        name: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
        framing: SourceFraming,
    ) -> Result<Self, SourceError> {
        Ok(Self {
            name: validate_component("artifact name", name.into())?,
            framing,
            input: ArtifactInput::Memory(bytes.into()),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn framing(&self) -> SourceFraming {
        self.framing
    }

    pub(crate) fn validated_path(&self) -> Option<&Path> {
        match &self.input {
            ArtifactInput::ValidatedFile(path) => Some(path),
            ArtifactInput::Memory(_) => None,
        }
    }

    pub(crate) fn memory_bytes(&self) -> Option<&[u8]> {
        match &self.input {
            ArtifactInput::ValidatedFile(_) => None,
            ArtifactInput::Memory(bytes) => Some(bytes),
        }
    }
}

/// A fully resolved parser input.
///
/// `SourceHandle` is deliberately a sealed data contract rather than a locator:
/// it contains a finite list of artifacts and exposes no filesystem discovery.
///
/// ```compile_fail
/// use aicx_parser::engine::{AgentKind, SourceHandle};
/// let _ = SourceHandle { agent: AgentKind::Codex };
/// ```
#[derive(Debug, Clone)]
pub struct SourceHandle {
    agent: AgentKind,
    source_id: String,
    logical_session_id: Option<String>,
    artifacts: Vec<SourceArtifact>,
}

impl SourceHandle {
    pub fn new(
        agent: AgentKind,
        source_id: impl Into<String>,
        logical_session_id: Option<String>,
        artifacts: Vec<SourceArtifact>,
    ) -> Result<Self, SourceError> {
        let source_id = validate_component("source id", source_id.into())?;
        let logical_session_id = logical_session_id
            .map(|id| validate_component("logical session id", id))
            .transpose()?;
        if artifacts.is_empty() {
            return Err(SourceError::EmptyArtifacts);
        }
        let mut names = BTreeSet::new();
        for artifact in &artifacts {
            if !names.insert(artifact.name.clone()) {
                return Err(SourceError::DuplicateArtifact(artifact.name.clone()));
            }
        }
        Ok(Self {
            agent,
            source_id,
            logical_session_id,
            artifacts,
        })
    }

    pub const fn agent(&self) -> AgentKind {
        self.agent
    }

    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    pub fn logical_session_id(&self) -> Option<&str> {
        self.logical_session_id.as_deref()
    }

    pub fn artifacts(&self) -> &[SourceArtifact] {
        &self.artifacts
    }
}

fn validate_component(label: &'static str, value: String) -> Result<String, SourceError> {
    let invalid = value.is_empty()
        || value.len() > 512
        || value.starts_with('.')
        || value.contains(['/', '\\'])
        || value.chars().any(char::is_control);
    if invalid {
        return Err(SourceError::InvalidComponent { label, value });
    }
    Ok(value)
}

#[derive(Debug)]
pub enum SourceError {
    EmptyArtifacts,
    DuplicateArtifact(String),
    InvalidComponent { label: &'static str, value: String },
    InvalidPath { path: PathBuf, message: String },
}

impl fmt::Display for SourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyArtifacts => {
                formatter.write_str("source handle requires at least one artifact")
            }
            Self::DuplicateArtifact(name) => write!(formatter, "duplicate source artifact: {name}"),
            Self::InvalidComponent { label, value } => {
                write!(formatter, "invalid {label}: {value:?}")
            }
            Self::InvalidPath { path, message } => {
                write!(
                    formatter,
                    "invalid source path '{}': {message}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for SourceError {}
