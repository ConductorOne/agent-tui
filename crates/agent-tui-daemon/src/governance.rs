//! Typed `Action` interceptor — the governance choke point.
//!
//! Every mutation handler funnels through `Governance::check`, which builds
//! a typed `Action`, runs it through the configured `Evaluator`, and returns
//! either an `Allow` (handler proceeds) or a `Deny` (handler returns a
//! `POLICY_DENIED` response without touching the PTY). Every decision —
//! Allow, Deny, or `RequireConfirm` — emits an `AuditEvent` on a broadcast
//! channel for downstream consumers (P5 ships an HTTP `/firehose` reader).
//!
//! Two evaluators ship in v1:
//!  - [`AllowAllEvaluator`] — always Allows. Useful in unsafe-by-default test
//!    setups and as the trivial baseline.
//!  - [`AllowlistEvaluator`] — checks `Spawn` actions against a binary
//!    allowlist (CSV via `--allowed-binaries` or env). `Input`/`Eval` pass
//!    through. Wildcards (`*`) are honored but audit-logged.
//!
//! OPA-WASM (`agent-tui --policy <file.rego>`) lands in a follow-on cycle.

use std::collections::HashSet;
use std::sync::Arc;

use agent_tui_protocol::action::{Action, ActionDetail, ActionKind, CallerInfo, Decision, Verdict};
use agent_tui_protocol::{AuditEvent, PaneId};
use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::broadcast;
use uuid::Uuid;

const AUDIT_CHANNEL_CAPACITY: usize = 1024;

/// Decision-maker trait. Implementations are interchangeable; the daemon
/// holds an `Arc<dyn Evaluator>` selected at startup.
#[async_trait]
pub trait Evaluator: Send + Sync {
    /// Score this action. Returning `Verdict::Deny` blocks the handler
    /// (translated to `POLICY_DENIED`); `RequireConfirm` blocks similarly
    /// until a future `policy confirm <id>` command (P3 follow-on).
    async fn evaluate(&self, action: &Action) -> Decision;
}

/// Permissive baseline: always Allow.
pub struct AllowAllEvaluator;

#[async_trait]
impl Evaluator for AllowAllEvaluator {
    async fn evaluate(&self, _action: &Action) -> Decision {
        Decision {
            audit_id: Uuid::new_v4(),
            verdict: Verdict::Allow,
            reason: "AllowAllEvaluator: no restrictions".into(),
        }
    }
}

/// Binary allowlist enforcement on `Spawn`; pass-through for other action
/// kinds. Empty allowlist means "everything allowed" (effective baseline
/// for development); the explicit `*` wildcard means "anything but I want
/// the audit log to know about it".
pub struct AllowlistEvaluator {
    binaries: HashSet<String>,
    wildcard: bool,
}

impl AllowlistEvaluator {
    /// Construct from a `Vec<String>`. The single token `*` enables
    /// wildcard mode (audit-logs every spawn but doesn't reject any).
    #[must_use]
    pub fn new(binaries: Vec<String>) -> Self {
        let wildcard = binaries.iter().any(|s| s == "*");
        let binaries = binaries
            .into_iter()
            .filter(|s| s != "*")
            .map(|s| basename(&s).to_string())
            .collect();
        Self { binaries, wildcard }
    }

    /// Parse a CSV string into an `AllowlistEvaluator`.
    #[must_use]
    pub fn from_csv(csv: &str) -> Self {
        Self::new(
            csv.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
        )
    }
}

