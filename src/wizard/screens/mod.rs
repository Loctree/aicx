pub mod corpus;
pub mod doctor;
pub mod intents;
pub mod store;

/// Clamped list-selection arithmetic shared by every wizard screen.
/// Empty lists collapse the cursor to `0`; movement saturates at both
/// ends instead of wrapping.
pub(crate) fn move_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    if delta < 0 {
        current.saturating_sub(delta.unsigned_abs()).min(len - 1)
    } else {
        current.saturating_add(delta as usize).min(len - 1)
    }
}
