use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SteerIncompatible {
    RebuildRequired { reason: String },
    NotBootstrapped { reason: String },
}

impl SteerIncompatible {
    pub(super) fn rebuild_required(reason: impl Into<String>) -> Self {
        Self::RebuildRequired {
            reason: reason.into(),
        }
    }

    pub(super) fn not_bootstrapped(reason: impl Into<String>) -> Self {
        Self::NotBootstrapped {
            reason: reason.into(),
        }
    }
}

impl fmt::Display for SteerIncompatible {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RebuildRequired { reason } => {
                write!(f, "steer index requires rebuild: {reason}")
            }
            Self::NotBootstrapped { reason } => {
                write!(f, "steer index is not bootstrapped: {reason}")
            }
        }
    }
}

impl std::error::Error for SteerIncompatible {}
