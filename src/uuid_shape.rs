//! The one UUID shape check for session source admission (cursor transcript
//! stems, kimi session dirs). Dependency-free by contract: `session_catalog`
//! is compiled both into the app crate and standalone into
//! `tests/session_catalog.rs` via `#[path]`, so this module must stay free of
//! `crate::` imports to resolve in both roots.

pub(crate) fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.as_bytes().iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}
