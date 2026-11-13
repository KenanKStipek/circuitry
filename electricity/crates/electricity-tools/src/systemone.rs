//! `plugins/systemone.py::SystemOnePlugin`, ported (issue #724): a single
//! `POST <base_url>/v1/systemone` request, TypeSafe's wire format, over
//! the shared HTTP client ([`crate::http_client`]) -- the request body,
//! the loopback/`allow_remote` host rule, the question/answer validation
//! and the `raw` shape are all byte-for-byte with `plugins/systemone.py`.
//!
//! A document with no file of its own cannot name `state_file`/`within`
//! at all; a literal `within` must be an absolute path or `{from:
//! input.<name>}`; a limited document's `within` must stay inside its
//! own project, both at compile time
//! (`electricity_compiler::compile::leaves::check_systemone_confinement`)
//! for a literal value and here, at dispatch, for every read regardless
//! of where `within` came from -- `read_state_file`'s own *limited*/
//! *confine_to* arguments, carried from `ToolOp.limited`/`.confine_to`
//! through `ToolCall`, exactly as `SystemOnePlugin.limited`/`.confine_to`
//! do in cof. A profile cannot switch a tool to or from `systemone`
//! (`electricity_compiler::overrides`), and `params_json` cannot set
//! `within` (`electricity_vm::exec::tool::reject_overlay_within`).
//!
//! Not ported: `plugins/systemone.py::static_param_errors`'s own
//! `model`/`questions` checks (`cof check`'s up-front validation of a
//! literal `model`/`questions`, so `cof run` fails a malformed document
//! before it starts) -- `execute()` here re-checks both before the
//! request the same way cof's own `execute()` does regardless of
//! whether `cof check` already passed, so a document this skips an
//! up-front refusal for still fails the same way, just at the effect's
//! own attempt instead of before the run starts (DESIGN.md §13, a
//! documented divergence).

use crate::fs::{as_bool, get, value_error};
use crate::http_client::{self, ConnectTarget, RawRequest, Scheme, SendError, TimeoutModel};
use crate::{CheckResult, ErrorCause, PyExc, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_value::pycompat::py_strip;
use electricity_value::{Dict, IntValue, Value};
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

const ENDPOINT_PATH: &str = "/v1/systemone";
const HOST_SETTING_NAMES: [&str; 3] = ["base_url", "api_key_env", "allow_remote"];
const QUESTION_TYPES: [&str; 3] = ["noul", "choice", "score"];
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
const MAX_NESTING_LEVELS: usize = 64;
const MAX_CHOICE_OPTIONS: usize = 255;
const MIN_SCORE_LEVELS: usize = 2;
const MAX_SCORE_LEVELS: usize = 10;
const PROBABILITY_SUM_TOLERANCE: f64 = 0.01;

fn runtime_error(message: impl Into<String>) -> ToolError {
    ToolError {
        py_class: PyExc::RuntimeError,
        message: message.into(),
        cause: ErrorCause::None,
    }
}

fn no_reply_error(message: impl Into<String>) -> ToolError {
    ToolError {
        py_class: PyExc::RuntimeError,
        message: message.into(),
        cause: ErrorCause::NoReply,
    }
}

fn is_json_text(value: &Value) -> bool {
    matches!(value, Value::Str(_) | Value::Dict(_) | Value::List(_))
}

/// `plugins/systemone.py::_number`: a finite, non-`bool` `int`/`float` as
/// `f64`; anything else (including an overflowing big int) is `None`.
fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Int(i) => {
            let f = i.to_f64();
            f.is_finite().then_some(f)
        }
        Value::Float(f) if f.is_finite() => Some(*f),
        _ => None,
    }
}

// ---------------------------------------------------------------------
// `is_loopback_host`
// ---------------------------------------------------------------------

/// `plugins/systemone.py::is_loopback_host`: the literal hosts
/// `localhost`, `127.0.0.0/8` and `[::1]` -- never a DNS lookup.
/// *host* has already been lower-cased by the caller.
fn is_loopback_host(host: &str) -> bool {
    if host == "localhost" {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => v4.octets()[0] == 127,
        Ok(std::net::IpAddr::V6(v6)) => v6 == std::net::Ipv6Addr::LOCALHOST,
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------
// Host settings: `base_url`/`api_key_env`/`allow_remote`.
// ---------------------------------------------------------------------

#[derive(Debug)]
struct Server {
    scheme: String,
    host: String,
    port: u16,
    /// `base_url.strip().rstrip("/")`'s own `urlsplit().path` -- a prefix
    /// the request target is sent under (`server + ENDPOINT_PATH`, cof's
    /// own plain string concatenation): e.g. `/api` for a `base_url` of
    /// `http://127.0.0.1:8080/api`, empty when `base_url` names no path.
    path: String,
    /// `urlsplit().netloc`, exactly as written in `base_url` -- the `Host`
    /// header and the proxy bypass/target lookup both key off this, never
    /// off *host*/*port* once parsed apart (`crate::http`'s own
    /// `netloc_host_as_written`/`resolve_proxy_target`, issue #486):
    /// brackets kept for an IPv6 literal, an explicit default port kept.
    netloc: String,
    /// `base_url.strip().rstrip("/")` -- the text every error message
    /// below names, never re-derived from the parsed parts.
    display: String,
}

struct ParsedNetloc {
    host: String,
    port: Option<u16>,
    has_userinfo: bool,
    bad_port: bool,
}

fn parse_netloc(netloc: &str) -> ParsedNetloc {
    let (has_userinfo, hostport) = match netloc.rfind('@') {
        Some(idx) => (true, &netloc[idx + 1..]),
        None => (false, netloc),
    };
    let parse_port = |text: &str| -> (Option<u16>, bool) {
        if text.is_empty() {
            return (None, false);
        }
        if !text.chars().all(|c| c.is_ascii_digit()) {
            return (None, true);
        }
        match text.parse::<u16>() {
            Ok(port) => (Some(port), false),
            Err(_) => (None, true),
        }
    };
    if let Some(rest) = hostport.strip_prefix('[') {
        return match rest.split_once(']') {
            Some((addr, after)) => {
                let port_text = after.strip_prefix(':').unwrap_or("");
                let (port, bad_port) = parse_port(port_text);
                ParsedNetloc {
                    host: addr.to_string(),
                    port,
                    has_userinfo,
                    bad_port,
                }
            }
            None => ParsedNetloc {
                host: hostport.to_string(),
                port: None,
                has_userinfo,
                bad_port: true,
            },
        };
    }
    match hostport.rsplit_once(':') {
        Some((host, port_text)) => {
            let (port, bad_port) = parse_port(port_text);
            ParsedNetloc {
                host: host.to_string(),
                port,
                has_userinfo,
                bad_port,
            }
        }
        None => ParsedNetloc {
            host: hostport.to_string(),
            port: None,
            has_userinfo,
            bad_port: false,
        },
    }
}

fn default_port(scheme: &str) -> u16 {
    if scheme == "https" { 443 } else { 80 }
}

/// `SystemOnePlugin._server`. *base_url* is the raw config `Value` --
/// `isinstance(base_url, str)` is false for anything but a string, cof's
/// own `_server` has no separate "wrong type" message, so a non-string
/// value hits the same "required" text a missing one does.
fn validate_base_url(base_url: Option<&Value>, allow_remote: bool) -> Result<Server, ToolError> {
    let raw = match base_url {
        Some(Value::Str(s)) if !s.trim().is_empty() => s.as_str(),
        _ => {
            return Err(value_error(
                "systemone: runtime.plugins.systemone.base_url is required \
                 (set it in config.json, e.g. http://127.0.0.1:8080)",
            ));
        }
    };
    let display = raw.trim().trim_end_matches('/').to_string();
    let split = crate::http::http_url::urlsplit(&display, "");
    let netloc = parse_netloc(&split.netloc);
    if netloc.bad_port {
        return Err(value_error(
            "systemone: runtime.plugins.systemone.base_url is not a valid URL",
        ));
    }
    if !matches!(split.scheme.as_str(), "http" | "https") || netloc.host.is_empty() {
        return Err(value_error(
            "systemone: runtime.plugins.systemone.base_url must be an http:// or https:// URL",
        ));
    }
    if netloc.has_userinfo {
        return Err(value_error(
            "systemone: runtime.plugins.systemone.base_url must not carry credentials; \
             name the key's environment variable in api_key_env instead",
        ));
    }
    if !split.query.is_empty() || !split.fragment.is_empty() {
        return Err(value_error(
            "systemone: runtime.plugins.systemone.base_url must not have a query or fragment",
        ));
    }
    let hostname = netloc.host.to_lowercase();
    if !allow_remote && !is_loopback_host(&hostname) {
        return Err(value_error(format!(
            "systemone: base_url host {} is not a loopback host; set \
             runtime.plugins.systemone.allow_remote to true to use a remote server",
            Value::Str(hostname).py_repr()
        )));
    }
    let port = netloc.port.unwrap_or_else(|| default_port(&split.scheme));
    Ok(Server {
        scheme: split.scheme,
        host: netloc.host,
        port,
        path: split.path,
        netloc: split.netloc,
        display,
    })
}

fn is_env_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn ascii_printable(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii() && !c.is_ascii_control())
}

/// `SystemOnePlugin._api_key`. *api_key_env* is the raw config `Value` --
/// a non-string, non-empty value raises the same "must be the NAME of
/// an environment variable" text cof's own `isinstance` check does.
fn resolve_api_key(api_key_env: Option<&Value>) -> Result<Option<String>, ToolError> {
    let name = match api_key_env {
        None => return Ok(None),
        Some(Value::Str(s)) if s.is_empty() => return Ok(None),
        Some(Value::Str(s)) => s.as_str(),
        Some(_) => {
            return Err(value_error(
                "systemone: runtime.plugins.systemone.api_key_env must be the NAME of an \
                 environment variable, not the key itself",
            ));
        }
    };
    if !is_env_name(name) {
        return Err(value_error(
            "systemone: runtime.plugins.systemone.api_key_env must be the NAME of an \
             environment variable, not the key itself",
        ));
    }
    let key = std::env::var(name).unwrap_or_default();
    let key = py_strip(&key).to_string();
    if key.is_empty() {
        return Ok(None);
    }
    if !ascii_printable(&key) {
        return Err(value_error(format!(
            "systemone: the key in ${name} has characters that cannot be sent in a header"
        )));
    }
    Ok(Some(key))
}

// ---------------------------------------------------------------------
// Question validation and normalization.
// ---------------------------------------------------------------------

/// A question/criteria/option id or key, once written the way the
/// request body would write it as a JSON object key -- `Invalid` carries
/// the *original* key, for an error message, when it either can't be
/// written as a string at all or collides with another key once it is
/// (`plugins/systemone.py::_RepeatedKey`).
enum NKey {
    Str(String),
    Invalid(Value),
}

/// `plugins/systemone.py::json_key`.
fn json_key(key: &Value) -> Value {
    match key {
        Value::Str(_) => key.clone(),
        Value::Bool(true) => Value::from("true"),
        Value::Bool(false) => Value::from("false"),
        Value::None => Value::from("null"),
        Value::Int(_) => Value::Str(key.py_str()),
        Value::Float(f) if f.is_finite() => Value::Str(key.py_str()),
        other => other.clone(),
    }
}

/// `plugins/systemone.py::_json_keys`: *dict*'s keys, each written as
/// [`json_key`] would, with a later collision reported instead of
/// silently overwriting the earlier entry.
fn normalize_dict_keys(dict: &Dict) -> Vec<(NKey, Value)> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(dict.len());
    for (key, value) in dict {
        let written = json_key(key);
        let nkey = match &written {
            Value::Str(s) => {
                if seen.insert(s.clone()) {
                    NKey::Str(s.clone())
                } else {
                    NKey::Invalid(key.clone())
                }
            }
            _ => NKey::Invalid(key.clone()),
        };
        out.push((nkey, value.clone()));
    }
    out
}

