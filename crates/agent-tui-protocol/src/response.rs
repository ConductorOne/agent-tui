//! Response envelope returned for every request.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ErrorCode, Generation, PaneId, Sequence, SessionId};

/// Per-snapshot content-boundary delimiters for prompt-injection defense.
///
/// See `docs/RFC.md` §11.3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutputDelim {
    /// Opening delimiter, e.g. `"<<<AGENT_TUI_OUTPUT_a7b3c91d>>>"`.
    pub start: String,
    /// Closing delimiter, e.g. `"<<<END_a7b3c91d>>>"`.
    pub end: String,
}

impl ToolOutputDelim {
    /// Construct a delimiter pair from an 8-hex-char nonce.
    #[must_use]
    pub fn from_nonce(nonce_hex: &str) -> Self {
        Self {
            start: format!("<<<AGENT_TUI_OUTPUT_{nonce_hex}>>>"),
            end: format!("<<<END_{nonce_hex}>>>"),
        }
    }
}

/// Body of an error response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    /// Stable string code.
    pub code: ErrorCode,
    /// Stable numeric code.
    pub numeric_code: u32,
    /// Human-readable error message.
    pub message: String,
    /// Recovery hint. **Required** on every error response.
    pub hint: String,
}

impl ErrorBody {
    /// Build an error body with the canonical numeric code.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            numeric_code: code.numeric(),
            code,
            message: message.into(),
            hint: hint.into(),
        }
    }
}

/// Non-fatal warning attached to an otherwise-successful response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Warning {
    /// Stable identifier so agents can branch on the cause.
    pub code: String,
    /// Human-readable message.
    pub message: String,
}

/// The success/error sum, modeled as a flat struct to avoid the known
/// serde-`untagged-enum + flatten` footgun. `success` is the canonical
/// discriminator — `true` ⇒ `data` may be set, `error` is `None`;
/// `false` ⇒ `error` is set, `data` is `None`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    /// `true` for success, `false` for failure.
    pub success: bool,
    /// Command-specific payload. Present only on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// Error payload. Present only on failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
    /// Optional non-fatal warning (only meaningful on success).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<Warning>,
}

impl Response {
    /// Build a successful response carrying `data`.
    #[must_use]
    pub fn ok(data: serde_json::Value) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
            warning: None,
        }
    }

    /// Build a successful response with no data payload.
    #[must_use]
    pub fn ok_empty() -> Self {
        Self {
            success: true,
            data: None,
            error: None,
            warning: None,
        }
    }

    /// Build a failure response from an [`ErrorBody`].
    #[must_use]
    pub fn err(error: ErrorBody) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(error),
            warning: None,
        }
    }

    /// Attach a non-fatal warning to a successful response. No-op on failure.
    #[must_use]
    pub fn with_warning(mut self, warning: Warning) -> Self {
        if self.success {
            self.warning = Some(warning);
        }
        self
    }

    /// Convenience: is this a failure response?
    #[must_use]
    pub fn is_failure(&self) -> bool {
        !self.success
    }
}

/// The full wire envelope written back to the client.
///
/// See `docs/RFC.md` §5.3.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    /// Correlation id of the matching request.
    pub id: Uuid,
    /// Daemon's protocol version.
    pub protocol: u32,
    /// Daemon's semver string.
    pub version: String,
    /// Session id; `None` for non-pane RPCs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    /// Pane id; `None` for non-pane RPCs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<PaneId>,
    /// Pane generation at response time; `None` for non-pane RPCs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<Generation>,
    /// Pane sequence at response time; `None` for non-pane RPCs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<Sequence>,
    /// Total elapsed time on the daemon side in milliseconds.
    pub elapsed_ms: u64,
    /// Per-snapshot content-boundary delimiters. Present only on commands
    /// whose payload may contain untrusted TUI bytes (snapshot, get text, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_output_delim: Option<ToolOutputDelim>,
    /// The actual response.
    #[serde(flatten)]
    pub response: Response,
}
