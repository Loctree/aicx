//! Bounded reader for the finite artifact set inside a [`SourceHandle`].

use super::identity::sha256_hex;
use super::source::{SourceArtifact, SourceFraming, SourceHandle};
use crate::sanitize::open_file_validated;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::Read;

pub const DEFAULT_MAX_SOURCE_BYTES: usize = 256 * 1024 * 1024;
pub const DEFAULT_MAX_UNIT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReaderPolicy {
    pub max_source_bytes: usize,
    pub max_unit_bytes: usize,
}

impl Default for ReaderPolicy {
    fn default() -> Self {
        Self {
            max_source_bytes: DEFAULT_MAX_SOURCE_BYTES,
            max_unit_bytes: DEFAULT_MAX_UNIT_BYTES,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitBoundary {
    Complete,
    UnterminatedTail,
    Oversized,
}

/// A bounded representation of one physical unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawUnit {
    pub coverage_ordinal: u64,
    pub physical_ordinal: u64,
    pub artifact_name: String,
    pub framing: SourceFraming,
    pub bytes: Vec<u8>,
    pub original_bytes: u64,
    pub content_hash: String,
    pub boundary: UnitBoundary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRead {
    pub units: Vec<RawUnit>,
    pub source_bytes: u64,
    pub source_hash: String,
}

#[derive(Debug, Clone)]
pub struct RawUnitReader {
    policy: ReaderPolicy,
}

impl RawUnitReader {
    pub const fn new(policy: ReaderPolicy) -> Self {
        Self { policy }
    }

    pub const fn policy(&self) -> ReaderPolicy {
        self.policy
    }

    pub fn read(&self, source: &SourceHandle) -> Result<SourceRead, ReaderError> {
        if self.policy.max_source_bytes == 0 || self.policy.max_unit_bytes == 0 {
            return Err(ReaderError::InvalidPolicy);
        }
        let mut units = Vec::new();
        let mut source_material = Vec::new();
        let single_artifact = source.artifacts().len() == 1;
        let mut single_source_hash = None;
        let mut source_bytes = 0_u64;
        for artifact in source.artifacts() {
            let bytes = read_artifact(artifact, self.policy.max_source_bytes)?;
            source_bytes = source_bytes.checked_add(bytes.len() as u64).ok_or(
                ReaderError::SourceTooLarge {
                    actual_bytes: u64::MAX,
                    max_bytes: self.policy.max_source_bytes,
                },
            )?;
            if source_bytes > self.policy.max_source_bytes as u64 {
                return Err(ReaderError::SourceTooLarge {
                    actual_bytes: source_bytes,
                    max_bytes: self.policy.max_source_bytes,
                });
            }
            let artifact_hash = sha256_hex(&bytes);
            if single_artifact {
                single_source_hash = Some(artifact_hash);
            } else {
                source_material.extend_from_slice(&(artifact.name().len() as u64).to_be_bytes());
                source_material.extend_from_slice(artifact.name().as_bytes());
                source_material.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
                source_material.extend_from_slice(artifact_hash.as_bytes());
            }
            append_units(artifact, &bytes, self.policy.max_unit_bytes, &mut units);
        }
        Ok(SourceRead {
            units,
            source_bytes,
            source_hash: single_source_hash.unwrap_or_else(|| sha256_hex(&source_material)),
        })
    }
}

fn read_artifact(artifact: &SourceArtifact, max_bytes: usize) -> Result<Vec<u8>, ReaderError> {
    if let Some(bytes) = artifact.memory_bytes() {
        if bytes.len() > max_bytes {
            return Err(ReaderError::ArtifactTooLarge {
                artifact: artifact.name().to_owned(),
                actual_bytes: bytes.len() as u64,
                max_bytes,
            });
        }
        return Ok(bytes.to_vec());
    }

    let path = artifact
        .validated_path()
        .expect("source artifact has exactly one input variant");
    let file = open_file_validated(path).map_err(|error| ReaderError::Open {
        artifact: artifact.name().to_owned(),
        message: error.to_string(),
    })?;
    let metadata_len = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    if metadata_len > max_bytes as u64 {
        return Err(ReaderError::ArtifactTooLarge {
            artifact: artifact.name().to_owned(),
            actual_bytes: metadata_len,
            max_bytes,
        });
    }
    let mut bytes = Vec::with_capacity(metadata_len as usize);
    file.take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| ReaderError::Read {
            artifact: artifact.name().to_owned(),
            message: error.to_string(),
        })?;
    if bytes.len() > max_bytes {
        return Err(ReaderError::ArtifactTooLarge {
            artifact: artifact.name().to_owned(),
            actual_bytes: bytes.len() as u64,
            max_bytes,
        });
    }
    Ok(bytes)
}

fn append_units(
    artifact: &SourceArtifact,
    bytes: &[u8],
    max_unit_bytes: usize,
    output: &mut Vec<RawUnit>,
) {
    match artifact.framing() {
        SourceFraming::JsonLines => {
            let terminated = bytes.ends_with(b"\n");
            let mut units = bytes.split(|byte| *byte == b'\n').peekable();
            let mut index = 0_u64;
            while let Some(raw) = units.next() {
                let is_last = units.peek().is_none();
                if raw.is_empty() && is_last && terminated {
                    continue;
                }
                index += 1;
                let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
                let tail = !terminated && is_last;
                push_unit(artifact, raw, index, tail, max_unit_bytes, output);
            }
        }
        SourceFraming::WholeDocument | SourceFraming::Opaque => {
            push_unit(artifact, bytes, 1, false, max_unit_bytes, output);
        }
    }
}

fn push_unit(
    artifact: &SourceArtifact,
    raw: &[u8],
    physical_ordinal: u64,
    unterminated_tail: bool,
    max_unit_bytes: usize,
    output: &mut Vec<RawUnit>,
) {
    let oversized = raw.len() > max_unit_bytes;
    output.push(RawUnit {
        coverage_ordinal: output.len() as u64 + 1,
        physical_ordinal,
        artifact_name: artifact.name().to_owned(),
        framing: artifact.framing(),
        bytes: raw[..raw.len().min(max_unit_bytes)].to_vec(),
        original_bytes: raw.len() as u64,
        content_hash: sha256_hex(raw),
        boundary: if oversized {
            UnitBoundary::Oversized
        } else if unterminated_tail {
            UnitBoundary::UnterminatedTail
        } else {
            UnitBoundary::Complete
        },
    });
}

#[derive(Debug)]
pub enum ReaderError {
    InvalidPolicy,
    SourceTooLarge {
        actual_bytes: u64,
        max_bytes: usize,
    },
    ArtifactTooLarge {
        artifact: String,
        actual_bytes: u64,
        max_bytes: usize,
    },
    Open {
        artifact: String,
        message: String,
    },
    Read {
        artifact: String,
        message: String,
    },
}

impl fmt::Display for ReaderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPolicy => formatter.write_str("reader byte caps must be non-zero"),
            Self::SourceTooLarge {
                actual_bytes,
                max_bytes,
            } => write!(
                formatter,
                "source exceeds cap: {actual_bytes} > {max_bytes} bytes"
            ),
            Self::ArtifactTooLarge {
                artifact,
                actual_bytes,
                max_bytes,
            } => write!(
                formatter,
                "artifact {artifact:?} exceeds cap: {actual_bytes} > {max_bytes} bytes"
            ),
            Self::Open { artifact, message } => {
                write!(
                    formatter,
                    "cannot validated-open artifact {artifact:?}: {message}"
                )
            }
            Self::Read { artifact, message } => {
                write!(formatter, "cannot read artifact {artifact:?}: {message}")
            }
        }
    }
}

impl std::error::Error for ReaderError {}
