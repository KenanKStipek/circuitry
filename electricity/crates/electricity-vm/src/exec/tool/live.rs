//! A process-backed `tool` effect's own running `meta.live` (#741 part
//! B) -- the same shape the agent tool (#636) and streamed prompts
//! (#674, `exec/prompt/live.rs`) already publish, fed by
//! [`electricity_tools::process::run_tracked`]'s own tail/byte-count
//! callback (`electricity_tools::ToolCall::on_live`) instead of an
//! adapter's streaming progress.

use electricity_tools::process::LiveTail;
use electricity_value::{Dict, Value};

fn key(name: &str) -> Value {
    Value::Str(name.to_string())
}

fn now_iso_string() -> String {
    use chrono::SecondsFormat;
    chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Micros, false)
}

/// The five providers `meta.live` is published for (#741 part B's own
/// E2) -- every process-backed provider that runs through
/// `electricity_tools::process::run_tracked`, except the agent CLIs
/// (which get the agent tool's own #636 live summary instead) and
/// `http` (which has no running subprocess output to publish at all).
/// *provider* must already be normalized (`py_strip`'d and
/// lowercased) the way [`super::resolve_plugin`]'s own caller already
/// normalizes it before this check.
pub(crate) fn publishes_live(provider: &str) -> bool {
    matches!(provider, "shell" | "awk" | "imagemagick" | "ffmpeg" | "gh")
}

/// One dispatch attempt's own `meta.live`, fixed at `started_at` and
/// refreshed at every [`LiveToolState::snapshot`] call -- `exec/
/// prompt/live.rs::LiveAttemptState`'s own shape, minus the figures
/// that are specific to a model call.
pub(crate) struct LiveToolState {
    started_at: String,
}

impl LiveToolState {
    pub(crate) fn new() -> Self {
        LiveToolState {
            started_at: now_iso_string(),
        }
    }

    /// `meta.live`'s own key order: `v`, `started_at`, `updated_at`,
    /// `stdout_tail`, `stderr_tail`, `stdout_bytes`, `stderr_bytes`.
    pub(crate) fn snapshot(&self, tail: LiveTail) -> Value {
        let mut dict = Dict::new();
        dict.insert(key("v"), Value::from(1i64));
        dict.insert(key("started_at"), Value::Str(self.started_at.clone()));
        dict.insert(key("updated_at"), Value::Str(now_iso_string()));
        dict.insert(key("stdout_tail"), Value::Str(tail.stdout_tail));
        dict.insert(key("stderr_tail"), Value::Str(tail.stderr_tail));
        dict.insert(key("stdout_bytes"), Value::from(tail.stdout_bytes as i64));
        dict.insert(key("stderr_bytes"), Value::from(tail.stderr_bytes as i64));
        Value::Dict(dict)
    }

    /// This attempt's own first `meta.live`, published right before
    /// dispatch -- empty tails, zero bytes -- the same moment `exec/
    /// prompt.rs` publishes a streamed prompt's own first snapshot,
    /// before its adapter call has produced anything either.
    pub(crate) fn empty_snapshot(&self) -> Value {
        self.snapshot(LiveTail {
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            stdout_bytes: 0,
            stderr_bytes: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishes_live_matches_exactly_the_five_process_backed_providers() {
        for provider in ["shell", "awk", "imagemagick", "ffmpeg", "gh"] {
            assert!(publishes_live(provider), "{provider} should publish live");
        }
        for provider in ["json", "http", "cel", "agent", "service", "systemone"] {
            assert!(
                !publishes_live(provider),
                "{provider} should not publish live"
            );
        }
    }

    #[test]
    fn snapshot_key_order_matches_the_streamed_prompts_own_shape() {
        let state = LiveToolState::new();
        let snapshot = state.snapshot(LiveTail {
            stdout_tail: "out".to_string(),
            stderr_tail: "err".to_string(),
            stdout_bytes: 3,
            stderr_bytes: 3,
        });
        let encoded =
            electricity_json::dumps(&snapshot, electricity_json::WriteMode::COMPACT).unwrap();
        let parsed = electricity_json::loads(&encoded).unwrap();
        let Value::Dict(ref dict) = parsed else {
            panic!("snapshot is always a dict");
        };
        let keys: Vec<String> = dict
            .keys()
            .map(|k| match k {
                Value::Str(s) => s.clone(),
                other => panic!("unexpected key {other:?}"),
            })
            .collect();
        assert_eq!(
            keys,
            vec![
                "v",
                "started_at",
                "updated_at",
                "stdout_tail",
                "stderr_tail",
                "stdout_bytes",
                "stderr_bytes",
            ]
        );
    }
}
