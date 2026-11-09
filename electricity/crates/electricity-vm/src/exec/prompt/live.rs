//! A streamed `prompt` attempt's own running `meta.live` figures
//! (`core/prompt.py::_LiveAttemptState`, ported) -- updated from
//! whichever point in the single-threaded executor actually drives a
//! streaming adapter's reader (there is no second OS thread here the
//! way Python's own adapter call has, so no lock is needed at all).

use std::time::Instant;

use electricity_value::{Dict, Value};

use crate::adapter::GenerateProgress;

fn key(name: &str) -> Value {
    Value::Str(name.to_string())
}

fn now_iso_string() -> String {
    use chrono::SecondsFormat;
    chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Micros, false)
}

/// `core/complexity.py::_estimate_tokens` -- ceiling division by 4
/// characters, `0` for an empty string. Kept as its own tiny copy
/// here rather than a shared dependency on the complexity crate: this
/// is the one figure `meta.live.tokens_sent_estimate` needs, and nothing
/// else in this module touches complexity scoring at all.
pub(crate) fn estimate_tokens(text: &str) -> i64 {
    let len = text.chars().count() as i64;
    if len == 0 {
        0
    } else {
        (len + 3) / 4
    }
}

/// `core/prompt.py::_LiveAttemptState`.
pub(crate) struct LiveAttemptState {
    started_at: String,
    first_token_at: Option<String>,
    first_token_monotonic: Option<Instant>,
    tokens_sent: Option<i64>,
    tokens_received: Option<i64>,
    tokens_received_estimate: i64,
    chars_received: i64,
}

impl LiveAttemptState {
    pub(crate) fn new() -> Self {
        LiveAttemptState {
            started_at: now_iso_string(),
            first_token_at: None,
            first_token_monotonic: None,
            tokens_sent: None,
            tokens_received: None,
            tokens_received_estimate: 0,
            chars_received: 0,
        }
    }

    fn tokens_per_second(&self) -> Option<f64> {
        let first = self.first_token_monotonic?;
        if self.tokens_received_estimate < 2 {
            return None;
        }
        let elapsed = first.elapsed().as_secs_f64();
        if elapsed < 0.5 {
            return None;
        }
        Some(self.tokens_received_estimate as f64 / elapsed)
    }

    pub(crate) fn tokens_received_estimate(&self) -> i64 {
        self.tokens_received_estimate
    }

    pub(crate) fn chars_received(&self) -> i64 {
        self.chars_received
    }

    pub(crate) fn tokens_sent(&self) -> Option<i64> {
        self.tokens_sent
    }

    pub(crate) fn apply(&mut self, progress: &GenerateProgress) {
        if progress.delta_chars > 0 {
            self.chars_received += progress.delta_chars as i64;
            self.tokens_received_estimate += i64::from(progress.delta_count);
            if self.first_token_at.is_none() {
                self.first_token_at = Some(now_iso_string());
                self.first_token_monotonic = Some(Instant::now());
            }
        }
        if let Some(sent) = progress.tokens_sent {
            self.tokens_sent = Some(sent);
        }
        if let Some(received) = progress.tokens_received {
            self.tokens_received = Some(received);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn snapshot(
        &self,
        attempt: u32,
        adapter: &str,
        model: &str,
        streaming: bool,
        tokens_sent_estimate: i64,
    ) -> Value {
        let mut dict = Dict::new();
        dict.insert(key("v"), Value::from(1i64));
        dict.insert(key("attempt"), Value::from(i64::from(attempt)));
        dict.insert(key("adapter"), Value::Str(adapter.to_string()));
        dict.insert(key("model"), Value::Str(model.to_string()));
        dict.insert(key("started_at"), Value::Str(self.started_at.clone()));
        dict.insert(key("updated_at"), Value::Str(now_iso_string()));
        dict.insert(key("streaming"), Value::Bool(streaming));
        dict.insert(
            key("first_token_at"),
            self.first_token_at.clone().map(Value::Str).unwrap_or(Value::None),
        );
        dict.insert(
            key("tokens_sent"),
            self.tokens_sent.map(Value::from).unwrap_or(Value::None),
        );
        dict.insert(key("tokens_sent_estimate"), Value::from(tokens_sent_estimate));
        dict.insert(
            key("tokens_received"),
            self.tokens_received.map(Value::from).unwrap_or(Value::None),
        );
        dict.insert(
            key("tokens_received_estimate"),
            Value::from(self.tokens_received_estimate),
        );
        dict.insert(key("chars_received"), Value::from(self.chars_received));
        dict.insert(
            key("tokens_per_second"),
            self.tokens_per_second().map(Value::Float).unwrap_or(Value::None),
        );
        Value::Dict(dict)
    }
}
