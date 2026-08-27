//! Deterministic content and evidence identity.
//!
//! Package identity (W1-T5) is the pair (store id, content hash of the
//! source). A store id alone is a handle the resolver may substitute; the
//! hash pins which bytes the handle stood for. Frame identity (A2) is the
//! content hash of the body — transport ids such as codex `msg_*` rotate
//! after compaction while the body stays identical.

use super::source::AgentKind;
use serde::{Deserialize, Serialize};
use std::fmt;

pub const EVIDENCE_ID_VERSION: &str = "ev1";

/// Identity of one parsed package: which store entry, and which exact bytes.
///
/// Equality is on both halves. Two packages with the same `store_id` and a
/// different `content_hash` are different packages (fork / rewrite); the
/// same hash under two store ids is the same source stored twice.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PackageIdentity {
    /// Store-side handle (`source_id` / session id as the store keys it).
    pub store_id: String,
    /// SHA-256 of the original source material (`Provenance::original_source_hash`).
    pub content_hash: String,
}

impl PackageIdentity {
    pub fn new(
        store_id: impl Into<String>,
        content_hash: impl Into<String>,
    ) -> Result<Self, EvidenceIdError> {
        let store_id = store_id.into();
        let content_hash = content_hash.into();
        validate_token("store_id", &store_id)?;
        validate_hash(&content_hash)?;
        Ok(Self {
            store_id,
            content_hash,
        })
    }

    /// Short, stable rendering `<store_id>@<hash[..16]>` for reports.
    pub fn short(&self) -> String {
        format!("{}@{}", self.store_id, &self.content_hash[..16])
    }

    /// Same bytes, regardless of which store id they sit under.
    pub fn same_content(&self, other: &Self) -> bool {
        self.content_hash == other.content_hash
    }
}

impl fmt::Display for PackageIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}@{}", self.store_id, self.content_hash)
    }
}

/// Identity of one transport frame inside a package: body hash first,
/// transport id only as an annotation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FrameIdentity {
    /// The package the frame belongs to.
    pub package: PackageIdentity,
    /// SHA-256 of the frame body bytes. This is the identity.
    pub body_hash: String,
    /// Transport-level id (`msg_*`, `uuid`, …) if the transport had one.
    /// Informational: never used for equality or dedup (A2).
    pub transport_id: Option<String>,
}

impl FrameIdentity {
    pub fn from_body(package: PackageIdentity, body: &[u8], transport_id: Option<String>) -> Self {
        Self {
            package,
            body_hash: sha256_hex(body),
            transport_id,
        }
    }

    /// Two frames are the same utterance when package and body hash match,
    /// whatever their transport ids say.
    pub fn same_utterance(&self, other: &Self) -> bool {
        self.package == other.package && self.body_hash == other.body_hash
    }
}

pub fn evidence_event_id(
    agent: AgentKind,
    session_id: &str,
    locator: &str,
    unit_kind: &str,
    raw_unit_bytes: &[u8],
) -> Result<String, EvidenceIdError> {
    evidence_event_id_from_hash(
        agent,
        session_id,
        locator,
        unit_kind,
        &sha256_hex(raw_unit_bytes),
    )
}

pub fn evidence_event_id_from_hash(
    agent: AgentKind,
    session_id: &str,
    locator: &str,
    unit_kind: &str,
    content_hash: &str,
) -> Result<String, EvidenceIdError> {
    validate_token("session_id", session_id)?;
    validate_token("locator", locator)?;
    validate_token("unit_kind", unit_kind)?;
    validate_hash(content_hash)?;
    Ok(format!(
        "{EVIDENCE_ID_VERSION}:{}:{session_id}:{locator}:{unit_kind}:{}",
        agent.as_str(),
        &content_hash[..16]
    ))
}

pub fn ordinal_locator(ordinal: u64) -> String {
    format!("{ordinal:06}")
}

fn validate_hash(content_hash: &str) -> Result<(), EvidenceIdError> {
    if content_hash.len() != 64 || !content_hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(EvidenceIdError::InvalidHash);
    }
    Ok(())
}

fn validate_token(label: &'static str, token: &str) -> Result<(), EvidenceIdError> {
    if token.is_empty()
        || token.len() > 512
        || token.contains(['/', '\\'])
        || token.chars().any(char::is_control)
    {
        return Err(EvidenceIdError::InvalidToken {
            label,
            value: token.to_owned(),
        });
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceIdError {
    InvalidToken { label: &'static str, value: String },
    InvalidHash,
}

impl fmt::Display for EvidenceIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidToken { label, value } => write!(formatter, "invalid {label}: {value:?}"),
            Self::InvalidHash => formatter.write_str("content hash must be 64 hexadecimal bytes"),
        }
    }
}

impl std::error::Error for EvidenceIdError {}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = sha256(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

// Small self-contained SHA-256 keeps the frozen identity contract inside the
// parser crate without adding a runtime process or a new package dependency.
fn sha256(input: &[u8]) -> [u8; 32] {
    const INITIAL: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let bit_len = (input.len() as u64).wrapping_mul(8);
    let padded_len = (input.len() + 9).div_ceil(64) * 64;
    let mut padded = Vec::with_capacity(padded_len);
    padded.extend_from_slice(input);
    padded.push(0x80);
    padded.resize(padded_len - 8, 0);
    padded.extend_from_slice(&bit_len.to_be_bytes());

    let mut state = INITIAL;
    for chunk in padded.chunks_exact(64) {
        let mut schedule = [0_u32; 64];
        for (index, word) in chunk.chunks_exact(4).enumerate() {
            schedule[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for index in 16..64 {
            let s0 = schedule[index - 15].rotate_right(7)
                ^ schedule[index - 15].rotate_right(18)
                ^ (schedule[index - 15] >> 3);
            let s1 = schedule[index - 2].rotate_right(17)
                ^ schedule[index - 2].rotate_right(19)
                ^ (schedule[index - 2] >> 10);
            schedule[index] = schedule[index - 16]
                .wrapping_add(s0)
                .wrapping_add(schedule[index - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let sigma1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(sigma1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(schedule[index]);
            let sigma0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = sigma0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }

    let mut digest = [0_u8; 32];
    for (chunk, word) in digest.chunks_exact_mut(4).zip(state) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    digest
}

#[cfg(test)]
mod tests {
    use super::{FrameIdentity, PackageIdentity, sha256_hex};

    #[test]
    fn package_identity_is_the_pair() {
        let empty = sha256_hex(b"");
        let abc = sha256_hex(b"abc");
        let a = PackageIdentity::new("store", empty.clone()).unwrap();
        let b = PackageIdentity::new("store", abc).unwrap();
        let c = PackageIdentity::new("other", empty).unwrap();
        assert_ne!(a, b, "same store id, different bytes: different package");
        assert_ne!(a, c, "same bytes, different store id: different handle");
        assert!(a.same_content(&c));
        assert_eq!(a.short(), "store@e3b0c44298fc1c14");
    }

    #[test]
    fn frame_identity_ignores_rotated_transport_ids() {
        let package = PackageIdentity::new("store", sha256_hex(b"src")).unwrap();
        let first = FrameIdentity::from_body(package.clone(), b"body", Some("msg_1".into()));
        let second = FrameIdentity::from_body(package, b"body", Some("msg_2".into()));
        assert!(first.same_utterance(&second));
    }

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
