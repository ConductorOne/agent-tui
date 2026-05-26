//! Ground-truth event-log recorder.
//!
//! Writes asciicast-v3-extended NDJSON per pane. Custom event kinds
//! beyond the stock spec:
//!
//! | kind | payload                                | purpose                               |
//! |------|----------------------------------------|---------------------------------------|
//! | `o`  | `"<bytes>"`                            | Output (stock asciicast)              |
//! | `i`  | `"<bytes>"`                            | Input (stock asciicast)               |
//! | `r`  | `{"cols": N, "rows": M}`               | Resize (stock asciicast)              |
//! | `g`  | `{"seq", "gen", "cells_b64"}`          | Grid snapshot at quiescence boundary  |
//! | `m`  | `{"kind", "command", "ok", "err"?}`    | Tool-call boundary                    |
//! | `s`  | `{"seq", "hash"}`                      | Sequence checkpoint (every N events)  |
//! | `p`  | `{"name"}`                             | User-defined marker                   |
//!
//! See `docs/RFC.md` §10.

#![forbid(unsafe_code)]

pub mod writer;
pub use writer::{
    CHANNEL_CAPACITY, DEFAULT_RETENTION_BYTES, DEFAULT_ROTATE_BYTES, Recorder, RecorderConfig,
    RecorderStats,
};

use serde::{Deserialize, Serialize};

/// One event in the recorder's NDJSON log.
///
/// Wire form is the three-tuple `[time, kind, payload]` per asciicast v3.
/// `payload` is a string for `o`/`i`, a JSON object for `r`/`g`/`m`/`s`/`p`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Seconds since session start, fractional.
    pub time: f64,
    /// Single-letter event kind.
    pub kind: char,
    /// Payload — string for `o`/`i`, JSON for the others.
    pub payload: serde_json::Value,
}

/// Stock asciicast `o` event — output bytes.
#[must_use]
pub fn output_event(time: f64, bytes: &[u8]) -> Event {
    Event {
        time,
        kind: 'o',
        payload: serde_json::Value::String(String::from_utf8_lossy(bytes).into_owned()),
    }
}

/// Stock asciicast `i` event — input bytes. For `auth use`, callers pass
/// `None` so the recorder writes a `null` placeholder (per RFC §11.5).
#[must_use]
pub fn input_event(time: f64, bytes: Option<&[u8]>) -> Event {
    let payload = match bytes {
        Some(b) => serde_json::Value::String(String::from_utf8_lossy(b).into_owned()),
        None => serde_json::Value::Null,
    };
    Event {
        time,
        kind: 'i',
        payload,
    }
}

/// Stock asciicast `r` event — resize.
#[must_use]
pub fn resize_event(time: f64, cols: u16, rows: u16) -> Event {
    Event {
        time,
        kind: 'r',
        payload: serde_json::json!({ "cols": cols, "rows": rows }),
    }
}

/// Custom `g` event — grid snapshot at quiescence.
#[must_use]
pub fn grid_event(time: f64, seq: u64, generation: u64, cells_b64: &str) -> Event {
    Event {
        time,
        kind: 'g',
        payload: serde_json::json!({
            "seq": seq,
            "gen": generation,
            "cells_b64": cells_b64,
        }),
    }
}

/// Custom `m` event — tool-call boundary.
#[must_use]
pub fn marker_event(time: f64, kind: &str, command: &str, ok: bool, err: Option<&str>) -> Event {
    let mut payload = serde_json::json!({
        "kind": kind,
        "command": command,
        "ok": ok,
    });
    if let Some(err) = err {
        payload["err"] = serde_json::Value::String(err.to_string());
    }
    Event {
        time,
        kind: 'm',
        payload,
    }
}

/// Custom `s` event — sequence checkpoint.
#[must_use]
pub fn checkpoint_event(time: f64, seq: u64, hash: &str) -> Event {
    Event {
        time,
        kind: 's',
        payload: serde_json::json!({ "seq": seq, "hash": hash }),
    }
}

/// Custom `p` event — user-defined marker from `--mark`.
#[must_use]
pub fn pin_event(time: f64, name: &str) -> Event {
    Event {
        time,
        kind: 'p',
        payload: serde_json::json!({ "name": name }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_event_round_trips() {
        let e = output_event(1.5, b"hello");
        let s = serde_json::to_string(&e).expect("encode ok");
        let parsed: Event = serde_json::from_str(&s).expect("decode ok");
        assert_eq!(parsed.kind, 'o');
        assert!((parsed.time - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn input_null_round_trips() {
        let e = input_event(1.0, None);
        let s = serde_json::to_string(&e).expect("encode ok");
        let parsed: Event = serde_json::from_str(&s).expect("decode ok");
        assert!(parsed.payload.is_null());
    }
}
