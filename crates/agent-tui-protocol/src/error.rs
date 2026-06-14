//! Standard error codes returned in [`crate::Response`] envelopes.
//!
//! Every code has a stable string identifier and a stable numeric code.
//! Agents are encouraged to branch on `numeric_code`, which never changes
//! within the v1 series; new codes are added in a new range.
//!
//! See `docs/design/RFC.md` §5.4.

use serde::{Deserialize, Serialize};

/// Stable error-code enum returned to callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    /// Reference handle (`@eN`) not found in the current ref map.
    RefNotFound,
    /// Reference handle is from a prior pane generation.
    RefStale,
    /// Operation needs an active pane and none was specified or focused.
    NoActivePane,
    /// The pane's PTY child has exited.
    PaneDead,
    /// Per-pane queue rejected the request (callers in `--no-wait` mode).
    PaneBusy,
    /// `eval` requested an adapter that isn't currently attached.
    AdapterMissing,
    /// An attached adapter returned an error.
    AdapterFailed,
    /// Adapter was attached but the program identity has since changed.
    AdapterUnattached,
    /// `press --to` named a target the attached adapter does not
    /// recognize. See `docs/design/addressing-rfc.md` §2.3.
    RoutingUnsupported,
    /// A `WaitFor` step in a routed write did not fire within its
    /// `max_wait_ms` budget. See `docs/design/addressing-rfc.md` §2.3.
    RoutingGateTimeout,
    /// `wait` did not satisfy its condition within `--timeout`.
    WaitTimeout,
    /// `wait --hash h` was given a hash not present in the seq→hash window.
    WaitHashUnknown,
    /// Governance interceptor denied the action.
    PolicyDenied,
    /// Governance interceptor requires explicit human confirmation.
    PolicyPending,
    /// Flag or argument parsing failed.
    InvalidArgs,
    /// `press` keymap notation could not be parsed.
    KeyFormatError,
    /// CLI could not connect to a daemon socket.
    DaemonUnreachable,
    /// Daemon is in the middle of an idle-shutdown.
    DaemonShuttingDown,
    /// CLI and daemon protocol versions are incompatible.
    DaemonVersionDrift,
    /// As `DaemonVersionDrift`, but there are non-shell panes that would be
    /// killed by replacing the daemon — caller must pass `--upgrade-force`.
    DaemonVersionDriftActive,
    /// Daemon is at its connection limit.
    SocketBusy,
    /// State file present but key wrong or AEAD verification failed.
    StateDecryptFailed,
    /// State file present, but its schema does not match a known version.
    StateFormatError,
    /// Disk-full or memory-budget exceeded.
    ResourceExhausted,
    /// Internal bug. Should never be the user's fault.
    Internal,
}

impl ErrorCode {
    /// Stable numeric code. Agents may branch on this without parsing the
    /// string form. New codes are appended in their own range; existing
    /// numbers are never recycled.
    #[must_use]
    pub const fn numeric(self) -> u32 {
        match self {
            Self::RefNotFound => 1001,
            Self::RefStale => 1004,
            Self::NoActivePane => 1005,
            Self::PaneDead => 1006,
            Self::PaneBusy => 1007,
            Self::AdapterMissing => 2001,
            Self::AdapterFailed => 2002,
            Self::AdapterUnattached => 2003,
            Self::RoutingUnsupported => 2004,
            Self::RoutingGateTimeout => 2005,
            Self::WaitTimeout => 3001,
            Self::WaitHashUnknown => 3002,
            Self::PolicyDenied => 4001,
            Self::PolicyPending => 4002,
            Self::InvalidArgs => 5001,
            Self::KeyFormatError => 5002,
            Self::DaemonUnreachable => 6001,
            Self::DaemonShuttingDown => 6002,
            Self::DaemonVersionDrift => 6003,
            Self::DaemonVersionDriftActive => 6004,
            Self::SocketBusy => 6005,
            Self::StateDecryptFailed => 7001,
            Self::StateFormatError => 7002,
            Self::ResourceExhausted => 8001,
            Self::Internal => 9001,
        }
    }
}

/// Errors raised while encoding/decoding the wire protocol itself.
///
/// These are distinct from [`ErrorCode`], which is the *response payload*
/// for command-level failures. A `ProtocolError` means we could not even
/// frame the request/response cleanly.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    /// Underlying `serde_json` failure.
    #[error("json decode/encode: {0}")]
    Json(#[from] serde_json::Error),

    /// The line was empty, malformed, or framed wrong.
    #[error("malformed wire frame: {0}")]
    Frame(String),

    /// The peer sent a wire-version we do not understand.
    #[error("protocol version mismatch: peer={peer}, ours={ours}")]
    VersionMismatch {
        /// Peer-advertised protocol version.
        peer: u32,
        /// Our protocol version constant.
        ours: u32,
    },
}

/// Convenience: turn an `ErrorCode` into its string slug.
impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Use serde_json to get the SCREAMING_SNAKE_CASE rendering for free.
        let json = serde_json::to_string(self).map_err(|_| std::fmt::Error)?;
        // json is `"REF_STALE"` — trim quotes.
        write!(f, "{}", json.trim_matches('"'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_codes_are_unique() {
        let codes = [
            ErrorCode::RefNotFound,
            ErrorCode::RefStale,
            ErrorCode::NoActivePane,
            ErrorCode::PaneDead,
            ErrorCode::PaneBusy,
            ErrorCode::AdapterMissing,
            ErrorCode::AdapterFailed,
            ErrorCode::AdapterUnattached,
            ErrorCode::WaitTimeout,
            ErrorCode::WaitHashUnknown,
            ErrorCode::PolicyDenied,
            ErrorCode::PolicyPending,
            ErrorCode::InvalidArgs,
            ErrorCode::KeyFormatError,
            ErrorCode::DaemonUnreachable,
            ErrorCode::DaemonShuttingDown,
            ErrorCode::DaemonVersionDrift,
            ErrorCode::DaemonVersionDriftActive,
            ErrorCode::SocketBusy,
            ErrorCode::StateDecryptFailed,
            ErrorCode::StateFormatError,
            ErrorCode::ResourceExhausted,
            ErrorCode::Internal,
        ];
        let mut seen = std::collections::HashSet::new();
        for c in codes {
            assert!(seen.insert(c.numeric()), "duplicate numeric code for {c:?}");
        }
    }

    #[test]
    fn display_matches_serde_slug() {
        assert_eq!(ErrorCode::RefStale.to_string(), "REF_STALE");
        assert_eq!(
            ErrorCode::DaemonVersionDriftActive.to_string(),
            "DAEMON_VERSION_DRIFT_ACTIVE"
        );
    }
}