/// `plugins/systemone.py::_criteria_errors`, plus the key normalization
/// `normalize_questions` applies to a criteria *dict* before this ever
/// runs (`choice`/`noul`; `score`'s own criteria is a list, never
/// key-normalized). `Ok(Some(..))` carries the normalized criteria to
/// write back onto the question; `Ok(None)` means nothing needs
/// rewriting (absent, or a `score` list).
fn normalize_and_check_criteria(
    where_: &str,
    qtype: &str,
    criteria: Option<&Value>,
) -> Result<Option<Value>, Vec<String>> {
    match qtype {
        "noul" => match criteria {
            None | Some(Value::None) => Ok(None),
            Some(Value::Dict(d)) => {
                let entries = normalize_dict_keys(d);
                let keys_ok = entries
                    .iter()
                    .all(|(k, _)| matches!(k, NKey::Str(s) if s == "true" || s == "false"));
                if !keys_ok {
                    return Err(vec![format!(
                        "{where_}: criteria for a noul question may only hold 'true' and 'false'"
                    )]);
                }
                if !entries.iter().all(|(_, v)| is_json_text(v)) {
                    return Err(vec![format!(
                        "{where_}: noul criteria 'true' and 'false' must be a string, object or array"
                    )]);
                }
                let mut out = Dict::new();
                for (k, v) in entries {
                    if let NKey::Str(s) = k {
                        out.insert(Value::Str(s), v);
                    }
                }
                Ok(Some(Value::Dict(out)))
            }
            Some(_) => Err(vec![format!(
                "{where_}: criteria for a noul question may only hold 'true' and 'false'"
            )]),
        },
        "choice" => {
            let Some(Value::Dict(d)) = criteria else {
                return Err(vec![format!(
                    "{where_}: criteria for a choice question must be a map of option to description"
                )]);
            };
            let entries = normalize_dict_keys(d);
            if let Some((NKey::Invalid(orig), _)) =
                entries.iter().find(|(k, _)| matches!(k, NKey::Invalid(_)))
            {
                return Err(vec![format!(
                    "{where_}: option {} is not text, or repeats another option once written as JSON",
                    orig.py_repr()
                )]);
            }
            if entries.len() > MAX_CHOICE_OPTIONS {
                return Err(vec![format!(
                    "{where_}: {} options; a choice question takes at most {MAX_CHOICE_OPTIONS}",
                    entries.len()
                )]);
            }
            if !entries
                .iter()
                .all(|(_, v)| matches!(v, Value::None) || is_json_text(v))
            {
                return Err(vec![format!(
                    "{where_}: each option's description must be a string, object, array or null"
                )]);
            }
            let mut out = Dict::new();
            for (k, v) in entries {
                if let NKey::Str(s) = k {
                    out.insert(Value::Str(s), v);
                }
            }
            Ok(Some(Value::Dict(out)))
        }
        _ => {
            let Some(Value::List(items)) = criteria else {
                return Err(vec![format!(
                    "{where_}: criteria for a score question must be an array of \
                     {MIN_SCORE_LEVELS} to {MAX_SCORE_LEVELS} level descriptions"
                )]);
            };
            if items.len() < MIN_SCORE_LEVELS || items.len() > MAX_SCORE_LEVELS {
                return Err(vec![format!(
                    "{where_}: criteria for a score question must be an array of \
                     {MIN_SCORE_LEVELS} to {MAX_SCORE_LEVELS} level descriptions"
                )]);
            }
            if !items.iter().all(is_json_text) {
                return Err(vec![format!(
                    "{where_}: each score level description must be a string, object or array"
                )]);
            }
            Ok(None)
        }
    }
}