#[async_trait]
impl Evaluator for AllowlistEvaluator {
    async fn evaluate(&self, action: &Action) -> Decision {
        let audit_id = Uuid::new_v4();
        if let ActionDetail::Spawn { argv, .. } = &action.detail {
            let comm = argv
                .first()
                .map(|s| basename(s).to_string())
                .unwrap_or_default();
            if self.wildcard {
                return Decision {
                    audit_id,
                    verdict: Verdict::Allow,
                    reason: format!("wildcard allowlist; spawn={comm}"),
                };
            }
            if self.binaries.is_empty() {
                return Decision {
                    audit_id,
                    verdict: Verdict::Allow,
                    reason: "empty allowlist (development mode)".into(),
                };
            }
            if self.binaries.contains(&comm) {
                return Decision {
                    audit_id,
                    verdict: Verdict::Allow,
                    reason: format!("allowlisted: {comm}"),
                };
            }
            return Decision {
                audit_id,
                verdict: Verdict::Deny,
                reason: format!(
                    "binary {comm} not in allowlist; permitted: {}",
                    self.binaries.iter().cloned().collect::<Vec<_>>().join(", ")
                ),
            };
        }
        Decision {
            audit_id,
            verdict: Verdict::Allow,
            reason: "non-spawn action; allowlist pass-through".into(),
        }
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Per-daemon governance state.
///
/// Wraps the active `Evaluator` and owns the audit broadcast channel that
/// every Decision is published to (Allow + Deny + `RequireConfirm` — the
/// audit firehose is interested in all three).
#[derive(Clone)]
pub struct Governance {
    evaluator: Arc<dyn Evaluator>,
    audit_tx: broadcast::Sender<AuditEvent>,
}

impl Governance {
    /// Build a Governance with the given evaluator.
    #[must_use]
    pub fn new(evaluator: Arc<dyn Evaluator>) -> Self {
        let (audit_tx, _) = broadcast::channel(AUDIT_CHANNEL_CAPACITY);
        Self {
            evaluator,
            audit_tx,
        }
    }

    /// Build a permissive Governance (every action allowed).
    #[must_use]
    pub fn allow_all() -> Self {
        Self::new(Arc::new(AllowAllEvaluator))
    }

    /// Subscribe to the `AuditEvent` firehose.
    pub fn subscribe(&self) -> broadcast::Receiver<AuditEvent> {
        self.audit_tx.subscribe()
    }

    /// Evaluate an action, emit an `AuditEvent` regardless of verdict, and
    /// return the Decision. The caller is responsible for turning a
    /// non-Allow verdict into a `POLICY_DENIED` / `POLICY_PENDING` Response.
    pub async fn check(&self, action: Action) -> Decision {
        let decision = self.evaluator.evaluate(&action).await;
        let evt = AuditEvent {
            session: String::new(),
            pane: action.pane.as_ref().map(|p| p.0.clone()),
            action_kind: action.kind,
            verdict: decision.verdict,
            reason: decision.reason.clone(),
            at: Utc::now(),
            detail_json: serde_json::to_value(&action.detail).unwrap_or(serde_json::Value::Null),
        };
        // Best-effort: no subscribers is not an error.
        let _ = self.audit_tx.send(evt);
        decision
    }
}

/// Helpers for building common Actions from handler call sites.
pub mod build {
    use super::{Action, ActionDetail, ActionKind, CallerInfo, PaneId};

    /// Spawn action.
    #[must_use]
    pub fn spawn(argv: Vec<String>, cwd: String) -> Action {
        Action {
            kind: ActionKind::Spawn,
            pane: None,
            detail: ActionDetail::Spawn { argv, cwd },
            caller: CallerInfo::default(),
        }
    }

    /// Input action (press / type / `send_ansi`).
    #[must_use]
    pub fn input(pane: PaneId, bytes: &[u8], key_tokens: Option<Vec<String>>) -> Action {
        Action {
            kind: ActionKind::Input,
            pane: Some(pane),
            detail: ActionDetail::Input {
                bytes_hex: hex::encode(bytes),
                key_tokens,
            },
            caller: CallerInfo::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn allow_all_evaluates_to_allow() {
        let g = Governance::allow_all();
        let d = g
            .check(build::spawn(vec!["/bin/bash".into()], "/".into()))
            .await;
        assert_eq!(d.verdict, Verdict::Allow);
    }

    #[tokio::test]
    async fn allowlist_denies_unknown_binary() {
        let g = Governance::new(Arc::new(AllowlistEvaluator::new(vec![
            "bash".into(),
            "zsh".into(),
        ])));
        let d = g
            .check(build::spawn(vec!["/usr/bin/nethack".into()], "/".into()))
            .await;
        assert_eq!(d.verdict, Verdict::Deny);
    }

    #[tokio::test]
    async fn allowlist_allows_known_binary_by_basename() {
        let g = Governance::new(Arc::new(AllowlistEvaluator::new(vec!["bash".into()])));
        let d = g
            .check(build::spawn(
                vec!["/bin/bash".into(), "-i".into()],
                "/".into(),
            ))
            .await;
        assert_eq!(d.verdict, Verdict::Allow);
    }

    #[tokio::test]
    async fn allowlist_wildcard_allows_anything() {
        let g = Governance::new(Arc::new(AllowlistEvaluator::new(vec!["*".into()])));
        let d = g
            .check(build::spawn(vec!["/something/weird".into()], "/".into()))
            .await;
        assert_eq!(d.verdict, Verdict::Allow);
    }

    #[tokio::test]
    async fn empty_allowlist_is_permissive() {
        let g = Governance::new(Arc::new(AllowlistEvaluator::new(vec![])));
        let d = g
            .check(build::spawn(vec!["/bin/foo".into()], "/".into()))
            .await;
        assert_eq!(d.verdict, Verdict::Allow);
    }

    #[tokio::test]
    async fn audit_event_emitted_on_check() {
        let g = Governance::new(Arc::new(AllowlistEvaluator::new(vec!["bash".into()])));
        let mut sub = g.subscribe();
        let _ = g
            .check(build::spawn(vec!["/bin/bash".into()], "/".into()))
            .await;
        let evt = sub.recv().await.expect("event");
        assert_eq!(evt.action_kind, ActionKind::Spawn);
        assert_eq!(evt.verdict, Verdict::Allow);
    }
}
