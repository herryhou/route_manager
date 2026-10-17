//! The machine envelope: the one JSON object every run prints to stdout.
//!
//! Shape (fields in serialization order; `data` and `logs` are omitted when
//! empty/None):
//! `{"status":"ok"|"rejected"|"noop","command":"<name>","data":{...},"logs":[...],"duration_ms":N}`

use serde::Serialize;

/// One run's machine-readable result — stdout carries exactly this object.
#[derive(Debug, Serialize)]
pub struct Envelope {
    /// `"ok"`, `"rejected"` or `"noop"`.
    pub status: &'static str,
    /// The command that ran, e.g. `"add-stop"`.
    pub command: &'static str,
    /// Command-specific payload; rejection and no-op reasons live in
    /// `data.reason`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// Human-readable progress lines.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub logs: Vec<String>,
    /// Wall-clock run time in milliseconds.
    pub duration_ms: u64,
}

/// A success envelope; the paired exit code is 0.
pub fn ok(command: &'static str, data: serde_json::Value) -> Envelope {
    Envelope {
        status: "ok",
        command,
        data: Some(data),
        logs: Vec::new(),
        duration_ms: 0,
    }
}

/// A rejection envelope; the paired exit code is 1.
pub fn rejected(command: &'static str, reason: &str) -> (Envelope, i32) {
    (
        Envelope {
            status: "rejected",
            command,
            data: Some(serde_json::json!({ "reason": reason })),
            logs: Vec::new(),
            duration_ms: 0,
        },
        1,
    )
}

/// A no-op envelope; the paired exit code is 2.
pub fn noop(command: &'static str, reason: &str) -> (Envelope, i32) {
    (
        Envelope {
            status: "noop",
            command,
            data: Some(serde_json::json!({ "reason": reason })),
            logs: Vec::new(),
            duration_ms: 0,
        },
        2,
    )
}
