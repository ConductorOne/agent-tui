//! Typed `Action` records that flow through the governance interceptor.
//!
//! See `docs/design/RFC.md` §11.1.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::PaneId;

/// What kind of action is being authorized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    /// `spawn` of a PTY child.
    Spawn,
    /// Input bytes (`press`/`type`/`send_ansi`/`click`).
    Input,
    /// `eval` against an adapter.
    Eval,
    /// `state save`.
    StateSave,
    /// `state load`.
    StateLoad,
    /// Attaching an adapter to a pane.
    AdapterAttach,
}

/// Per-kind payload supplied to the evaluator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActionDetail {
    /// `spawn <argv...>` in `cwd` with the merged `env`.
    Spawn {
        /// argv as the caller wants it executed.
        argv: Vec<String>,
        /// Working directory for the spawn.
        cwd: String,
    },
    /// Raw input bytes plus optional key-token representation.
    Input {
        /// Hex-encoded bytes (so the evaluator can match e.g. `\x03`).
        bytes_hex: String,
        /// If the bytes came from `press`, the parsed key tokens; else `None`.
        key_tokens: Option<Vec<String>>,
    },
    /// `eval --adapter <name> '<expr>'`.
    Eval {
        /// Adapter name that will execute the expression.
        adapter: String,
        /// The expression string.
        expr: String,
    },
    /// `state save <path>`.
    StateSave {
        /// Resolved absolute path the daemon would write to.
        path: String,
        /// True if the body will be AES-256-GCM encrypted at rest.
        encrypted: bool,
    },
    /// `state load <path>`.
    StateLoad {
        /// Resolved absolute path the daemon would read from.
        path: String,
    },
    /// Attach an adapter to a pane.
    AdapterAttach {
        /// Adapter registry key.
        adapter: String,
    },
}

/// Verdict returned by an evaluator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Permit the action immediately.
    Allow,
    /// Reject the action outright.
    Deny,
    /// Require explicit confirmation before proceeding.
    RequireConfirm,
}

/// Decision record returned by an evaluator and emitted to the audit firehose.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    /// Stable id for tying together the decision and any later confirmation.
    pub audit_id: Uuid,
    /// The verdict itself.
    pub verdict: Verdict,
    /// Human-readable reason. Surfaced to callers in `hint` and to humans
    /// in audit UIs.
    pub reason: String,
}

/// A complete action submitted to the evaluator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    /// What kind of action.
    pub kind: ActionKind,
    /// Pane the action targets (if any).
    pub pane: Option<PaneId>,
    /// Typed per-kind payload.
    pub detail: ActionDetail,
    /// Who is asking. Includes whether agent-mode (boundary markers etc.)
    /// is in effect.
    pub caller: CallerInfo,
}

/// Information about the caller (the agent harness invoking the daemon).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CallerInfo {
    /// True iff `AGENT_TUI_AGENT_MODE=1` (or `--content-boundaries` on cmd).
    pub agent_mode: bool,
    /// Optional caller identity tag (e.g. `"claude-code"`, `"codex"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
}

/// One row in the audit firehose. Emitted for every governance decision
/// regardless of verdict. See `docs/design/RFC.md` §11.6.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Session id (string; empty when not pane-scoped).
    pub session: String,
    /// Pane id (`None` for session-level actions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<String>,
    /// Action kind that was evaluated.
    pub action_kind: ActionKind,
    /// Decision verdict.
    pub verdict: Verdict,
    /// Human-readable reason (from the evaluator).
    pub reason: String,
    /// Wall-clock timestamp.
    pub at: chrono::DateTime<chrono::Utc>,
    /// Action-specific detail (`Spawn { argv, cwd }`, `Input { bytes_hex, key_tokens }`, ...).
    pub detail_json: serde_json::Value,
}