/// `plugins/systemone.py::_question_errors`, folded together with the
/// criteria-key half of `normalize_questions` (this port's one pass).
fn check_and_normalize_question(qid: &str, question: &Value) -> Result<Value, Vec<String>> {
    let where_ = format!("question {}", Value::Str(qid.to_string()).py_repr());
    let Value::Dict(qdict) = question else {
        return Err(vec![format!(
            "{where_}: must be a map with type, instructions and criteria"
        )]);
    };
    let mut errors = Vec::new();

    let qtype = qdict.get(&Value::from("type"));
    let typed = match qtype {
        Some(Value::Str(s)) if QUESTION_TYPES.contains(&s.as_str()) => Some(s.clone()),
        _ => {
            errors.push(format!(
                "{where_}: type must be one of {}, got {}",
                QUESTION_TYPES.join(", "),
                qtype.cloned().unwrap_or(Value::None).py_repr()
            ));
            None
        }
    };

    match qdict.get(&Value::from("instructions")) {
        None | Some(Value::None) => errors.push(format!("{where_}: instructions are required")),
        Some(v) if !is_json_text(v) => errors.push(format!(
            "{where_}: instructions must be a string, object or array"
        )),
        _ => {}
    }

    let mut normalized_question = qdict.clone();
    if let Some(typed) = &typed {
        let criteria = qdict.get(&Value::from("criteria"));
        match normalize_and_check_criteria(&where_, typed, criteria) {
            Ok(Some(normalized_criteria)) => {
                normalized_question.insert(Value::from("criteria"), normalized_criteria);
            }
            Ok(None) => {}
            Err(mut errs) => errors.append(&mut errs),
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(Value::Dict(normalized_question))
}

/// `plugins/systemone.py::normalize_questions` + `question_errors`,
/// folded into one pass (this port skips the `static` check `cof check`
/// alone needs -- module doc). `Ok` carries the question ids in their
/// original order and the normalized, string-keyed questions map the
/// request body sends; `Err` carries every problem found, `cof`'s own
/// `"; "`-joined order.
fn validate_questions(raw: &Value) -> Result<(Vec<String>, Dict), Vec<String>> {
    let Value::Dict(dict) = raw else {
        return Err(vec![
            "must be a map of question id to {type, instructions, criteria}".to_string(),
        ]);
    };
    let entries = normalize_dict_keys(dict);
    let mut errors = Vec::new();
    let mut ids = Vec::new();
    let mut built = Dict::new();
    for (key, question) in entries {
        match key {
            NKey::Invalid(orig) => {
                errors.push(format!(
                    "question {}: the id is not text, or repeats another id once written as JSON",
                    orig.py_repr()
                ));
            }
            NKey::Str(qid) => match check_and_normalize_question(&qid, &question) {
                Ok(normalized) => {
                    ids.push(qid.clone());
                    built.insert(Value::Str(qid), normalized);
                }
                Err(mut errs) => errors.append(&mut errs),
            },
        }
    }
    if errors.is_empty() {
        Ok((ids, built))
    } else {
        Err(errors)
    }
}

// ---------------------------------------------------------------------
// Answer validation.
// ---------------------------------------------------------------------

fn probability_errors(
    where_: &str,
    probabilities: Option<&Value>,
    known: &HashSet<String>,
    noun: &str,
) -> Vec<String> {
    let Some(Value::Dict(probs)) = probabilities else {
        return vec![format!("{where_}: probabilities are missing or not a map")];
    };
    let mut errors = Vec::new();
    let mut total = 0.0f64;
    for (key, raw) in probs {
        let known_key = key.as_str().is_some_and(|k| known.contains(k));
        if !known_key {
            errors.push(format!(
                "{where_}: probabilities name {}, which is not an {noun} of the question",
                key.py_repr()
            ));
        }
        match number(raw) {
            Some(v) if (0.0..=1.0).contains(&v) => total += v,
            _ => errors.push(format!(
                "{where_}: probability for {} is {}, not a number in [0, 1]",
                key.py_repr(),
                raw.py_repr()
            )),
        }
    }
    if errors.is_empty() && (total - 1.0).abs() > PROBABILITY_SUM_TOLERANCE {
        errors.push(format!(
            "{where_}: probabilities sum to {}, not 1 (\u{b1}0.01)",
            crate::agent_cli::py_format_g(total)
        ));
    }
    errors
}

fn single_answer_errors(qid: &str, question: &Dict, answer: &Value) -> Vec<String> {
    let where_ = format!("question {}", Value::Str(qid.to_string()).py_repr());
    let Some(answer_dict) = answer.as_dict() else {
        return vec![format!("{where_}: answer is not an object")];
    };
    let qtype = question
        .get(&Value::from("type"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let answer_type = answer_dict.get(&Value::from("type"));
    if answer_type.and_then(Value::as_str) != Some(qtype) {
        return vec![format!(
            "{where_}: answer type {} differs from the question's {}",
            answer_type.cloned().unwrap_or(Value::None).py_repr(),
            Value::Str(qtype.to_string()).py_repr()
        )];
    }

    let mut errors = Vec::new();
    match qtype {
        "noul" => {
            let noul = answer_dict.get(&Value::from("noul"));
            let value = noul.and_then(number);
            if !matches!(value, Some(v) if (0.0..=1.0).contains(&v)) {
                errors.push(format!(
                    "{where_}: noul {} is not a number in [0, 1]",
                    noul.cloned().unwrap_or(Value::None).py_repr()
                ));
            }
        }
        "choice" => {
            let option_names: HashSet<String> = question
                .get(&Value::from("criteria"))
                .and_then(Value::as_dict)
                .map(|o| {
                    o.keys()
                        .filter_map(Value::as_str)
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            let chosen = answer_dict.get(&Value::from("choice"));
            let valid_choice = chosen
                .and_then(Value::as_str)
                .is_some_and(|c| option_names.contains(c));
            if !valid_choice {
                errors.push(format!(
                    "{where_}: choice {} is not one of the question's options",
                    chosen.cloned().unwrap_or(Value::None).py_repr()
                ));
            }
            errors.extend(probability_errors(
                &where_,
                answer_dict.get(&Value::from("probabilities")),
                &option_names,
                "option",
            ));
        }
        _ => {
            let levels = question
                .get(&Value::from("criteria"))
                .and_then(Value::as_list)
                .map(<[Value]>::len)
                .unwrap_or(0);
            let score = answer_dict.get(&Value::from("score"));
            let score_value = score.and_then(number);
            let max_score = (levels as f64) - 1.0;
            if !matches!(score_value, Some(v) if (0.0..=max_score).contains(&v)) {
                errors.push(format!(
                    "{where_}: score {} is not a number in [0, {}]",
                    score.cloned().unwrap_or(Value::None).py_repr(),
                    levels.saturating_sub(1)
                ));
            }
            let valid_levels: HashSet<String> = (0..levels).map(|l| l.to_string()).collect();
            errors.extend(probability_errors(
                &where_,
                answer_dict.get(&Value::from("probabilities")),
                &valid_levels,
                "level",
            ));
        }
    }

    if let Some(confidence) = answer_dict.get(&Value::from("confidence")) {
        let value = number(confidence);
        if !matches!(value, Some(v) if (0.0..=1.0).contains(&v)) {
            errors.push(format!(
                "{where_}: confidence {} is not a number in [0, 1]",
                confidence.py_repr()
            ));
        }
    }
    errors
}

/// `plugins/systemone.py::answer_errors`. *questions* is the validated,
/// string-keyed map [`validate_questions`] built.
fn answer_errors(questions: &Dict, answers: &Value) -> Vec<String> {
    let Some(answers_dict) = answers.as_dict() else {
        return vec!["response 'answers' is not a map".to_string()];
    };
    let mut errors = Vec::new();
    let mut asked: HashSet<String> = HashSet::new();
    for (qid_value, question) in questions {
        let qid = qid_value
            .as_str()
            .expect("questions keys are always strings");
        asked.insert(qid.to_string());
        match answers_dict.get(&Value::from(qid)) {
            None => errors.push(format!(
                "question {}: no answer",
                Value::Str(qid.to_string()).py_repr()
            )),
            Some(answer) => errors.extend(single_answer_errors(
                qid,
                question.as_dict().expect("validated questions are maps"),
                answer,
            )),
        }
    }
    for answered in answers_dict.keys() {
        let answered_str = answered.as_str().unwrap_or_default();
        if !asked.contains(answered_str) {
            errors.push(format!(
                "answer {}: no such question was asked",
                answered.py_repr()
            ));
        }
    }
    errors
}

// ---------------------------------------------------------------------
// Request building.
// ---------------------------------------------------------------------

fn nesting_levels(value: &Value) -> usize {
    let mut deepest = 0usize;
    let mut stack: Vec<(&Value, usize)> = vec![(value, 1)];
    while let Some((item, level)) = stack.pop() {
        let children: Vec<&Value> = match item {
            Value::Dict(d) => d.values().collect(),
            Value::List(items) => items.iter().collect(),
            _ => continue,
        };
        deepest = deepest.max(level);
        if deepest > MAX_NESTING_LEVELS {
            break;
        }
        for child in children {
            stack.push((child, level + 1));
        }
    }
    deepest
}

fn non_finite_token(value: &Value) -> bool {
    match value {
        Value::Float(f) => !f.is_finite(),
        Value::List(items) => items.iter().any(non_finite_token),
        Value::Dict(d) => d
            .iter()
            .any(|(k, v)| non_finite_token(k) || non_finite_token(v)),
        _ => false,
    }
}

/// `plugins/systemone.py::_json_bytes`: `json.dumps(value,
/// ensure_ascii=False, allow_nan=False, separators=(",", ":"))`,
/// UTF-8 encoded.
fn json_bytes(value: &Value, what: &str) -> Result<Vec<u8>, ToolError> {
    if non_finite_token(value) {
        return Err(value_error(format!(
            "systemone: {what} cannot be sent as JSON: Out of range float values are not JSON compliant"
        )));
    }
    let mode = electricity_json::WriteMode {
        indent: None,
        sort_keys: false,
        ensure_ascii: false,
        separators: electricity_json::Separators::Compact,
    };
    electricity_json::dumps(value, mode)
        .map(String::into_bytes)
        .map_err(|err| value_error(format!("systemone: {what} cannot be sent as JSON: {err}")))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

enum StateFailure {
    TooLarge(String),
    Error(ToolError),
}

impl From<ToolError> for StateFailure {
    fn from(err: ToolError) -> Self {
        StateFailure::Error(err)
    }
}

fn strerror(err: &std::io::Error) -> String {
    let text = err.to_string();
    match text.rfind(" (os error ") {
        Some(idx) if text.ends_with(')') => text[..idx].to_string(),
        _ => text,
    }
}

fn reject_non_finite_json(text: &str, path_text: &str) -> Result<Value, StateFailure> {
    let value = electricity_json::loads(text).map_err(|err| {
        StateFailure::Error(value_error(format!(
            "systemone: params.state_file {} is not valid JSON: {err}",
            Value::Str(path_text.to_string()).py_repr()
        )))
    })?;
    fn first_token(value: &Value) -> Option<&'static str> {
        match value {
            Value::Float(f) if f.is_nan() => Some("NaN"),
            Value::Float(f) if *f == f64::INFINITY => Some("Infinity"),
            Value::Float(f) if *f == f64::NEG_INFINITY => Some("-Infinity"),
            Value::List(items) => items.iter().find_map(first_token),
            Value::Dict(d) => d
                .iter()
                .find_map(|(k, v)| first_token(k).or_else(|| first_token(v))),
            _ => None,
        }
    }
    if let Some(token) = first_token(&value) {
        return Err(StateFailure::Error(value_error(format!(
            "systemone: params.state_file {} is not valid JSON: {token} is not a finite number",
            Value::Str(path_text.to_string()).py_repr()
        ))));
    }
    Ok(value)
}

fn decode_utf8_sig(bytes: &[u8]) -> Option<String> {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8(bytes.to_vec()).ok()
}

/// `plugins/systemone.py::_read_state_file`.
fn read_state_file(
    path_text: &str,
    within_text: Option<&str>,
    limited: bool,
    confine_to: Option<&str>,
) -> Result<Value, StateFailure> {
    let within_text = match within_text {
        Some(s) if !s.trim().is_empty() => s,
        _ => {
            return Err(StateFailure::Error(value_error(
                "systemone: params.within is required with params.state_file",
            )));
        }
    };
    if !Path::new(within_text).is_absolute() || within_text.contains('\0') {
        return Err(StateFailure::Error(value_error(
            "systemone: params.within must be an absolute path",
        )));
    }
    if path_text.contains('\0') {
        return Err(StateFailure::Error(value_error(
            "systemone: params.state_file must not contain a null byte",
        )));
    }

    let within = Path::new(within_text)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(within_text).to_path_buf());
    if limited {
        let Some(confine_to) = confine_to else {
            return Err(StateFailure::Error(value_error(
                "systemone: this document has no project, so it cannot read a \
                 params.state_file",
            )));
        };
        let root = Path::new(confine_to)
            .canonicalize()
            .unwrap_or_else(|_| Path::new(confine_to).to_path_buf());
        if within != root && !within.starts_with(&root) {
            return Err(StateFailure::Error(value_error(format!(
                "systemone: params.within: {OUTSIDE_PROJECT}"
            ))));
        }
    }
    let resolved = match Path::new(path_text).canonicalize() {
        Ok(p) => p,
        Err(err) => {
            return Err(StateFailure::Error(value_error(format!(
                "systemone: params.state_file {} cannot be read: {}",
                Value::Str(path_text.to_string()).py_repr(),
                strerror(&err)
            ))));
        }
    };
    if resolved != within && !resolved.starts_with(&within) {
        return Err(StateFailure::Error(value_error(format!(
            "systemone: params.state_file {} resolves outside params.within",
            Value::Str(path_text.to_string()).py_repr()
        ))));
    }
    let metadata = match resolved.metadata() {
        Ok(m) => m,
        Err(err) => {
            return Err(StateFailure::Error(value_error(format!(
                "systemone: params.state_file {} cannot be read: {}",
                Value::Str(path_text.to_string()).py_repr(),
                strerror(&err)
            ))));
        }
    };
    if !metadata.is_file() {
        return Err(StateFailure::Error(value_error(format!(
            "systemone: params.state_file {} is not a regular file",
            Value::Str(path_text.to_string()).py_repr()
        ))));
    }
    if metadata.len() > MAX_BODY_BYTES as u64 {
        return Err(StateFailure::TooLarge(format!(
            "systemone: params.state_file {} is over the {MAX_BODY_BYTES}-byte request limit",
            Value::Str(path_text.to_string()).py_repr()
        )));
    }
    let bytes = match std::fs::read(&resolved) {
        Ok(b) => b,
        Err(err) => {
            return Err(StateFailure::Error(value_error(format!(
                "systemone: params.state_file {} cannot be read: {}",
                Value::Str(path_text.to_string()).py_repr(),
                strerror(&err)
            ))));
        }
    };
    let text = match decode_utf8_sig(&bytes) {
        Some(t) => t,
        None => {
            return Err(StateFailure::Error(value_error(format!(
                "systemone: params.state_file {} is not valid UTF-8",
                Value::Str(path_text.to_string()).py_repr()
            ))));
        }
    };

    let is_json = resolved
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("json"));
    if !is_json {
        return Ok(Value::Str(text));
    }
    reject_non_finite_json(&text, path_text)
}

/// `plugins/systemone.py::_OUTSIDE_PROJECT`.
const OUTSIDE_PROJECT: &str = "must lie inside the document's own project (a document the \
     caller did not name by path is confined to it)";

/// `SystemOnePlugin._state`.
fn resolve_state(
    params: &Value,
    limited: bool,
    confine_to: Option<&str>,
) -> Result<Value, StateFailure> {
    let has_state = get(params, "state").is_some();
    let has_file = get(params, "state_file").is_some();
    if has_state == has_file {
        return Err(StateFailure::Error(value_error(
            "systemone: give exactly one of params.state and params.state_file",
        )));
    }
    if has_state {
        if get(params, "within").is_some() {
            return Err(StateFailure::Error(value_error(
                "systemone: params.within only applies to params.state_file",
            )));
        }
        return Ok(get(params, "state").cloned().unwrap_or(Value::None));
    }
    let path_text = match get(params, "state_file") {
        Some(Value::Str(s)) if !s.trim().is_empty() => s.clone(),
        _ => {
            return Err(StateFailure::Error(value_error(
                "systemone: params.state_file must be a non-empty path string",
            )));
        }
    };
    let within_text = get(params, "within").and_then(Value::as_str);
    read_state_file(&path_text, within_text, limited, confine_to)
}

// ---------------------------------------------------------------------
// Response handling.
// ---------------------------------------------------------------------

/// `plugins/systemone.py::_error_kind`.
fn error_kind(status: u16, body_text: &str) -> &'static str {
    if (300..400).contains(&status) {
        return "redirect";
    }
    if status == 400 {
        return if body_text.contains("max_tokens_exceeded") {
            "context_exceeded"
        } else {
            "invalid"
        };
    }
    match status {
        401 => "unauthorized",
        403 => "forbidden",
        404 => "not_found",
        408 => "timeout",
        413 => "too_large",
        415 => "unsupported_media_type",
        422 => "invalid",
        429 => "rate_limited",
        529 => "overloaded",
        _ if (500..600).contains(&status) => "server_error",
        _ => "http_error",
    }
}

fn scrub(text: &str, key: Option<&str>) -> String {
    match key {
        Some(k) if !k.is_empty() => text.replace(k, "[redacted]"),
        _ => text.to_string(),
    }
}

/// `plugins/systemone.py::_post::scrubbed` -- every string in *value*,
/// keys included, with the key replaced by `[redacted]`.
fn scrubbed(value: Value, key: Option<&str>) -> Value {
    match &value {
        Value::Str(s) => Value::Str(scrub(s, key)),
        Value::Dict(d) => Value::Dict(
            d.iter()
                .map(|(k, v)| (scrubbed(k.clone(), key), scrubbed(v.clone(), key)))
                .collect(),
        ),
        Value::List(items) => Value::List(items.iter().map(|v| scrubbed(v.clone(), key)).collect()),
        _ => value,
    }
}

fn truncate_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn usage_tokens(usage: Option<&Value>, field: &str) -> Option<i64> {
    let dict = usage.and_then(Value::as_dict)?;
    match dict.get(&Value::from(field)) {
        Some(Value::Int(IntValue::Small(n))) if *n >= 0 => Some(*n),
        _ => None,
    }
}

fn failed_result(raw: Dict, kind: &str, message: String, key: Option<&str>) -> ToolResult {
    let mut raw = raw;
    raw.insert(Value::from("error_kind"), Value::Str(kind.to_string()));
    ToolResult {
        value: Value::None,
        raw: Value::Dict(raw),
        stdout: None,
        stderr: Some(scrub(&message, key)),
        exit_code: None,
        ok: false,
        stdout_parsed: false,
        tokens_sent: None,
        tokens_received: None,
        cost_usd: None,
    }
}

// ---------------------------------------------------------------------
// The plugin.
// ---------------------------------------------------------------------

pub struct SystemOnePlugin {
    base_url: Option<Value>,
    api_key_env: Option<Value>,
    allow_remote: bool,
}

#[async_trait(?Send)]
impl ToolPlugin for SystemOnePlugin {
    fn name(&self) -> &str {
        "systemone"
    }

    async fn execute(&self, params: Value, call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        for name in HOST_SETTING_NAMES {
            if get(&params, name).is_some() {
                return Err(value_error(format!(
                    "systemone: params.{name} is a host setting; set it under \
                     runtime.plugins.systemone in config.json"
                )));
            }
        }
        let server = validate_base_url(self.base_url.as_ref(), self.allow_remote)?;
        let key = resolve_api_key(self.api_key_env.as_ref())?;

        let model = match get(&params, "model") {
            Some(Value::Str(s)) if !py_strip(s).is_empty() => s.clone(),
            _ => {
                return Err(value_error(
                    "systemone: params.model is required as a non-empty string",
                ));
            }
        };
        let raw_questions = get(&params, "questions").cloned().unwrap_or(Value::None);
        let (question_ids, questions) = validate_questions(&raw_questions)
            .map_err(|problems| value_error(format!("systemone: {}", problems.join("; "))))?;

        let mut raw = Dict::new();
        raw.insert(Value::from("model"), Value::Str(model.clone()));
        raw.insert(
            Value::from("question_ids"),
            Value::List(question_ids.iter().cloned().map(Value::Str).collect()),
        );

        let state = match resolve_state(&params, call.limited, call.confine_to) {
            Ok(state) => state,
            Err(StateFailure::TooLarge(message)) => {
                return Ok(failed_result(raw, "too_large", message, None));
            }
            Err(StateFailure::Error(err)) => return Err(err),
        };
        let state_json = json_bytes(&state, "the state")?;
        raw.insert(
            Value::from("state_sha256"),
            Value::Str(sha256_hex(&state_json)),
        );
        raw.insert(
            Value::from("state_bytes"),
            Value::from(state_json.len() as i64),
        );

        let mut body = Dict::new();
        body.insert(Value::from("model"), Value::Str(model.clone()));
        body.insert(Value::from("state"), state);
        body.insert(Value::from("questions"), Value::Dict(questions.clone()));
        let body = Value::Dict(body);

        if nesting_levels(&body) > MAX_NESTING_LEVELS {
            return Err(value_error(format!(
                "systemone: the request nests more than {MAX_NESTING_LEVELS} levels deep"
            )));
        }
        let payload = json_bytes(&body, "the request")?;
        if payload.len() > MAX_BODY_BYTES {
            let message = format!(
                "systemone: the request is {} bytes, over the {MAX_BODY_BYTES}-byte limit",
                payload.len()
            );
            return Ok(failed_result(raw, "too_large", message, None));
        }

        self.post(&server, key.as_deref(), payload, raw, &questions, call)
            .await
    }

    fn check(&self) -> CheckResult {
        // `SystemOnePlugin.check`: "not configured" only for an absent or
        // empty-string `base_url` -- a present non-string value (e.g. a
        // number) falls through to `validate_base_url`, which reports it
        // as `ok: false` the same way a missing one would.
        let not_configured = match &self.base_url {
            None => true,
            Some(Value::Str(s)) => s.trim().is_empty(),
            Some(_) => false,
        };
        if not_configured {
            return CheckResult {
                ok: true,
                missing: Vec::new(),
                message: Some("not configured (runtime.plugins.systemone.base_url)".to_string()),
            };
        }
        match validate_base_url(self.base_url.as_ref(), self.allow_remote)
            .and_then(|_| resolve_api_key(self.api_key_env.as_ref()))
        {
            Ok(_) => CheckResult {
                ok: true,
                missing: Vec::new(),
                message: None,
            },
            Err(err) => CheckResult {
                ok: false,
                missing: Vec::new(),
                message: Some(err.message),
            },
        }
    }
}

impl SystemOnePlugin {
    async fn post(
        &self,
        server: &Server,
        key: Option<&str>,
        payload: Vec<u8>,
        mut raw: Dict,
        questions: &Dict,
        call: &ToolCall<'_>,
    ) -> Result<ToolResult, ToolError> {
        let scheme_enum = if server.scheme == "https" {
            Scheme::Https
        } else {
            Scheme::Http
        };
        let loopback = is_loopback_host(&server.host.to_lowercase());
        let proxy = if loopback {
            None
        } else {
            crate::http::resolve_proxy_target(&server.scheme, &server.netloc)
        };

        let mut headers: Vec<(String, String)> = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Accept".to_string(), "application/json".to_string()),
        ];
        if let Some(k) = key {
            headers.push(("Authorization".to_string(), format!("Bearer {k}")));
        }
        headers.push(("Content-Length".to_string(), payload.len().to_string()));
        headers.push((
            "Host".to_string(),
            crate::http::netloc_host_as_written(&server.netloc).to_string(),
        ));
        headers.push(("Accept-Encoding".to_string(), "identity".to_string()));
        // A plain `http://` request through a proxy has no `CONNECT`
        // tunnel at all, so (unlike the tunneled `https://` case, where
        // `http_client::send` puts this on the `CONNECT` line only)
        // `Proxy-authorization` belongs in the regular header set here --
        // same as `crate::http`'s own request builder (issue #486).
        if scheme_enum == Scheme::Http {
            if let Some(creds) = proxy.as_ref().and_then(|p| p.basic_auth.as_deref()) {
                headers.push(("Proxy-authorization".to_string(), format!("Basic {creds}")));
            }
        }
        headers.push(("Connection".to_string(), "close".to_string()));
        headers.push((
            "User-Agent".to_string(),
            format!("electricity/{}", env!("CARGO_PKG_VERSION")),
        ));

        let connect_target = ConnectTarget {
            scheme: scheme_enum,
            host: server.host.clone(),
            port: server.port,
            proxy: proxy.clone(),
        };
        let target_path = format!("{}{ENDPOINT_PATH}", server.path);
        // Through an `http://` proxy the wire target stays absolute-form
        // (`scheme://netloc/path`), the request this *is* the one sent to
        // the proxy -- same as `crate::http`'s own request builder.
        let wire_target = match &proxy {
            Some(_) if scheme_enum == Scheme::Http => format!(
                "{}://{}{target_path}",
                server.scheme,
                crate::http::netloc_host_as_written(&server.netloc)
            ),
            _ => target_path,
        };
        let raw_request = RawRequest {
            method: "POST".to_string(),
            target: wire_target,
            headers,
            body: Some(payload),
        };
        let client = http_client::build_client().map_err(runtime_error)?;
        let timeout_seconds = call.timeout_seconds.max(1);
        let timeout = TimeoutModel::PerOperation(Duration::from_secs(u64::from(timeout_seconds)));

        // `response.read(_MAX_RESPONSE_BYTES + 1)` -- bounded the same
        // way the request's own state is: a response bigger than the
        // request limit is not a response worth reading in full.
        let send_result = http_client::send_capped(
            &client,
            &connect_target,
            &raw_request,
            timeout,
            MAX_BODY_BYTES + 1,
        )
        .await;
        if call.token.is_set() {
            return Err(ToolError::message("cancelled"));
        }
        let response = match send_result {
            Err(SendError::Timeout) => {
                return Err(no_reply_error(format!(
                    "systemone request to {} timed out after {timeout_seconds}s",
                    server.display
                )));
            }
            Err(SendError::RemoteDisconnected) | Err(SendError::IncompleteRead { .. }) => {
                return Err(no_reply_error(format!(
                    "systemone server at {} dropped the connection",
                    server.display
                )));
            }
            Err(SendError::Connect { .. }) | Err(SendError::Other(_)) => {
                return Err(no_reply_error(format!(
                    "systemone server not reachable at {}",
                    server.display
                )));
            }
            // Only `send_to_sink` (save_to) produces these; mapped rather
            // than unreachable, so a future change there can never panic here.
            Err(SendError::BodyOverCap { .. }) | Err(SendError::Sink(_)) => {
                return Err(no_reply_error(format!(
                    "systemone server at {} dropped the connection",
                    server.display
                )));
            }
            Ok(response) => response,
        };

        let status = response.status;
        raw.insert(Value::from("status"), Value::from(i64::from(status)));
        let retry_after = response
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
            .map(|(_, value)| value.clone());
        if let Some(retry_after) = &retry_after {
            let mut headers_dict = Dict::new();
            headers_dict.insert(
                Value::from("retry-after"),
                Value::Str(scrub(&truncate_chars(retry_after, 100), key)),
            );
            raw.insert(Value::from("headers"), Value::Dict(headers_dict));
        }
        // `with urlopen(...) as resp: ... resp.read()` -- a 2xx has no
        // `except HTTPError` handler of its own to swallow a body-read
        // failure into, unlike every other status below.
        if (200..300).contains(&status) {
            match &response.body_error {
                Some(SendError::Timeout) => {
                    return Err(no_reply_error(format!(
                        "systemone request to {} timed out after {timeout_seconds}s",
                        server.display
                    )));
                }
                Some(SendError::RemoteDisconnected) | Some(SendError::Other(_)) => {
                    return Err(no_reply_error(format!(
                        "systemone server at {} dropped the connection",
                        server.display
                    )));
                }
                Some(SendError::Connect { .. }) => {
                    return Err(no_reply_error(format!(
                        "systemone server not reachable at {}",
                        server.display
                    )));
                }
                // `response.read(amt)` -- a `Content-Length` short read
                // returns the bytes it did get rather than raising; the
                // short body almost always then fails to parse as JSON.
                Some(SendError::IncompleteRead { .. }) | None => {}
                // Only `send_to_sink` (save_to) produces these; treated like a
                // short read rather than unreachable.
                Some(SendError::BodyOverCap { .. }) | Some(SendError::Sink(_)) => {}
            }
        }
        let text = if matches!(response.body_error, Some(SendError::IncompleteRead { .. })) {
            String::from_utf8_lossy(&response.partial_body).into_owned()
        } else {
            String::from_utf8_lossy(&response.body).into_owned()
        };

        if !(200..300).contains(&status) {
            let kind = error_kind(status, &text);
            let message = if (300..400).contains(&status) {
                format!(
                    "systemone server at {} answered HTTP {status}; a redirect is never followed",
                    server.display
                )
            } else {
                let mut message = format!("systemone HTTP {status} ({kind})");
                let excerpt = crate::http::http_error_excerpt(&text, 500);
                if !excerpt.is_empty() {
                    message = format!("{message}: {excerpt}");
                }
                message
            };
            return Ok(failed_result(raw, kind, message, key));
        }

        Ok(self.build_answers(raw, questions, &text, key))
    }

    fn build_answers(
        &self,
        mut raw: Dict,
        questions: &Dict,
        text: &str,
        key: Option<&str>,
    ) -> ToolResult {
        let reply = match electricity_json::loads(text) {
            Ok(v) => v,
            Err(_) => {
                return failed_result(
                    raw,
                    "invalid_response",
                    "systemone response is not valid JSON".to_string(),
                    key,
                );
            }
        };
        let Some(reply_dict) = reply.as_dict() else {
            return failed_result(
                raw,
                "invalid_response",
                "systemone response has no 'answers'".to_string(),
                key,
            );
        };
        let Some(answers) = reply_dict.get(&Value::from("answers")).cloned() else {
            return failed_result(
                raw,
                "invalid_response",
                "systemone response has no 'answers'".to_string(),
                key,
            );
        };

        if let Some(Value::Str(model_name)) = reply_dict.get(&Value::from("model")) {
            raw.insert(Value::from("model"), Value::Str(scrub(model_name, key)));
        }
        let usage = reply_dict.get(&Value::from("usage")).cloned();
        match &usage {
            Some(Value::Dict(_)) => {
                raw.insert(
                    Value::from("usage"),
                    scrubbed(usage.clone().expect("checked Some above"), key),
                );
            }
            _ => {
                raw.insert(Value::from("usage"), Value::None);
            }
        }

        let problems = answer_errors(questions, &answers);
        if !problems.is_empty() {
            let message = format!(
                "systemone answers do not match the questions: {}",
                problems.join("; ")
            );
            return failed_result(raw, "invalid_answer", message, key);
        }

        let tokens_sent = usage_tokens(usage.as_ref(), "input_tokens");
        let tokens_received = usage_tokens(usage.as_ref(), "output_tokens");
        ToolResult {
            value: scrubbed(answers, key),
            raw: Value::Dict(raw),
            stdout: None,
            stderr: None,
            exit_code: None,
            ok: true,
            stdout_parsed: false,
            tokens_sent,
            tokens_received,
            cost_usd: None,
        }
    }
}

/// `plugins/systemone.py::build`.
pub fn make_plugin(cfg: &Value) -> Result<Box<dyn ToolPlugin>, ToolError> {
    // The raw config `Value`s, not just a `str` projection -- a non-string
    // `base_url`/`api_key_env` is a distinct, reachable misconfiguration
    // `validate_base_url`/`resolve_api_key` report on, same as cof's own
    // `cfg.get(...)` (an explicit `null` and an absent key are both
    // Python's `None`, so filtered out the same way here).
    let base_url = get(cfg, "base_url")
        .filter(|v| !matches!(v, Value::None))
        .cloned();
    let api_key_env = get(cfg, "api_key_env")
        .filter(|v| !matches!(v, Value::None))
        .cloned();
    let allow_remote = as_bool(get(cfg, "allow_remote"), false);
    Ok(Box::new(SystemOnePlugin {
        base_url,
        api_key_env,
        allow_remote,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::CancellationToken;
    use std::io::{Read, Write};

    fn call<'a>(token: &'a CancellationToken, config: &'a Value) -> ToolCall<'a> {
        ToolCall {
            timeout_seconds: 5,
            config,
            token,
            armed: false,
            secret_headers: &[],
            scrub: None,
            on_live: None,
            on_agent_live: None,
            agent_attempt: 1,
            limited: false,
            confine_to: None,
        }
    }

    fn params(pairs: Vec<(&str, Value)>) -> Value {
        let mut dict = Dict::new();
        for (k, v) in pairs {
            dict.insert(Value::from(k), v);
        }
        Value::Dict(dict)
    }

    #[test]
    fn is_loopback_host_recognizes_localhost_and_loopback_addresses() {
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("127.5.6.7"));
        assert!(is_loopback_host("::1"));
        assert!(!is_loopback_host("example.com"));
        assert!(!is_loopback_host("10.0.0.1"));
    }

    #[test]
    fn validate_base_url_requires_a_base_url() {
        let err = validate_base_url(None, false).unwrap_err();
        assert_eq!(err.py_class, PyExc::ValueError);
        assert!(err.message.contains("base_url is required"));
    }

    #[test]
    fn validate_base_url_refuses_a_remote_host_without_allow_remote() {
        let err = validate_base_url(Some(&Value::from("http://example.com")), false).unwrap_err();
        assert!(err.message.contains("is not a loopback host"));
    }

    #[test]
    fn validate_base_url_accepts_a_remote_host_with_allow_remote() {
        let server =
            validate_base_url(Some(&Value::from("https://example.com:8443/")), true).unwrap();
        assert_eq!(server.host, "example.com");
        assert_eq!(server.port, 8443);
        assert_eq!(server.display, "https://example.com:8443");
    }

    #[test]
    fn validate_base_url_keeps_a_path_prefix() {
        let server =
            validate_base_url(Some(&Value::from("http://127.0.0.1:8080/api")), false).unwrap();
        assert_eq!(server.path, "/api");
        assert_eq!(server.display, "http://127.0.0.1:8080/api");
    }

    #[test]
    fn validate_base_url_refuses_a_non_string_value() {
        let err = validate_base_url(Some(&Value::from(123)), false).unwrap_err();
        assert!(err.message.contains("base_url is required"));
    }

    #[test]
    fn resolve_api_key_refuses_a_non_string_value() {
        let err = resolve_api_key(Some(&Value::from(123))).unwrap_err();
        assert!(
            err.message
                .contains("must be the NAME of an environment variable")
        );
    }

    #[test]
    fn probability_errors_matches_cofs_g_format_for_an_inexact_sum() {
        let mut probabilities = Dict::new();
        probabilities.insert(Value::from("a"), Value::from(0.6));
        probabilities.insert(Value::from("b"), Value::from(0.3));
        let known: HashSet<String> = ["a".to_string(), "b".to_string()].into_iter().collect();
        let errors = probability_errors(
            "question 'q'",
            Some(&Value::Dict(probabilities)),
            &known,
            "option",
        );
        assert_eq!(
            errors,
            vec!["question 'q': probabilities sum to 0.9, not 1 (\u{b1}0.01)".to_string()]
        );
    }

    #[test]
    fn validate_base_url_refuses_credentials() {
        let err = validate_base_url(Some(&Value::from("http://user:pass@127.0.0.1:8080")), false)
            .unwrap_err();
        assert!(err.message.contains("must not carry credentials"));
    }

    #[test]
    fn validate_base_url_refuses_a_query_or_fragment() {
        let err =
            validate_base_url(Some(&Value::from("http://127.0.0.1:8080/?x=1")), false).unwrap_err();
        assert!(err.message.contains("must not have a query or fragment"));
    }

    #[test]
    fn validate_questions_reports_an_unknown_type() {
        let mut q = Dict::new();
        q.insert(
            Value::from("a"),
            Value::Dict({
                let mut d = Dict::new();
                d.insert(Value::from("type"), Value::from("nope"));
                d.insert(Value::from("instructions"), Value::from("x"));
                d
            }),
        );
        let err = validate_questions(&Value::Dict(q)).unwrap_err();
        assert_eq!(
            err,
            vec!["question 'a': type must be one of noul, choice, score, got 'nope'".to_string()]
        );
    }

    #[test]
    fn validate_questions_normalizes_yaml_boolean_criteria_keys() {
        let mut criteria = Dict::new();
        criteria.insert(Value::Bool(true), Value::from("yes"));
        criteria.insert(Value::Bool(false), Value::from("no"));
        let mut question = Dict::new();
        question.insert(Value::from("type"), Value::from("noul"));
        question.insert(Value::from("instructions"), Value::from("x"));
        question.insert(Value::from("criteria"), Value::Dict(criteria));
        let mut q = Dict::new();
        q.insert(Value::from("a"), Value::Dict(question));
        let (ids, built) = validate_questions(&Value::Dict(q)).unwrap();
        assert_eq!(ids, vec!["a".to_string()]);
        let normalized_criteria = built
            .get(&Value::from("a"))
            .unwrap()
            .as_dict()
            .unwrap()
            .get(&Value::from("criteria"))
            .unwrap()
            .as_dict()
            .unwrap();
        assert_eq!(
            normalized_criteria.get(&Value::from("true")),
            Some(&Value::from("yes"))
        );
    }

    #[test]
    fn answer_errors_reports_a_missing_answer() {
        let mut question = Dict::new();
        question.insert(Value::from("type"), Value::from("noul"));
        let mut questions = Dict::new();
        questions.insert(Value::from("a"), Value::Dict(question));
        let errors = answer_errors(&questions, &Value::Dict(Dict::new()));
        assert_eq!(errors, vec!["question 'a': no answer".to_string()]);
    }

    #[tokio::test]
    async fn execute_requires_base_url_configured() {
        let plugin = SystemOnePlugin {
            base_url: None,
            api_key_env: None,
            allow_remote: false,
        };
        let token = CancellationToken::new();
        let config = Value::None;
        let err = plugin
            .execute(params(vec![]), &call(&token, &config))
            .await
            .unwrap_err();
        assert!(err.message.contains("base_url is required"));
    }

    #[tokio::test]
    async fn execute_refuses_a_host_setting_as_a_param() {
        let plugin = SystemOnePlugin {
            base_url: Some(Value::from("http://127.0.0.1:8080")),
            api_key_env: None,
            allow_remote: false,
        };
        let token = CancellationToken::new();
        let config = Value::None;
        let err = plugin
            .execute(
                params(vec![("base_url", Value::from("http://evil"))]),
                &call(&token, &config),
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("params.base_url is a host setting"));
    }

    #[tokio::test]
    async fn execute_requires_exactly_one_of_state_and_state_file() {
        let plugin = SystemOnePlugin {
            base_url: Some(Value::from("http://127.0.0.1:8080")),
            api_key_env: None,
            allow_remote: false,
        };
        let token = CancellationToken::new();
        let config = Value::None;
        let mut q = Dict::new();
        q.insert(
            Value::from("a"),
            Value::Dict({
                let mut d = Dict::new();
                d.insert(Value::from("type"), Value::from("noul"));
                d.insert(Value::from("instructions"), Value::from("x"));
                d
            }),
        );
        let err = plugin
            .execute(
                params(vec![
                    ("model", Value::from("m")),
                    ("questions", Value::Dict(q)),
                ]),
                &call(&token, &config),
            )
            .await
            .unwrap_err();
        assert!(
            err.message
                .contains("give exactly one of params.state and params.state_file")
        );
    }

    #[test]
    fn check_reports_not_configured_when_base_url_is_absent() {
        let plugin = SystemOnePlugin {
            base_url: None,
            api_key_env: None,
            allow_remote: false,
        };
        let result = plugin.check();
        assert!(result.ok);
        assert_eq!(
            result.message,
            Some("not configured (runtime.plugins.systemone.base_url)".to_string())
        );
    }

    #[tokio::test]
    async fn execute_returns_ok_false_too_large_when_the_request_payload_is_over_the_limit() {
        let plugin = SystemOnePlugin {
            base_url: Some(Value::from("http://127.0.0.1:8080")),
            api_key_env: None,
            allow_remote: false,
        };
        let token = CancellationToken::new();
        let config = Value::None;
        let mut q = Dict::new();
        q.insert(
            Value::from("a"),
            Value::Dict({
                let mut d = Dict::new();
                d.insert(Value::from("type"), Value::from("noul"));
                d.insert(Value::from("instructions"), Value::from("x"));
                d
            }),
        );
        // One character short of MAX_BODY_BYTES on its own would still fit
        // once wrapped in the request envelope -- comfortably over it is
        // simpler than tuning the exact boundary.
        let huge_state = Value::from("x".repeat(MAX_BODY_BYTES + 1024));
        let result = plugin
            .execute(
                params(vec![
                    ("model", Value::from("m")),
                    ("questions", Value::Dict(q)),
                    ("state", huge_state),
                ]),
                &call(&token, &config),
            )
            .await
            .unwrap();
        assert!(!result.ok);
        let raw = result.raw.as_dict().unwrap();
        assert_eq!(
            raw.get(&Value::from("error_kind")),
            Some(&Value::from("too_large"))
        );
        assert!(result.stderr.unwrap().contains("over the"));
    }

    #[test]
    fn check_reports_the_remote_refusal_when_configured_without_allow_remote() {
        let plugin = SystemOnePlugin {
            base_url: Some(Value::from("http://example.com")),
            api_key_env: None,
            allow_remote: false,
        };
        let result = plugin.check();
        assert!(!result.ok);
        assert!(result.message.unwrap().contains("is not a loopback host"));
    }

    /// A scratch directory under the process's own temp dir, never the
    /// real `$HOME` -- removed on drop.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new() -> Self {
            static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "electricity-systemone-test-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn read_state_file_refuses_a_limited_document_with_no_project() {
        let dir = TempDir::new();
        let state_path = dir.0.join("state.json");
        std::fs::write(&state_path, "{}").unwrap();
        let err = read_state_file(
            state_path.to_str().unwrap(),
            Some(dir.0.to_str().unwrap()),
            true,
            None,
        )
        .unwrap_err();
        let StateFailure::Error(err) = err else {
            panic!("expected a plain error, not a too-large failure");
        };
        assert!(
            err.message
                .contains("this document has no project, so it cannot read a params.state_file")
        );
    }

    #[test]
    fn read_state_file_refuses_a_limited_documents_within_outside_its_project() {
        let project = TempDir::new();
        let outside = TempDir::new();
        let state_path = outside.0.join("state.json");
        std::fs::write(&state_path, "{}").unwrap();
        let err = read_state_file(
            state_path.to_str().unwrap(),
            Some(outside.0.to_str().unwrap()),
            true,
            Some(project.0.to_str().unwrap()),
        )
        .unwrap_err();
        let StateFailure::Error(err) = err else {
            panic!("expected a plain error, not a too-large failure");
        };
        assert!(
            err.message
                .contains("params.within: must lie inside the document's own project")
        );
    }

    #[test]
    fn read_state_file_allows_a_limited_documents_within_inside_its_project() {
        let project = TempDir::new();
        let state_path = project.0.join("state.json");
        std::fs::write(&state_path, "{\"a\":1}").unwrap();
        let result = read_state_file(
            state_path.to_str().unwrap(),
            Some(project.0.to_str().unwrap()),
            true,
            Some(project.0.to_str().unwrap()),
        );
        let value = match result {
            Ok(value) => value,
            Err(_) => panic!("expected a within inside the project to be allowed"),
        };
        assert_eq!(
            value.as_dict().unwrap().get(&Value::from("a")),
            Some(&Value::from(1))
        );
    }

    fn one_question() -> Dict {
        let mut q = Dict::new();
        q.insert(
            Value::from("a"),
            Value::Dict({
                let mut d = Dict::new();
                d.insert(Value::from("type"), Value::from("noul"));
                d.insert(Value::from("instructions"), Value::from("x"));
                d
            }),
        );
        q
    }

    fn record_request(
        response: &'static [u8],
    ) -> (std::net::SocketAddr, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap();
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
            let _ = stream.write_all(response);
        });
        (addr, rx)
    }

    #[tokio::test]
    async fn execute_posts_to_the_path_prefix_in_base_url() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 58\r\nConnection: close\r\n\r\n{\"answers\":{\"a\":{\"type\":\"noul\",\"noul\":0.1}}}";
        let (addr, rx) = record_request(response);
        let plugin = SystemOnePlugin {
            base_url: Some(Value::from(format!("http://{addr}/api"))),
            api_key_env: None,
            allow_remote: false,
        };
        let token = CancellationToken::new();
        let config = Value::None;
        let result = plugin
            .execute(
                params(vec![
                    ("model", Value::from("m")),
                    ("questions", Value::Dict(one_question())),
                    ("state", Value::from("s")),
                ]),
                &call(&token, &config),
            )
            .await
            .unwrap();
        assert!(result.ok, "{:?}", result.stderr);
        let request = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(
            request.starts_with("POST /api/v1/systemone "),
            "request line: {request:?}"
        );
    }

    #[tokio::test]
    async fn execute_sends_a_bearer_header_and_scrubs_the_key_from_a_failure_message() {
        let key = "s1-test-key-for-electricity-systemone-scrubbing";
        unsafe {
            std::env::set_var("ELECTRICITY_SYSTEMONE_TEST_KEY", key);
        }
        let body = format!("the key {key} is not valid");
        let response = format!(
            "HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let response_bytes: &'static [u8] = Box::leak(response.into_bytes().into_boxed_slice());
        let (addr, rx) = record_request(response_bytes);
        let plugin = SystemOnePlugin {
            base_url: Some(Value::from(format!("http://{addr}"))),
            api_key_env: Some(Value::from("ELECTRICITY_SYSTEMONE_TEST_KEY")),
            allow_remote: false,
        };
        let token = CancellationToken::new();
        let config = Value::None;
        let result = plugin
            .execute(
                params(vec![
                    ("model", Value::from("m")),
                    ("questions", Value::Dict(one_question())),
                    ("state", Value::from("s")),
                ]),
                &call(&token, &config),
            )
            .await
            .unwrap();
        assert!(!result.ok);
        let message = result.stderr.unwrap();
        assert!(!message.contains(key), "leaked the key: {message}");
        assert!(message.contains("[redacted]"), "{message}");
        let request = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(
            request.contains(&format!("Authorization: Bearer {key}")),
            "request: {request}"
        );
    }

    #[tokio::test]
    async fn execute_treats_a_short_2xx_body_as_invalid_response_not_a_raised_failure() {
        // `Content-Length` promises more than the server actually sends,
        // then closes -- `SendError::IncompleteRead` on a 2xx, which
        // `http.client`'s own `read(amt)` would return as a short body
        // rather than raise (module doc).
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\nnot enough bytes here";
        let (addr, _rx) = record_request(response);
        let plugin = SystemOnePlugin {
            base_url: Some(Value::from(format!("http://{addr}"))),
            api_key_env: None,
            allow_remote: false,
        };
        let token = CancellationToken::new();
        let config = Value::None;
        let result = plugin
            .execute(
                params(vec![
                    ("model", Value::from("m")),
                    ("questions", Value::Dict(one_question())),
                    ("state", Value::from("s")),
                ]),
                &call(&token, &config),
            )
            .await
            .unwrap();
        assert!(!result.ok);
        let raw = result.raw.as_dict().unwrap();
        assert_eq!(
            raw.get(&Value::from("error_kind")),
            Some(&Value::from("invalid_response"))
        );
    }
}
