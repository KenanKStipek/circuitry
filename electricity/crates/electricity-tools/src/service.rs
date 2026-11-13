//! `plugins/service.py::ServicePlugin`, ported (issue #721): starts a
//! background process (a dev server, a local API) that outlives the
//! call, waits until it is ready, and later stops exactly what it
//! started -- never anything else on the machine. `action` is `start`,
//! `stop` or `status`; `name` is unique per state directory.
//!
//! Ownership is provable before any signal: a record in the state
//! directory (`runtime.plugins.service.state_dir`, default
//! `~/.config/circuitry/services`), written under an `flock` on
//! `<state_dir>/.lock`, holds the process group id, the group leader's
//! start time and the machine's boot time -- the same record and lock
//! `cof run` uses, so a service one engine started, the other can stop.
//!
//! POSIX only (process groups, `flock`): the real logic lives in the
//! [`imp`] module, compiled only `#[cfg(unix)]`; elsewhere `execute`/
//! `check` report the same "requires a POSIX system" failure cof's own
//! `os.name != "posix"` guard does, without pulling any unix-only API
//! into a non-unix build.

use crate::{CheckResult, ErrorCause, PyExc, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_value::Value;

/// `plugins/service.py::ServicePlugin`.
pub struct ServicePlugin {
    /// `runtime.plugins.service.state_dir`; `None` means
    /// [`imp::default_state_dir`].
    pub state_dir: Option<String>,
}

/// `plugins/factory.py::_build_service`.
pub fn make_plugin(cfg: &Value) -> Result<Box<dyn ToolPlugin>, ToolError> {
    let state_dir = match cfg
        .as_dict()
        .and_then(|d| d.get(&Value::Str("state_dir".to_string())))
    {
        None | Some(Value::None) => None,
        // `state_dir or None` (`plugins/factory.py::_build_service`):
        // an empty string means the default directory too.
        Some(Value::Str(s)) if s.is_empty() => None,
        Some(Value::Str(s)) => Some(s.clone()),
        Some(_) => {
            return Err(ToolError {
                py_class: PyExc::ValueError,
                message: "runtime.plugins.service.state_dir must be a path string.".to_string(),
                cause: ErrorCause::None,
            });
        }
    };
    Ok(Box::new(ServicePlugin { state_dir }))
}

#[async_trait(?Send)]
impl ToolPlugin for ServicePlugin {
    fn name(&self) -> &str {
        "service"
    }

    async fn execute(&self, params: Value, call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        #[cfg(not(unix))]
        {
            let _ = (&params, call);
            return Err(posix_only_error());
        }
        #[cfg(unix)]
        {
            imp::execute(self, params, call).await
        }
    }

    fn check(&self) -> CheckResult {
        #[cfg(not(unix))]
        {
            return CheckResult {
                ok: false,
                missing: Vec::new(),
                message: Some("service requires a POSIX system.".to_string()),
            };
        }
        #[cfg(unix)]
        {
            imp::check()
        }
    }
}

#[cfg(not(unix))]
fn posix_only_error() -> ToolError {
    ToolError {
        py_class: PyExc::RuntimeError,
        message: "service: requires a POSIX system (process groups, flock).".to_string(),
        cause: ErrorCause::None,
    }
}

/// Joins every `service` teardown or stop a cancelled (or otherwise
/// dropped) call displaced -- `electricity-cli`'s own `main` calls
/// this, on the same runtime the run itself used, after the run's
/// `block_on` has returned and before the process's exit code is
/// decided, so the process never exits while one of those is still
/// signalling a group or rewriting a record. A no-op once nothing is
/// outstanding (the common case): draining an empty list costs
/// nothing.
#[cfg(unix)]
pub async fn join_pending_teardowns() {
    imp::join_pending_teardowns().await;
}

#[cfg(not(unix))]
pub async fn join_pending_teardowns() {}

#[cfg(unix)]
mod imp {
    use super::ServicePlugin;
    use crate::subprocess_tool::{as_bool_param, is_truthy};
    use crate::{CheckResult, ErrorCause, PyExc, ToolCall, ToolError, ToolResult};
    use electricity_json::{WriteMode, dumps, loads};
    use electricity_value::pycompat::py_strip;
    use electricity_value::{Dict, Value};
    use std::collections::HashMap;
    use std::fs::{self, OpenOptions};
    use std::io::{self, Read, Seek, SeekFrom};
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    use std::os::unix::io::AsRawFd;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const ACTIONS: [&str; 3] = ["start", "stop", "status"];
    const LOCAL_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];
    const DEFAULT_READY_TIMEOUT_MS: u64 = 60_000;
    const DEFAULT_GRACE_MS: u64 = 5_000;
    const SETTLE_SECONDS: f64 = 5.0;
    const POLL_MS: u64 = 100;
    const LOG_TAIL_LINES: usize = 40;
    const LOG_TAIL_CHARS: usize = 4_000;
    const BOOT_TIME_TOLERANCE_SECONDS: i64 = 5;
    const HELPER_TIMEOUT: Duration = Duration::from_secs(10);

    fn value_error(message: impl Into<String>) -> ToolError {
        ToolError {
            py_class: PyExc::ValueError,
            message: message.into(),
            cause: ErrorCause::None,
        }
    }

    fn runtime_error(message: impl Into<String>) -> ToolError {
        ToolError {
            py_class: PyExc::RuntimeError,
            message: message.into(),
            cause: ErrorCause::None,
        }
    }

    fn repr_str(s: &str) -> String {
        Value::Str(s.to_string()).py_repr()
    }

    /// `str(params.get(key) or "")` -- a falsy value (absent, `None`,
    /// `""`, `false`, `0`, ...) becomes an empty string; a truthy value
    /// passes through unchanged.
    fn falsy_or_default(value: Option<&Value>) -> Value {
        match value {
            Some(v) if is_truthy(v) => v.clone(),
            _ => Value::from(""),
        }
    }

    fn get<'a>(params: &'a Dict, key: &str) -> Option<&'a Value> {
        params.get(&Value::Str(key.to_string()))
    }

    /// `_NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,99}$")`.
    fn valid_name(name: &str) -> bool {
        let bytes = name.as_bytes();
        if bytes.is_empty() || bytes.len() > 100 {
            return false;
        }
        if !bytes[0].is_ascii_alphanumeric() {
            return false;
        }
        bytes
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-')
    }

    fn value_as_i64(value: &Value) -> Option<i64> {
        match value {
            Value::Int(_) => value.as_int().map(|iv| iv.to_f64() as i64),
            Value::Float(f) => Some(*f as i64),
            Value::Str(s) => py_strip(s).parse().ok(),
            _ => None,
        }
    }

    /// Python's general float format (`{x:g}`): 6 significant digits,
    /// switching to scientific notation once the exponent falls
    /// outside `-4..6`, with trailing zeros (and a bare trailing `.`)
    /// trimmed either way.
    fn format_g(x: f64) -> String {
        if x == 0.0 {
            return "0".to_string();
        }
        let neg = x.is_sign_negative();
        let ax = x.abs();
        const PRECISION: i32 = 6;
        // Scientific notation at `PRECISION` significant digits reads
        // off the true base-10 exponent (with the rounding already
        // applied) directly from where `{:e}` places the decimal
        // point -- sidestepping `log10().floor()`'s own rounding
        // trouble exactly at a power of ten.
        let sci = format!("{ax:.*e}", (PRECISION - 1) as usize);
        let (mantissa, exp_str) = sci.split_once('e').expect("`{:e}` always has an exponent");
        let exp: i32 = exp_str.parse().expect("a valid exponent");
        let body = if !(-4..PRECISION).contains(&exp) {
            format!(
                "{}e{}{:02}",
                trim_trailing_zeros(mantissa),
                if exp < 0 { '-' } else { '+' },
                exp.abs()
            )
        } else {
            let decimals = (PRECISION - 1 - exp).max(0) as usize;
            trim_trailing_zeros(&format!("{ax:.decimals$}")).to_string()
        };
        if neg { format!("-{body}") } else { body }
    }

    fn trim_trailing_zeros(s: &str) -> &str {
        if !s.contains('.') {
            return s;
        }
        s.trim_end_matches('0').trim_end_matches('.')
    }

    fn is_linux_proc() -> bool {
        Path::new("/proc/self/stat").exists()
    }

    /// `core/cancellation.py`'s own locale/zone-independent helper env.
    /// Runs on a dedicated OS thread, never the caller's: every call site
    /// that can run on the async runtime's own thread (as opposed to a
    /// `TeardownOnCancel::drop` dedicated thread, which can block freely)
    /// reaches this through `tokio::task::spawn_blocking`. A `ps`/`lsof`
    /// that outlives [`HELPER_TIMEOUT`] is killed outright, not merely
    /// abandoned.
    fn helper(cmd: &[&str]) -> Option<(String, bool)> {
        let owned: Vec<String> = cmd.iter().map(|s| s.to_string()).collect();
        let (tx, rx) = std::sync::mpsc::channel();
        let (pid_tx, pid_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut command = Command::new(&owned[0]);
            command.args(&owned[1..]);
            command.env("LC_ALL", "C");
            command.env("TZ", "UTC");
            command.stdin(Stdio::null());
            command.stdout(Stdio::piped());
            command.stderr(Stdio::piped());
            match command.spawn() {
                Ok(child) => {
                    let _ = pid_tx.send(child.id());
                    let _ = tx.send(child.wait_with_output());
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                }
            }
        });
        match rx.recv_timeout(HELPER_TIMEOUT) {
            Ok(Ok(output)) => Some((
                String::from_utf8_lossy(&output.stdout).into_owned(),
                output.status.success(),
            )),
            _ => {
                if let Ok(pid) = pid_rx.try_recv() {
                    unsafe {
                        libc::kill(pid as libc::pid_t, libc::SIGKILL);
                    }
                }
                None
            }
        }
    }

    fn pid_exists(pid: u32) -> bool {
        let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if ret == 0 {
            return true;
        }
        io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }

    /// `core/cancellation.py::boot_time`.
    fn boot_time() -> Result<i64, ToolError> {
        if is_linux_proc() {
            let text = fs::read_to_string("/proc/stat")
                .map_err(|e| runtime_error(format!("service: cannot read /proc/stat: {e}")))?;
            for line in text.lines() {
                if let Some(rest) = line.strip_prefix("btime ") {
                    if let Ok(v) = rest.trim().parse::<i64>() {
                        return Ok(v);
                    }
                }
            }
            return Err(runtime_error("service: no btime in /proc/stat."));
        }
        if let Some((stdout, _)) = helper(&["sysctl", "-n", "kern.boottime"]) {
            if let Some(sec) = extract_sysctl_sec(&stdout) {
                return Ok(sec);
            }
        }
        Err(runtime_error(
            "service: cannot read the boot time (sysctl -n kern.boottime).",
        ))
    }

    /// `re.search(r"sec\s*=\s*(\d+)", ...)`.
    fn extract_sysctl_sec(text: &str) -> Option<i64> {
        let idx = text.find("sec")?;
        let rest = text[idx + 3..].trim_start();
        let rest = rest.strip_prefix('=')?.trim_start();
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            None
        } else {
            digits.parse().ok()
        }
    }

    /// `core/cancellation.py::process_start_time`.
    fn process_start_time(pid: u32) -> Result<Option<String>, ToolError> {
        if is_linux_proc() {
            let text = match fs::read_to_string(format!("/proc/{pid}/stat")) {
                Ok(t) => t,
                Err(_) => return Ok(None),
            };
            let Some(idx) = text.rfind(')') else {
                return Ok(None);
            };
            let fields: Vec<&str> = text[idx + 2..].split_whitespace().collect();
            return Ok(fields.get(19).map(|s| s.to_string()));
        }
        if let Some((stdout, success)) = helper(&["ps", "-o", "lstart=", "-p", &pid.to_string()]) {
            let value = stdout.trim();
            if success && !value.is_empty() {
                return Ok(Some(value.to_string()));
            }
        }
        if !pid_exists(pid) {
            return Ok(None);
        }
        Err(runtime_error(format!(
            "service: cannot read the start time of pid {pid} (ps -o lstart=)."
        )))
    }

    fn group_has_running_member(pgid: u32) -> bool {
        if is_linux_proc() {
            let Ok(entries) = fs::read_dir("/proc") else {
                return false;
            };
            for entry in entries.flatten() {
                let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else {
                    continue;
                };
                if !name.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                let Ok(text) = fs::read_to_string(entry.path().join("stat")) else {
                    continue;
                };
                let Some(idx) = text.rfind(')') else {
                    continue;
                };
                let fields: Vec<&str> = text[idx + 2..].split_whitespace().collect();
                if fields.len() > 2
                    && fields[2] == pgid.to_string()
                    && fields[0] != "Z"
                    && fields[0] != "X"
                {
                    return true;
                }
            }
            return false;
        }
        let Some((stdout, success)) = helper(&["ps", "-A", "-o", "pgid=,stat="]) else {
            return true; // cannot tell: never call a group gone without proof
        };
        if !success {
            return true;
        }
        for line in stdout.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 && parts[0] == pgid.to_string() && !parts[1].starts_with('Z') {
                return true;
            }
        }
        false
    }

    fn group_exists(pgid: u32) -> bool {
        let ret = unsafe { libc::killpg(pgid as libc::pid_t, 0) };
        if ret != 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return false;
        }
        group_has_running_member(pgid)
    }

    /// [`group_exists`] off the async runtime's own thread (see
    /// [`helper`]'s own doc comment) -- every call from `wait_ready`'s
    /// and `terminate_group`'s own poll loops goes through this, not
    /// the bare sync function, so a slow `ps` never stalls a sibling
    /// branch dispatched on the same single-threaded runtime.
    async fn group_exists_nb(pgid: u32) -> bool {
        tokio::task::spawn_blocking(move || group_exists(pgid))
            .await
            .unwrap_or(false)
    }

    struct Ownership {
        ours: bool,
        reason: String,
    }

    /// `core/cancellation.py::check_ownership`.
    fn check_ownership(record: &Dict) -> Result<Ownership, ToolError> {
        let incomplete = || Ownership {
            ours: false,
            reason: "the record is incomplete".to_string(),
        };
        let Some(pgid) = get(record, "pgid").and_then(value_as_i64) else {
            return Ok(incomplete());
        };
        let Some(recorded_start) = get(record, "start_time").map(Value::py_str) else {
            return Ok(incomplete());
        };
        let Some(recorded_boot) = get(record, "boot_time").and_then(value_as_i64) else {
            return Ok(incomplete());
        };
        if pgid <= 1 {
            return Ok(Ownership {
                ours: false,
                reason: "the record names no process group".to_string(),
            });
        }
        if (boot_time()? - recorded_boot).abs() > BOOT_TIME_TOLERANCE_SECONDS {
            return Ok(Ownership {
                ours: false,
                reason: "the machine has rebooted since it was started".to_string(),
            });
        }
        let start = process_start_time(pgid as u32)?;
        if let Some(start) = &start {
            if *start != recorded_start {
                return Ok(Ownership {
                    ours: false,
                    reason: format!(
                        "pid {pgid} now belongs to a different process (started {}, recorded {})",
                        repr_str(start),
                        repr_str(&recorded_start),
                    ),
                });
            }
        }
        if !group_exists(pgid as u32) {
            return Ok(Ownership {
                ours: false,
                reason: "its process group no longer exists".to_string(),
            });
        }
        if start.is_some() {
            Ok(Ownership {
                ours: true,
                reason: "its group leader is running".to_string(),
            })
        } else {
            Ok(Ownership {
                ours: true,
                reason: "its group leader exited; members of its process group remain".to_string(),
            })
        }
    }

    // -- ports / holders --------------------------------------------------

    #[derive(Clone)]
    struct Holder {
        port: i64,
        pid: Option<i64>,
        pgid: Option<i64>,
        /// A plain string (`ps -o command=`) for a foreign process;
        /// the full argv list for a port claimed by another of our own
        /// services, copied straight from that service's own record --
        /// `claims[port].get("command")` on cof's side, a list there
        /// too.
        command: Option<Value>,
        cwd: Option<String>,
        service: Option<String>,
    }

    fn holder_to_value(h: &Holder, ours: Option<bool>) -> Value {
        let mut d = Dict::new();
        d.insert(Value::from("port"), Value::from(h.port));
        d.insert(
            Value::from("pid"),
            h.pid.map(Value::from).unwrap_or(Value::None),
        );
        d.insert(
            Value::from("pgid"),
            h.pgid.map(Value::from).unwrap_or(Value::None),
        );
        d.insert(
            Value::from("command"),
            h.command.clone().unwrap_or(Value::None),
        );
        d.insert(
            Value::from("cwd"),
            h.cwd.clone().map(Value::from).unwrap_or(Value::None),
        );
        if let Some(svc) = &h.service {
            d.insert(Value::from("service"), Value::from(svc.as_str()));
        }
        if let Some(o) = ours {
            d.insert(Value::from("ours"), Value::from(o));
        }
        Value::Dict(d)
    }

    fn format_holder(h: &Holder) -> String {
        if let Some(service) = &h.service {
            return format!(
                "port {} is claimed by service {} (pgid {})",
                h.port,
                repr_str(service),
                h.pgid.map(|p| p.to_string()).unwrap_or("None".to_string()),
            );
        }
        match h.pid {
            None => format!(
                "port {} is held by a process this plugin did not start \
                 (install lsof to see its pid, command and cwd)",
                h.port
            ),
            Some(pid) => {
                let command_text = match &h.command {
                    Some(Value::Str(s)) => repr_str(s),
                    _ => repr_str("?"),
                };
                format!(
                    "port {} is held by a process this plugin did not start: pid {pid}, \
                     command {command_text}, cwd {}",
                    h.port,
                    repr_str(h.cwd.as_deref().unwrap_or("?")),
                )
            }
        }
    }

    async fn port_accepts(port: u16) -> bool {
        for host in ["127.0.0.1", "::1"] {
            let addr = if host.contains(':') {
                format!("[{host}]:{port}")
            } else {
                format!("{host}:{port}")
            };
            if let Ok(Ok(_)) = tokio::time::timeout(
                Duration::from_millis(500),
                tokio::net::TcpStream::connect(&addr),
            )
            .await
            {
                return true;
            }
        }
        false
    }

    fn describe_holder(pid: i64, port: i64) -> Holder {
        let command = helper(&["ps", "-o", "command=", "-p", &pid.to_string()])
            .filter(|(_, ok)| *ok)
            .map(|(out, _)| out.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(|s| Value::from(s.as_str()));
        let mut cwd = if is_linux_proc() {
            fs::read_link(format!("/proc/{pid}/cwd"))
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        } else {
            None
        };
        if cwd.is_none() && crate::process::which("lsof").is_some() {
            if let Some((out, ok)) =
                helper(&["lsof", "-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
            {
                if ok {
                    for line in out.lines() {
                        if let Some(rest) = line.strip_prefix('n') {
                            cwd = Some(rest.to_string());
                            break;
                        }
                    }
                }
            }
        }
        let pgid = {
            let g = unsafe { libc::getpgid(pid as libc::pid_t) };
            if g == -1 { None } else { Some(g as i64) }
        };
        Holder {
            port,
            pid: Some(pid),
            pgid,
            command,
            cwd,
            service: None,
        }
    }

    /// `plugins/service.py::port_holders`. The `lsof` listing and the
    /// per-pid `ps`/`lsof` detail queries ([`describe_holder`]) run on
    /// a dedicated blocking-pool thread (see [`helper`]'s own doc
    /// comment); only the TCP connect fallback ([`port_accepts`]) is
    /// genuinely async.
    async fn port_holders(port: i64) -> Vec<Holder> {
        if crate::process::which("lsof").is_some() {
            let found = tokio::task::spawn_blocking(move || {
                let mut found = Vec::new();
                if let Some((out, _)) = helper(&[
                    "lsof",
                    "-nP",
                    &format!("-iTCP:{port}"),
                    "-sTCP:LISTEN",
                    "-Fp",
                ]) {
                    let mut pids: Vec<i64> = out
                        .lines()
                        .filter_map(|l| l.strip_prefix('p'))
                        .filter_map(|s| s.parse::<i64>().ok())
                        .collect();
                    pids.sort_unstable();
                    pids.dedup();
                    found.extend(pids.into_iter().map(|pid| describe_holder(pid, port)));
                }
                found
            })
            .await
            .unwrap_or_default();
            if !found.is_empty() {
                return found;
            }
        }
        if port_accepts(port as u16).await {
            return vec![Holder {
                port,
                pid: None,
                pgid: None,
                command: None,
                cwd: None,
                service: None,
            }];
        }
        vec![]
    }

    // -- ready -------------------------------------------------------------

    #[derive(Clone)]
    enum ReadySpec {
        Port(i64),
        Url(String),
    }

    #[derive(Clone)]
    struct ParsedUrl {
        scheme: String,
        host: String,
        port: u16,
        path: String,
    }

    fn parse_url(text: &str) -> Option<ParsedUrl> {
        let (scheme, rest) = text.split_once("://")?;
        let scheme = scheme.to_lowercase();
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let authority = authority.rsplit('@').next().unwrap_or(authority);
        let (host, port) = if let Some(stripped) = authority.strip_prefix('[') {
            let end = stripped.find(']')?;
            let host = stripped[..end].to_string();
            let after = &stripped[end + 1..];
            let port = after.strip_prefix(':').and_then(|p| p.parse().ok());
            (host, port)
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
                    (h.to_string(), p.parse().ok())
                }
                _ => (authority.to_string(), None),
            }
        };
        if host.is_empty() {
            return None;
        }
        let default_port = if scheme == "https" { 443 } else { 80 };
        Some(ParsedUrl {
            scheme,
            host,
            port: port.unwrap_or(default_port),
            path: path.to_string(),
        })
    }

    fn parse_port_value(value: &Value, what: &str) -> Result<i64, ToolError> {
        let text = value.py_str();
        let trimmed = py_strip(&text);
        let port: i64 = trimmed.parse().map_err(|_| {
            value_error(format!(
                "service: {what} must be a port number, got {}.",
                value.py_repr()
            ))
        })?;
        if !(1..=65535).contains(&port) {
            return Err(value_error(format!(
                "service: {what} {port} is out of range (1-65535)."
            )));
        }
        Ok(port)
    }

    fn parse_ms(params: &Dict, key: &str, default: u64) -> Result<u64, ToolError> {
        let value = match get(params, key) {
            None | Some(Value::None) => return Ok(default),
            Some(Value::Str(s)) if s.is_empty() => return Ok(default),
            Some(v) => v,
        };
        let text = value.py_str();
        let trimmed = py_strip(&text);
        let ms: i64 = trimmed.parse().map_err(|_| {
            value_error(format!(
                "service: params[{}] must be milliseconds, got {}.",
                repr_str(key),
                value.py_repr()
            ))
        })?;
        if ms < 0 {
            return Err(value_error(format!(
                "service: params[{}] must not be negative.",
                repr_str(key)
            )));
        }
        Ok(ms as u64)
    }

    fn parse_ready(value: Option<&Value>) -> Result<Option<ReadySpec>, ToolError> {
        let value = match value {
            None | Some(Value::None) => return Ok(None),
            Some(Value::Str(s)) if s.is_empty() => return Ok(None),
            Some(v) => v,
        };
        let text = py_strip(&value.py_str()).to_string();
        if text.contains("://") {
            let ok = parse_url(&text).is_some_and(|p| p.scheme == "http" || p.scheme == "https");
            if !ok {
                return Err(value_error(format!(
                    "service: params['ready'] URL must be http(s)://host..., got {}.",
                    repr_str(&text)
                )));
            }
            return Ok(Some(ReadySpec::Url(text)));
        }
        Ok(Some(ReadySpec::Port(parse_port_value(
            &Value::from(text.as_str()),
            "params['ready']",
        )?)))
    }

    fn ready_port(spec: &ReadySpec) -> Option<i64> {
        match spec {
            ReadySpec::Port(p) => Some(*p),
            ReadySpec::Url(text) => {
                let parsed = parse_url(text)?;
                if !LOCAL_HOSTS.contains(&parsed.host.as_str()) {
                    return None;
                }
                Some(parsed.port as i64)
            }
        }
    }

    fn ready_to_value(ready: &Option<ReadySpec>) -> Value {
        match ready {
            None => Value::None,
            Some(ReadySpec::Port(p)) => {
                let mut d = Dict::new();
                d.insert(Value::from("port"), Value::from(*p));
                Value::Dict(d)
            }
            Some(ReadySpec::Url(u)) => {
                let mut d = Dict::new();
                d.insert(Value::from("url"), Value::from(u.as_str()));
                Value::Dict(d)
            }
        }
    }

    fn value_to_ready(value: &Value) -> Option<ReadySpec> {
        let d = value.as_dict()?;
        if let Some(p) = d.get(&Value::from("port")).and_then(value_as_i64) {
            return Some(ReadySpec::Port(p));
        }
        if let Some(u) = d.get(&Value::from("url")).and_then(Value::as_str) {
            return Some(ReadySpec::Url(u.to_string()));
        }
        None
    }

    fn ready_target_text(spec: &ReadySpec) -> String {
        match spec {
            ReadySpec::Url(u) => u.clone(),
            ReadySpec::Port(p) => format!("port {p}"),
        }
    }

    #[derive(Debug)]
    struct AcceptAnyServerCert {
        algs: rustls::crypto::WebPkiSupportedAlgorithms,
    }

    impl rustls::client::danger::ServerCertVerifier for AcceptAnyServerCert {
        fn verify_server_cert(
            &self,
            _end_entity: &rustls_pki_types::CertificateDer<'_>,
            _intermediates: &[rustls_pki_types::CertificateDer<'_>],
            _server_name: &rustls_pki_types::ServerName<'_>,
            _ocsp_response: &[u8],
            _now: rustls_pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &rustls_pki_types::CertificateDer<'_>,
            dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algs)
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &rustls_pki_types::CertificateDer<'_>,
            dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algs)
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            self.algs
                .mapping
                .iter()
                .map(|(scheme, _)| *scheme)
                .collect()
        }
    }

    /// `ssl.create_default_context()` with `check_hostname = False`,
    /// `verify_mode = ssl.CERT_NONE` -- a local dev server usually has a
    /// self-signed certificate, so the readiness probe only checks "the
    /// server answers", never the chain of trust.
    fn tls_config_no_verify() -> std::sync::Arc<rustls::ClientConfig> {
        let algs = rustls::crypto::ring::default_provider().signature_verification_algorithms;
        let verifier = std::sync::Arc::new(AcceptAnyServerCert { algs });
        let mut config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        std::sync::Arc::new(config)
    }

    const MAX_RESPONSE_HEAD_BYTES: usize = 8192;

    /// Reads until the blank line ending the response's headers (or
    /// the connection closes, or [`MAX_RESPONSE_HEAD_BYTES`] is hit) --
    /// enough to carry a `Location` header for [`fetch_status`]'s own
    /// redirect following, not just the status line a single-read
    /// 1024-byte buffer used to settle for.
    async fn read_response_head<S: tokio::io::AsyncRead + Unpin>(
        stream: &mut S,
    ) -> Option<Vec<u8>> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() >= MAX_RESPONSE_HEAD_BYTES {
                break;
            }
            let n = stream.read(&mut chunk).await.ok()?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        if buf.is_empty() { None } else { Some(buf) }
    }

    fn parse_status_code(bytes: &[u8]) -> Option<u16> {
        let text = String::from_utf8_lossy(bytes);
        let line = text.lines().next()?;
        line.split_whitespace().nth(1)?.parse().ok()
    }

    /// The first header named *name* (case-insensitive), scanning only
    /// up to the blank line ending the headers.
    fn parse_header(bytes: &[u8], name: &str) -> Option<String> {
        let text = String::from_utf8_lossy(bytes);
        let mut lines = text.lines();
        lines.next()?; // the status line
        for line in lines {
            if line.is_empty() {
                break;
            }
            if let Some((key, value)) = line.split_once(':') {
                if key.trim().eq_ignore_ascii_case(name) {
                    return Some(value.trim().to_string());
                }
            }
        }
        None
    }

    /// *url*'s own `Host` header: bracketed when the host is an IPv6
    /// literal, and carrying the port whenever it isn't the scheme's
    /// default -- `http://[::1]:8080/` sends `Host: [::1]:8080`, not
    /// bare `::1` (ambiguous with a port) or no port at all.
    fn host_header(parsed: &ParsedUrl) -> String {
        let default_port = if parsed.scheme == "https" { 443 } else { 80 };
        let host = if parsed.host.contains(':') {
            format!("[{}]", parsed.host)
        } else {
            parsed.host.clone()
        };
        if parsed.port == default_port {
            host
        } else {
            format!("{host}:{}", parsed.port)
        }
    }

    /// A redirect's `Location` header resolved against *current* --
    /// an absolute `http(s)://` target is parsed outright; anything
    /// else is treated as a path on *current*'s own host (a local dev
    /// server's redirects are never anything more exotic than that in
    /// practice), lexically normalized the same way [`resolve_path`]
    /// normalizes a nonexistent filesystem path's own tail.
    fn resolve_redirect(current: &ParsedUrl, location: &str) -> Option<ParsedUrl> {
        if location.contains("://") {
            return parse_url(location).filter(|p| p.scheme == "http" || p.scheme == "https");
        }
        let path = if location.starts_with('/') {
            location.to_string()
        } else {
            let base = current.path.rsplit_once('/').map_or("", |(base, _)| base);
            format!("{base}/{location}")
        };
        Some(ParsedUrl {
            scheme: current.scheme.clone(),
            host: current.host.clone(),
            port: current.port,
            path,
        })
    }

    /// Up to this many redirects are followed -- `urllib.request`'s
    /// own `HTTPRedirectHandler.max_redirections`, the limit cof's own
    /// readiness probe (an `opener.open()` through the default
    /// handlers) inherits.
    const MAX_REDIRECTS: u32 = 10;

    /// A single GET, no redirect following (DESIGN.md §13): "ready on
    /// any status below 500" already treats a 3xx as ready, the common
    /// case; a redirect to a 5xx is the one divergence from cof, which
    /// follows it via urllib's own default `HTTPRedirectHandler`.
    async fn fetch_once(parsed: &ParsedUrl) -> Option<Vec<u8>> {
        let host_for_addr = if parsed.host.contains(':') {
            format!("[{}]", parsed.host)
        } else {
            parsed.host.clone()
        };
        let addr = format!("{host_for_addr}:{}", parsed.port);
        let tcp = tokio::net::TcpStream::connect(&addr).await.ok()?;
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            parsed.path,
            host_header(parsed)
        );
        if parsed.scheme == "https" {
            let connector = tokio_rustls::TlsConnector::from(tls_config_no_verify());
            let server_name = rustls_pki_types::ServerName::try_from(parsed.host.clone()).ok()?;
            let mut tls = connector.connect(server_name, tcp).await.ok()?;
            tls.write_all(request.as_bytes()).await.ok()?;
            read_response_head(&mut tls).await
        } else {
            let mut tcp = tcp;
            tcp.write_all(request.as_bytes()).await.ok()?;
            read_response_head(&mut tcp).await
        }
    }

    /// `_probe_ready`'s own `opener.open(url)` through urllib's
    /// default `HTTPRedirectHandler`: follows up to [`MAX_REDIRECTS`]
    /// redirects before reading the final response's status.
    async fn fetch_status(parsed: &ParsedUrl) -> Option<u16> {
        let mut current = parsed.clone();
        for _ in 0..=MAX_REDIRECTS {
            let head = fetch_once(&current).await?;
            let status = parse_status_code(&head)?;
            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                if let Some(location) = parse_header(&head, "location") {
                    if let Some(next) = resolve_redirect(&current, &location) {
                        current = next;
                        continue;
                    }
                }
            }
            return Some(status);
        }
        None
    }

    async fn probe_url(text: &str) -> bool {
        let Some(parsed) = parse_url(text) else {
            return false;
        };
        let outcome = tokio::time::timeout(Duration::from_secs(2), fetch_status(&parsed)).await;
        matches!(outcome, Ok(Some(status)) if status < 500)
    }

    async fn probe_ready(spec: &ReadySpec) -> bool {
        match spec {
            ReadySpec::Port(p) => port_accepts(*p as u16).await,
            ReadySpec::Url(text) => probe_url(text).await,
        }
    }

    // -- state directory / lock / record -----------------------------------

    /// `plugins/service.py::default_state_dir`.
    fn default_state_dir() -> PathBuf {
        let home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("."));
        home.join(".config").join("circuitry").join("services")
    }

    fn expand_user(raw: &str) -> PathBuf {
        if raw == "~" {
            return std::env::home_dir().unwrap_or_else(|| PathBuf::from(raw));
        }
        if let Some(rest) = raw.strip_prefix("~/") {
            if let Some(home) = std::env::home_dir() {
                return home.join(rest);
            }
        }
        PathBuf::from(raw)
    }

    /// `Path(...).resolve()`: canonicalize the longest existing
    /// ancestor, then lexically append (and normalize `.`/`..` within)
    /// whatever doesn't exist yet. Unlike a bare `fs::canonicalize`
    /// fallback (which gives up and returns the raw, un-resolved path
    /// the moment the target itself is missing), this still resolves a
    /// symlinked *ancestor* of a `log`/`cwd` that doesn't exist yet --
    /// a `log: /tmp/x.log` on macOS (`/tmp` a symlink to `/private/tmp`)
    /// records `/private/tmp/x.log`, matching Python's own `resolve()`,
    /// even though `x.log` itself is created only as this very call
    /// runs.
    fn resolve_path(raw: &Path) -> PathBuf {
        let joined = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_default().join(raw)
        };
        let mut existing = joined.clone();
        let mut suffix: Vec<std::ffi::OsString> = Vec::new();
        while !existing.exists() {
            let Some(name) = existing.file_name().map(|n| n.to_os_string()) else {
                break;
            };
            suffix.push(name);
            let Some(parent) = existing.parent().map(Path::to_path_buf) else {
                break;
            };
            existing = parent;
        }
        let mut result = fs::canonicalize(&existing).unwrap_or(existing);
        for component in suffix.into_iter().rev() {
            if component == ".." {
                result.pop();
            } else if component != "." {
                result.push(component);
            }
        }
        result
    }

    impl ServicePlugin {
        /// `ServicePlugin._dir` -- `expanduser()` only, deliberately
        /// never `resolve()`: a relative `state_dir` stays relative to
        /// the process's own cwd exactly as cof leaves it, and a
        /// `state_dir` under a symlinked directory (`/tmp` on macOS)
        /// is never rewritten to its canonical form.
        fn dir(&self) -> PathBuf {
            match &self.state_dir {
                Some(s) => expand_user(s),
                None => default_state_dir(),
            }
        }
    }

    /// Creates *dir* with mode `0o700` directly (`DirBuilder::mode`,
    /// not a plain `create_dir` followed by `set_permissions`: a
    /// window between the two would leave *dir* briefly
    /// world-readable), only when this call is the one that creates it
    /// (`Path.mkdir(mode=0o700, parents=True, exist_ok=True)`'s own
    /// behaviour: an already-existing directory's permissions are left
    /// untouched, and any missing *parent* gets the process's default
    /// mode, not `0o700`).
    fn create_0700(dir: &Path) -> io::Result<()> {
        std::fs::DirBuilder::new().mode(0o700).create(dir)
    }

    fn ensure_dir(dir: &Path) -> io::Result<()> {
        match create_0700(dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if let Some(parent) = dir.parent() {
                    fs::create_dir_all(parent)?;
                }
                match create_0700(dir) {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }

    struct LockGuard {
        file: std::fs::File,
    }

    impl LockGuard {
        /// Non-blocking, polled with an async sleep between tries --
        /// never a plain blocking `LOCK_EX`, which would block
        /// electricity's own single-threaded dispatch runtime for as
        /// long as another branch (parallel `start`/`stop` on the same
        /// state directory) holds the lock, starving every sibling
        /// branch of that same run, including the one that holds it.
        async fn acquire(dir: &Path) -> io::Result<Self> {
            ensure_dir(dir)?;
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(dir.join(".lock"))?;
            loop {
                let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
                if ret == 0 {
                    return Ok(LockGuard { file });
                }
                let err = io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
                    return Err(err);
                }
                tokio::time::sleep(Duration::from_millis(POLL_MS)).await;
            }
        }
    }

    impl Drop for LockGuard {
        fn drop(&mut self) {
            unsafe {
                libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }

    fn record_path(dir: &Path, name: &str) -> PathBuf {
        dir.join(format!("{name}.json"))
    }

    enum LoadOutcome {
        Missing,
        Unreadable,
        Parsed(Dict),
    }

    fn load_record(dir: &Path, name: &str) -> LoadOutcome {
        let path = record_path(dir, name);
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return LoadOutcome::Missing,
            Err(_) => return LoadOutcome::Unreadable,
        };
        match loads(&text) {
            Ok(ref v) => match v.as_dict() {
                Some(d) => LoadOutcome::Parsed(d.clone()),
                None => LoadOutcome::Unreadable,
            },
            Err(_) => LoadOutcome::Unreadable,
        }
    }

    fn drop_record(dir: &Path, name: &str) {
        let _ = fs::remove_file(record_path(dir, name));
    }

    fn save_record(dir: &Path, record: &Dict) -> Result<(), ToolError> {
        let name = get(record, "name")
            .and_then(Value::as_str)
            .expect("records always carry their own name");
        let path = record_path(dir, name);
        let tmp = dir.join(format!("{name}.json.tmp"));
        let text = dumps(&Value::Dict(record.clone()), WriteMode::PRETTY)
            .map_err(|e| runtime_error(format!("service: cannot encode the record: {e}")))?;
        fs::write(&tmp, text)
            .map_err(|e| runtime_error(format!("service: cannot write the record: {e}")))?;
        fs::rename(&tmp, &path)
            .map_err(|e| runtime_error(format!("service: cannot write the record: {e}")))?;
        Ok(())
    }

    /// `ServicePlugin._live_record` -- the record for *name* when it
    /// still proves ours; otherwise drop it and return why it was stale.
    fn live_record(dir: &Path, name: &str) -> Result<(Option<Dict>, Option<String>), ToolError> {
        match load_record(dir, name) {
            LoadOutcome::Missing => Ok((None, None)),
            LoadOutcome::Unreadable => {
                drop_record(dir, name);
                Ok((None, Some("the record is incomplete".to_string())))
            }
            LoadOutcome::Parsed(record) => match check_ownership(&record)? {
                Ownership { ours: true, .. } => Ok((Some(record), None)),
                Ownership {
                    ours: false,
                    reason,
                } => {
                    drop_record(dir, name);
                    Ok((None, Some(reason)))
                }
            },
        }
    }

    /// `ServicePlugin._other_claims` -- port -> live record of another
    /// service of ours that claims it.
    fn other_claims(dir: &Path, name: &str) -> Result<HashMap<i64, Dict>, ToolError> {
        let mut claims = HashMap::new();
        let Ok(entries) = fs::read_dir(dir) else {
            return Ok(claims);
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
            .collect();
        paths.sort();
        for path in paths {
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if stem == name {
                continue;
            }
            let (record, _) = live_record(dir, stem)?;
            if let Some(record) = record {
                if let Some(ports) = get(&record, "ports").and_then(Value::as_list) {
                    for port in ports {
                        if let Some(p) = value_as_i64(port) {
                            claims.insert(p, record.clone());
                        }
                    }
                }
            }
        }
        Ok(claims)
    }

    /// [`live_record`] off the async runtime's own thread (see
    /// [`helper`]'s own doc comment) -- every `start`/`stop`/`status`
    /// call site uses this, not the bare sync function.
    async fn live_record_nb(
        dir: PathBuf,
        name: String,
    ) -> Result<(Option<Dict>, Option<String>), ToolError> {
        tokio::task::spawn_blocking(move || live_record(&dir, &name))
            .await
            .map_err(|e| runtime_error(format!("service: internal error: {e}")))?
    }

    /// [`other_claims`] off the async runtime's own thread (see
    /// [`helper`]'s own doc comment).
    async fn other_claims_nb(dir: PathBuf, name: String) -> Result<HashMap<i64, Dict>, ToolError> {
        tokio::task::spawn_blocking(move || other_claims(&dir, &name))
            .await
            .map_err(|e| runtime_error(format!("service: internal error: {e}")))?
    }

    /// [`process_start_time`] off the async runtime's own thread (see
    /// [`helper`]'s own doc comment).
    async fn process_start_time_nb(pid: u32) -> Result<Option<String>, ToolError> {
        tokio::task::spawn_blocking(move || process_start_time(pid))
            .await
            .map_err(|e| runtime_error(format!("service: internal error: {e}")))?
    }

    /// [`boot_time`] off the async runtime's own thread (see
    /// [`helper`]'s own doc comment).
    async fn boot_time_nb() -> Result<i64, ToolError> {
        tokio::task::spawn_blocking(boot_time)
            .await
            .map_err(|e| runtime_error(format!("service: internal error: {e}")))?
    }

    fn log_tail(path: &Path, offset: u64) -> String {
        let Ok(mut file) = std::fs::File::open(path) else {
            return String::new();
        };
        if file.seek(SeekFrom::Start(offset)).is_err() {
            return String::new();
        }
        let mut data = Vec::new();
        if file.read_to_end(&mut data).is_err() {
            return String::new();
        }
        let text = String::from_utf8_lossy(&data).into_owned();
        let lines: Vec<&str> = text.lines().collect();
        let tail_lines: &[&str] = if lines.len() > LOG_TAIL_LINES {
            &lines[lines.len() - LOG_TAIL_LINES..]
        } else {
            &lines[..]
        };
        let joined = tail_lines.join("\n");
        let chars: Vec<char> = joined.chars().collect();
        if chars.len() <= LOG_TAIL_CHARS {
            joined
        } else {
            chars[chars.len() - LOG_TAIL_CHARS..].iter().collect()
        }
    }

    fn iso_now() -> String {
        chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.6f+00:00")
            .to_string()
    }

    fn exit_code_of(status: &std::process::ExitStatus) -> i32 {
        match status.code() {
            Some(c) => c,
            None => -status.signal().unwrap_or(0),
        }
    }

    /// Teardown/stop work this process displaced -- a [`TeardownOnCancel`]
    /// drop, or [`stop_internal`]'s own locked section -- spawned onto
    /// the runtime rather than run inline or awaited to completion, so
    /// a call whose own future is dropped (cancellation; see both call
    /// sites' own comments) never leaves a group half-signalled or a
    /// record in the wrong state. [`join_pending_teardowns`] joins
    /// every handle pushed here exactly once, right before the process
    /// decides its exit code.
    static PENDING_TEARDOWNS: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>> =
        std::sync::Mutex::new(Vec::new());

    fn register_pending_teardown(handle: tokio::task::JoinHandle<()>) {
        PENDING_TEARDOWNS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(handle);
    }

    /// `electricity-cli`'s own `main`, on the same runtime the run
    /// itself used, after that run's `block_on` has already returned
    /// and before the process's exit code is decided -- so a cancelled
    /// run's exit 130 is never returned while one of these is still
    /// signalling a group or rewriting a record, matching cof's own
    /// cancellation path running the same cleanup synchronously before
    /// the exception that triggered it finishes propagating.
    pub(super) async fn join_pending_teardowns() {
        let handles: Vec<_> = {
            let mut guard = PENDING_TEARDOWNS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::take(&mut *guard)
        };
        for handle in handles {
            let _ = handle.await;
        }
    }

    /// A safety net against this whole `start` future being dropped
    /// outright (`electricity_vm::exec::tool`'s own `select!` races
    /// dispatch against the run's cancellation and drops the loser) --
    /// mirrors `process.rs`'s own `KillGroupOnDrop`. Armed only while a
    /// `start` call has a group of its own still to answer for.
    struct TeardownOnCancel {
        armed: bool,
        pgid: u32,
        dir: PathBuf,
        name: String,
        grace: Duration,
    }

    impl Drop for TeardownOnCancel {
        fn drop(&mut self) {
            if !self.armed {
                return;
            }
            // Spawned, never run synchronously inside `Drop` itself --
            // `Drop` cannot `.await`, and a genuinely *blocking*
            // `flock(LOCK_EX)` here would block electricity's own
            // single runtime thread for as long as another branch (a
            // `stop` holding the very same state directory's lock
            // across its own `grace_ms`, as `ServicePlugin._stop` does)
            // takes to finish -- and that branch can never make
            // progress while this thread is blocked, so the two
            // deadlock for good (issue #721's own follow-up: a `start`
            // still armed and a `stop` holding the lock through its
            // grace, both alive when the run is cancelled, hung every
            // time). Spawning instead lets the runtime keep polling
            // every other branch while this waits for the lock, same
            // as [`stop_internal`]'s own locked section; both are
            // tracked in [`PENDING_TEARDOWNS`], which
            // `join_pending_teardowns` joins before the process exits,
            // so a cancelled run never exits while one of these is
            // still signalling a group or rewriting a record -- same
            // guarantee cof's own synchronous cleanup gives, reached a
            // different way.
            let dir = self.dir.clone();
            let name = self.name.clone();
            let pgid = self.pgid;
            let grace = self.grace;
            register_pending_teardown(tokio::spawn(async move {
                cancel_teardown(dir, name, pgid, grace).await;
            }));
        }
    }

    /// [`TeardownOnCancel`]'s own cleanup (see its `Drop` impl's own
    /// comment for why this is spawned rather than run inline). Same
    /// lock, same ownership re-check (a restart of this exact name,
    /// racing this very call's own `wait_ready`, must never have *its*
    /// group torn down just because this call's group shared the old
    /// name), and the same graceful `SIGTERM`-then-`grace_ms`-then-
    /// `SIGKILL` cof's own cancellation path gives a `start` it is
    /// tearing down -- and, like `ServicePlugin._stop`, the record is
    /// dropped only once the group is confirmed gone, not before the
    /// signal is even sent.
    async fn cancel_teardown(dir: PathBuf, name: String, pgid: u32, grace: Duration) {
        let Ok(lock) = LockGuard::acquire(&dir).await else {
            return;
        };
        let Ok((record, _)) = live_record_nb(dir.clone(), name.clone()).await else {
            return;
        };
        let Some(rec) = record else {
            return;
        };
        let actual = get(&rec, "pgid").and_then(value_as_i64).unwrap_or(-1) as u32;
        if actual != pgid {
            return;
        }
        let (gone, _sent) = terminate_group(pgid, grace).await;
        if gone {
            drop_record(&dir, &name);
        }
        drop(lock);
    }

    /// `core/cancellation.py::_terminate_group`.
    async fn terminate_group(pgid: u32, grace: Duration) -> (bool, Option<String>) {
        let mut sent: Option<String> = None;
        for (sig, wait) in [
            (libc::SIGTERM, grace),
            (libc::SIGKILL, Duration::from_secs_f64(SETTLE_SECONDS)),
        ] {
            if !group_exists_nb(pgid).await {
                return (true, sent);
            }
            let ret = unsafe { libc::killpg(pgid as libc::pid_t, sig) };
            if ret != 0 {
                let errno = io::Error::last_os_error().raw_os_error();
                if errno == Some(libc::ESRCH) {
                    return (true, sent);
                }
                // `EPERM`: only zombies left (macOS), or not ours.
                return (!group_exists_nb(pgid).await, sent);
            }
            sent = Some(
                if sig == libc::SIGTERM {
                    "SIGTERM"
                } else {
                    "SIGKILL"
                }
                .to_string(),
            );
            let deadline = Instant::now() + wait;
            loop {
                if !group_exists_nb(pgid).await {
                    return (true, sent);
                }
                if Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(POLL_MS)).await;
            }
        }
        (!group_exists_nb(pgid).await, sent)
    }

    /// `ServicePlugin._wait_ready`. *exit_rx* is `Some` only for a
    /// service this very call started (a oneshot the leader's reaper
    /// thread fires on exit, carrying `proc.returncode`'s own
    /// convention); `None` for a service an earlier call started, which
    /// polls [`group_exists`] instead -- same split as cof's own
    /// *proc* being `None`/not.
    async fn wait_ready(
        pgid: u32,
        mut exit_rx: Option<tokio::sync::oneshot::Receiver<i32>>,
        ready: Option<&ReadySpec>,
        timeout: Duration,
    ) -> Option<String> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let exited: Option<Option<i32>> = match exit_rx.as_mut() {
                Some(rx) => match rx.try_recv() {
                    Ok(code) => Some(Some(code)),
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed) => Some(None),
                },
                None => {
                    if !group_exists_nb(pgid).await {
                        Some(None)
                    } else {
                        None
                    }
                }
            };
            if let Some(code) = exited {
                return Some(match code {
                    Some(c) => format!("exited with code {c} before it was ready"),
                    None => "exited before it was ready".to_string(),
                });
            }
            match ready {
                None => return None,
                Some(spec) => {
                    if probe_ready(spec).await {
                        return None;
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return Some(format!(
                            "not ready ({}) within {}s",
                            ready_target_text(spec),
                            format_g(timeout.as_secs_f64())
                        ));
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(POLL_MS * 2)).await;
        }
    }

    // -- the tool -----------------------------------------------------------

    pub(super) async fn execute(
        plugin: &ServicePlugin,
        params: Value,
        call: &ToolCall<'_>,
    ) -> Result<ToolResult, ToolError> {
        let dict = params.as_dict();
        let action = py_strip(&falsy_or_default(dict.and_then(|d| get(d, "action"))).py_str())
            .to_lowercase();
        if !ACTIONS.contains(&action.as_str()) {
            return Err(value_error(format!(
                "service: params['action'] must be one of {}, got {}.",
                ACTIONS.join(", "),
                repr_str(&action)
            )));
        }
        let name =
            py_strip(&falsy_or_default(dict.and_then(|d| get(d, "name"))).py_str()).to_string();
        if !valid_name(&name) {
            return Err(value_error(format!(
                "service: params['name'] is required: letters, digits, '_', '.' and '-' \
                 (starting with a letter or digit), got {}.",
                repr_str(&name)
            )));
        }
        let empty = Dict::new();
        let params_dict = dict.unwrap_or(&empty);
        match action.as_str() {
            "start" => start(plugin, &name, params_dict, call).await,
            "stop" => {
                let grace_ms = parse_ms(params_dict, "grace_ms", DEFAULT_GRACE_MS)?;
                stop_internal(&plugin.dir(), &name, Duration::from_millis(grace_ms), None).await
            }
            _ => status(&plugin.dir(), &name).await,
        }
    }

    async fn start(
        plugin: &ServicePlugin,
        name: &str,
        params: &Dict,
        call: &ToolCall<'_>,
    ) -> Result<ToolResult, ToolError> {
        let shell = as_bool_param(get(params, "shell"));
        let argv: Vec<String> = if shell {
            match get(params, "command") {
                Some(Value::Str(s)) if !py_strip(s).is_empty() => {
                    vec!["/bin/sh".to_string(), "-c".to_string(), s.clone()]
                }
                _ => {
                    return Err(value_error(
                        "service: with shell: true, params['command'] must be a string.",
                    ));
                }
            }
        } else {
            match get(params, "command") {
                Some(Value::List(items))
                    if !items.is_empty()
                        && items.iter().all(|v| {
                            matches!(
                                v,
                                Value::Str(_) | Value::Int(_) | Value::Float(_) | Value::Bool(_)
                            )
                        }) =>
                {
                    items.iter().map(Value::py_str).collect()
                }
                _ => {
                    return Err(value_error(
                        "service: params['command'] must be a non-empty argv list \
                         (or a string with shell: true).",
                    ));
                }
            }
        };
        if argv.iter().any(|a| a.contains('\0')) {
            return Err(value_error(
                "service: params['command'] contains a null byte.",
            ));
        }

        let raw_ports: Vec<Value> = match get(params, "ports") {
            None | Some(Value::None) => vec![],
            Some(Value::Str(s)) if s.is_empty() => vec![],
            Some(Value::List(items)) => items.clone(),
            Some(other) => vec![other.clone()],
        };
        let mut ports: Vec<i64> = raw_ports
            .iter()
            .map(|p| parse_port_value(p, "params['ports'] entry"))
            .collect::<Result<_, _>>()?;
        let ready = parse_ready(get(params, "ready"))?;
        if let Some(rp) = ready.as_ref().and_then(ready_port) {
            if !ports.contains(&rp) {
                ports.push(rp);
            }
        }
        let ready_timeout_ms = parse_ms(params, "ready_timeout_ms", DEFAULT_READY_TIMEOUT_MS)?;
        let ready_timeout = Duration::from_millis(ready_timeout_ms)
            .min(Duration::from_secs(call.timeout_seconds as u64));
        let grace_ms = parse_ms(params, "grace_ms", DEFAULT_GRACE_MS)?;
        let grace = Duration::from_millis(grace_ms);

        let env_param = match get(params, "env") {
            Some(v) if is_truthy(v) => v.clone(),
            _ => Value::Dict(Dict::new()),
        };
        let overrides = match &env_param {
            Value::Dict(d) => d.clone(),
            _ => return Err(value_error("service: params['env'] must be a mapping.")),
        };
        // `vars_os`, not `vars`: the latter panics on a non-UTF-8
        // environment variable, which `os.environ` (Python surrogate-
        // escapes it) never does.
        let mut env: HashMap<std::ffi::OsString, std::ffi::OsString> =
            std::env::vars_os().collect();
        for (k, v) in overrides.iter() {
            env.insert(
                std::ffi::OsString::from(k.py_str()),
                std::ffi::OsString::from(v.py_str()),
            );
        }

        let cwd: PathBuf = match get(params, "cwd") {
            Some(v) if is_truthy(v) => resolve_path(&expand_user(&v.py_str())),
            _ => std::env::current_dir().map_err(|e| {
                runtime_error(format!("service: cannot read the working directory: {e}"))
            })?,
        };
        if !cwd.is_dir() {
            return Err(value_error(format!(
                "service: params['cwd'] {} is not a directory.",
                repr_str(&cwd.to_string_lossy())
            )));
        }

        if call.token.is_set() {
            return Err(ToolError::message("cancelled"));
        }

        let state_dir = plugin.dir();
        let lock = LockGuard::acquire(&state_dir)
            .await
            .map_err(|e| runtime_error(format!("service: cannot lock the state directory: {e}")))?;

        let log_path = match get(params, "log") {
            Some(v) if is_truthy(v) => resolve_path(&expand_user(&v.py_str())),
            _ => state_dir.join(format!("{name}.log")),
        };

        let (existing, _) = live_record_nb(state_dir.clone(), name.to_string()).await?;
        let started: bool;
        let record: Dict;
        let mut exit_rx: Option<tokio::sync::oneshot::Receiver<i32>> = None;
        let log_offset: u64;
        let mut guard: Option<TeardownOnCancel> = None;

        if let Some(existing_record) = existing {
            started = false;
            record = existing_record;
            log_offset = 0;
        } else {
            let claims = other_claims_nb(state_dir.clone(), name.to_string()).await?;
            let mut holders: Vec<Holder> = Vec::new();
            for port in &ports {
                if let Some(claim) = claims.get(port) {
                    holders.push(Holder {
                        port: *port,
                        pid: get(claim, "pgid").and_then(value_as_i64),
                        pgid: get(claim, "pgid").and_then(value_as_i64),
                        command: get(claim, "command").cloned(),
                        cwd: get(claim, "cwd")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        service: get(claim, "name")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    });
                } else {
                    holders.extend(port_holders(*port).await);
                }
            }
            if !holders.is_empty() {
                let detail = holders
                    .iter()
                    .map(format_holder)
                    .collect::<Vec<_>>()
                    .join("; ");
                let message = format!(
                    "service {}: not started \u{2014} {detail}. Nothing was signalled; \
                     stop that process yourself or pick another port.",
                    repr_str(name)
                );
                let mut raw = Dict::new();
                raw.insert(Value::from("action"), Value::from("start"));
                raw.insert(Value::from("name"), Value::from(name));
                raw.insert(
                    Value::from("port_holders"),
                    Value::List(holders.iter().map(|h| holder_to_value(h, None)).collect()),
                );
                return Ok(ToolResult {
                    value: Value::None,
                    raw: Value::Dict(raw),
                    stdout: None,
                    stderr: Some(message),
                    exit_code: None,
                    ok: false,
                    stdout_parsed: false,
                    tokens_sent: None,
                    tokens_received: None,
                });
            }

            if let Some(parent) = log_path.parent() {
                fs::create_dir_all(parent).map_err(|e| {
                    runtime_error(format!("service: cannot create the log directory: {e}"))
                })?;
            }
            let log_file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .map_err(|e| {
                    runtime_error(format!(
                        "service {}: cannot open the log: {e}",
                        repr_str(name)
                    ))
                })?;
            log_offset = log_file.metadata().map(|m| m.len()).unwrap_or(0);
            let log_file_err = log_file.try_clone().map_err(|e| {
                runtime_error(format!(
                    "service {}: cannot open the log: {e}",
                    repr_str(name)
                ))
            })?;

            let mut command = Command::new(&argv[0]);
            command.args(&argv[1..]);
            command.current_dir(&cwd);
            command.env_clear();
            command.envs(&env);
            command.stdin(Stdio::null());
            command.stdout(log_file);
            command.stderr(log_file_err);
            // `setsid()`, not merely `setpgid` (`process_group(0)`):
            // Python's own `start_new_session=True` detaches the child
            // from any controlling terminal entirely, so it can never
            // be stopped by `SIGTTOU`/`SIGTTIN` for touching the
            // inherited tty -- same reasoning as `process.rs`'s own
            // `new_session` (PR #467).
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut child = command.spawn().map_err(|e| {
                let (_, detail) = crate::process::describe_spawn_error(&e, &argv[0], None);
                runtime_error(format!(
                    "service {}: cannot start {}: {detail}",
                    repr_str(name),
                    repr_str(&argv[0])
                ))
            })?;
            let pid = child.id();
            let (tx, rx) = tokio::sync::oneshot::channel();
            std::thread::spawn(move || {
                if let Ok(status) = child.wait() {
                    let _ = tx.send(exit_code_of(&status));
                }
            });
            exit_rx = Some(rx);
            guard = Some(TeardownOnCancel {
                armed: true,
                pgid: pid,
                dir: state_dir.clone(),
                name: name.to_string(),
                grace,
            });

            let outcome: Result<Dict, ToolError> = async {
                let start_time = process_start_time_nb(pid).await?;
                let Some(start_time) = start_time else {
                    return Err(runtime_error(format!(
                        "pid {pid} vanished before it could be recorded"
                    )));
                };
                let boot = boot_time_nb().await?;
                let mut rec = Dict::new();
                rec.insert(Value::from("name"), Value::from(name));
                rec.insert(Value::from("pgid"), Value::from(pid as i64));
                rec.insert(Value::from("start_time"), Value::from(start_time));
                rec.insert(Value::from("boot_time"), Value::from(boot));
                rec.insert(
                    Value::from("ports"),
                    Value::List(ports.iter().map(|p| Value::from(*p)).collect()),
                );
                rec.insert(
                    Value::from("log"),
                    Value::from(log_path.to_string_lossy().into_owned()),
                );
                rec.insert(
                    Value::from("command"),
                    Value::List(argv.iter().map(|a| Value::from(a.as_str())).collect()),
                );
                rec.insert(
                    Value::from("cwd"),
                    Value::from(cwd.to_string_lossy().into_owned()),
                );
                rec.insert(Value::from("ready"), ready_to_value(&ready));
                rec.insert(Value::from("started_at"), Value::from(iso_now()));
                save_record(&state_dir, &rec)?;
                Ok(rec)
            }
            .await;
            match outcome {
                Ok(rec) => {
                    started = true;
                    record = rec;
                }
                Err(err) => {
                    terminate_group(pid, grace).await;
                    if let Some(g) = guard.as_mut() {
                        g.armed = false;
                    }
                    return Err(err);
                }
            }
        }
        drop(lock);

        let pgid = get(&record, "pgid")
            .and_then(value_as_i64)
            .expect("a live record always carries pgid") as u32;
        let effective_ready = if started {
            ready.clone()
        } else {
            get(&record, "ready").and_then(value_to_ready)
        };
        let outcome = wait_ready(pgid, exit_rx, effective_ready.as_ref(), ready_timeout).await;

        let ports_value = get(&record, "ports")
            .cloned()
            .unwrap_or(Value::List(vec![]));
        let log_value = get(&record, "log").cloned().unwrap_or(Value::None);
        let started_at_value = get(&record, "started_at").cloned().unwrap_or(Value::None);

        if outcome.is_none() {
            if let Some(g) = guard.as_mut() {
                g.armed = false;
            }
            let mut value = Dict::new();
            value.insert(Value::from("name"), Value::from(name));
            value.insert(Value::from("running"), Value::from(true));
            value.insert(Value::from("ready"), Value::from(true));
            value.insert(Value::from("already_running"), Value::from(!started));
            value.insert(Value::from("pgid"), Value::from(pgid as i64));
            value.insert(Value::from("ports"), ports_value);
            value.insert(Value::from("log"), log_value);
            value.insert(Value::from("started_at"), started_at_value);
            let mut raw = Dict::new();
            raw.insert(Value::from("action"), Value::from("start"));
            for (k, v) in value.iter() {
                raw.insert(k.clone(), v.clone());
            }
            return Ok(ToolResult::new(Value::Dict(value), Value::Dict(raw)));
        }

        let error_text = outcome.unwrap();
        let tail = log_tail(Path::new(&log_value.py_str()), log_offset);
        let mut running = true;
        if started {
            let _ = stop_internal(&state_dir, name, grace, Some(pgid)).await;
            running = false;
        }
        if let Some(g) = guard.as_mut() {
            g.armed = false;
        }

        let mut value = Dict::new();
        value.insert(Value::from("name"), Value::from(name));
        value.insert(Value::from("running"), Value::from(running));
        value.insert(Value::from("ready"), Value::from(false));
        value.insert(Value::from("already_running"), Value::from(!started));
        value.insert(Value::from("pgid"), Value::from(pgid as i64));
        value.insert(Value::from("ports"), ports_value);
        value.insert(Value::from("log"), log_value.clone());
        value.insert(Value::from("started_at"), started_at_value);

        let message = if tail.is_empty() {
            format!("service {}: {error_text}", repr_str(name))
        } else {
            format!(
                "service {}: {error_text}\n--- log tail ({}) ---\n{tail}",
                repr_str(name),
                log_value.py_str()
            )
        };
        let mut raw = Dict::new();
        raw.insert(Value::from("action"), Value::from("start"));
        for (k, v) in value.iter() {
            raw.insert(k.clone(), v.clone());
        }
        raw.insert(Value::from("error"), Value::from(error_text.as_str()));
        raw.insert(Value::from("log_tail"), Value::from(tail.as_str()));

        Ok(ToolResult {
            value: Value::None,
            raw: Value::Dict(raw),
            stdout: None,
            stderr: Some(message),
            exit_code: None,
            ok: false,
            stdout_parsed: false,
            tokens_sent: None,
            tokens_received: None,
        })
    }

    fn stopped_false_result(name: &str, stale: Option<String>) -> ToolResult {
        let mut value = Dict::new();
        value.insert(Value::from("name"), Value::from(name));
        value.insert(Value::from("stopped"), Value::from(false));
        value.insert(Value::from("running"), Value::from(false));
        value.insert(Value::from("pgid"), Value::None);
        value.insert(Value::from("signal"), Value::None);
        value.insert(Value::from("ports_free"), Value::from(true));
        value.insert(Value::from("port_holders"), Value::List(vec![]));
        value.insert(
            Value::from("stale_record"),
            stale
                .map(|s| Value::from(s.as_str()))
                .unwrap_or(Value::None),
        );
        let mut raw = Dict::new();
        raw.insert(Value::from("action"), Value::from("stop"));
        for (k, v) in value.iter() {
            raw.insert(k.clone(), v.clone());
        }
        ToolResult::new(Value::Dict(value), Value::Dict(raw))
    }

    struct Stopped {
        pgid: u32,
        sent: Option<String>,
        ports: Vec<i64>,
    }

    /// [`stop_internal`]'s own outcome, once the lock is released:
    /// either an early result the lock-held checks already settled
    /// (nothing to stop, or the group survived `SIGKILL`), or the
    /// group this call actually stopped.
    enum StopLocked {
        Early(ToolResult),
        Stopped(Stopped),
    }

    /// `ServicePlugin._stop`'s own `with self._locked(): ...` block --
    /// acquires the state directory's lock, confirms ownership, signals
    /// the group and (once it's confirmed gone) removes the record.
    /// Spawned by [`stop_internal`] rather than awaited inline, and
    /// tracked in [`PENDING_TEARDOWNS`]: cof's own equivalent block has
    /// no cancellation checkpoint of its own, so once it starts it
    /// always finishes, holding the lock the whole time -- here, a
    /// cancelled run dropping `stop_internal`'s own await (`exec::
    /// tool`'s `select!` races every dispatch against the run's
    /// cancellation and drops the loser) must not stop this from
    /// finishing the same way, or the group it already sent `SIGTERM`
    /// to is never followed up with `SIGKILL`, and its record outlives
    /// it.
    async fn stop_locked(
        dir: PathBuf,
        name: String,
        grace: Duration,
        expected_pgid: Option<u32>,
    ) -> Result<StopLocked, ToolError> {
        let lock = LockGuard::acquire(&dir).await.map_err(|e| {
            runtime_error(format!("service: cannot lock the state directory: {e}"))
        })?;
        let (mut record, stale) = live_record_nb(dir.clone(), name.clone()).await?;
        if let Some(rec) = &record {
            if let Some(expected) = expected_pgid {
                let actual = get(rec, "pgid").and_then(value_as_i64).unwrap_or(-1) as u32;
                if actual != expected {
                    record = None;
                }
            }
        }
        let Some(rec) = record else {
            return Ok(StopLocked::Early(stopped_false_result(&name, stale)));
        };
        let pgid = get(&rec, "pgid").and_then(value_as_i64).unwrap_or(0) as u32;
        let (gone, sent) = terminate_group(pgid, grace).await;
        if !gone {
            let message = if sent.as_deref() == Some("SIGKILL") {
                format!(
                    "service {}: process group {pgid} survived SIGKILL.",
                    repr_str(&name)
                )
            } else {
                format!(
                    "service {}: not permitted to signal process group {pgid}.",
                    repr_str(&name)
                )
            };
            let mut raw = Dict::new();
            raw.insert(Value::from("action"), Value::from("stop"));
            raw.insert(Value::from("name"), Value::from(name.as_str()));
            raw.insert(Value::from("pgid"), Value::from(pgid as i64));
            raw.insert(
                Value::from("signal"),
                sent.map(|s| Value::from(s.as_str())).unwrap_or(Value::None),
            );
            return Ok(StopLocked::Early(ToolResult {
                value: Value::None,
                raw: Value::Dict(raw),
                stdout: None,
                stderr: Some(message),
                exit_code: None,
                ok: false,
                stdout_parsed: false,
                tokens_sent: None,
                tokens_received: None,
            }));
        }
        drop_record(&dir, &name);
        let ports: Vec<i64> = get(&rec, "ports")
            .and_then(Value::as_list)
            .map(|l| l.iter().filter_map(value_as_i64).collect())
            .unwrap_or_default();
        drop(lock);
        Ok(StopLocked::Stopped(Stopped { pgid, sent, ports }))
    }

    /// `ServicePlugin._stop`. With *expected_pgid* (a start cleaning up
    /// after itself), only when the record still names that group.
    async fn stop_internal(
        dir: &Path,
        name: &str,
        grace: Duration,
        expected_pgid: Option<u32>,
    ) -> Result<ToolResult, ToolError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn({
            let dir = dir.to_path_buf();
            let name = name.to_string();
            async move {
                let outcome = stop_locked(dir, name, grace, expected_pgid).await;
                let _ = tx.send(outcome);
            }
        });
        register_pending_teardown(handle);
        let locked = match rx.await {
            Ok(outcome) => outcome?,
            Err(_) => {
                return Err(runtime_error(
                    "service: internal error: the stop task ended early".to_string(),
                ));
            }
        };
        let stopped = match locked {
            StopLocked::Early(result) => return Ok(result),
            StopLocked::Stopped(stopped) => stopped,
        };

        let deadline = Instant::now() + Duration::from_secs_f64(SETTLE_SECONDS);
        let mut holders: Vec<Holder>;
        loop {
            holders = Vec::new();
            for port in &stopped.ports {
                holders.extend(port_holders(*port).await);
            }
            if holders.is_empty() || Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(POLL_MS)).await;
        }

        let mut value = Dict::new();
        value.insert(Value::from("name"), Value::from(name));
        value.insert(Value::from("stopped"), Value::from(true));
        value.insert(Value::from("running"), Value::from(false));
        value.insert(Value::from("pgid"), Value::from(stopped.pgid as i64));
        value.insert(
            Value::from("signal"),
            stopped
                .sent
                .clone()
                .map(|s| Value::from(s.as_str()))
                .unwrap_or(Value::None),
        );
        value.insert(Value::from("ports_free"), Value::from(holders.is_empty()));
        value.insert(
            Value::from("port_holders"),
            Value::List(holders.iter().map(|h| holder_to_value(h, None)).collect()),
        );
        value.insert(Value::from("stale_record"), Value::None);
        let mut raw = Dict::new();
        raw.insert(Value::from("action"), Value::from("stop"));
        for (k, v) in value.iter() {
            raw.insert(k.clone(), v.clone());
        }
        Ok(ToolResult::new(Value::Dict(value), Value::Dict(raw)))
    }

    async fn status(dir: &Path, name: &str) -> Result<ToolResult, ToolError> {
        let (record, stale) = {
            let lock = LockGuard::acquire(dir).await.map_err(|e| {
                runtime_error(format!("service: cannot lock the state directory: {e}"))
            })?;
            let result = live_record_nb(dir.to_path_buf(), name.to_string()).await?;
            drop(lock);
            result
        };
        let ports: Vec<i64> = record
            .as_ref()
            .and_then(|r| get(r, "ports"))
            .and_then(Value::as_list)
            .map(|l| l.iter().filter_map(value_as_i64).collect())
            .unwrap_or_default();
        let mut holders: Vec<Holder> = Vec::new();
        for port in &ports {
            holders.extend(port_holders(*port).await);
        }
        let pgid = record
            .as_ref()
            .and_then(|r| get(r, "pgid"))
            .and_then(value_as_i64);

        let ready_spec = record
            .as_ref()
            .and_then(|r| get(r, "ready"))
            .and_then(value_to_ready);
        let ready_value = if record.is_some() {
            match &ready_spec {
                Some(spec) => Value::from(probe_ready(spec).await),
                None => Value::None,
            }
        } else {
            Value::from(false)
        };

        let mut value = Dict::new();
        value.insert(Value::from("name"), Value::from(name));
        value.insert(Value::from("running"), Value::from(record.is_some()));
        value.insert(Value::from("ready"), ready_value);
        value.insert(
            Value::from("pgid"),
            pgid.map(Value::from).unwrap_or(Value::None),
        );
        value.insert(
            Value::from("ports"),
            Value::List(ports.iter().map(|p| Value::from(*p)).collect()),
        );
        value.insert(
            Value::from("log"),
            record
                .as_ref()
                .and_then(|r| get(r, "log"))
                .cloned()
                .unwrap_or(Value::None),
        );
        value.insert(
            Value::from("started_at"),
            record
                .as_ref()
                .and_then(|r| get(r, "started_at"))
                .cloned()
                .unwrap_or(Value::None),
        );
        value.insert(
            Value::from("port_holders"),
            Value::List(
                holders
                    .iter()
                    .map(|h| holder_to_value(h, Some(pgid.is_some() && h.pgid == pgid)))
                    .collect(),
            ),
        );
        value.insert(
            Value::from("stale_record"),
            stale
                .map(|s| Value::from(s.as_str()))
                .unwrap_or(Value::None),
        );

        let mut raw = Dict::new();
        raw.insert(Value::from("action"), Value::from("status"));
        for (k, v) in value.iter() {
            raw.insert(k.clone(), v.clone());
        }
        Ok(ToolResult::new(Value::Dict(value), Value::Dict(raw)))
    }

    pub(super) fn check() -> CheckResult {
        if !is_linux_proc() && crate::process::which("ps").is_none() {
            return CheckResult {
                ok: false,
                missing: vec!["binary:ps".to_string()],
                message: None,
            };
        }
        if crate::process::which("lsof").is_none() {
            return CheckResult {
                ok: true,
                missing: Vec::new(),
                message: Some(
                    "lsof not found: a port held by another process is still detected, \
                     but without its pid, command and cwd."
                        .to_string(),
                ),
            };
        }
        CheckResult {
            ok: true,
            missing: Vec::new(),
            message: None,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::ToolPlugin;
        use electricity_value::CancellationToken;

        static NONE: Value = Value::None;

        fn call(token: &CancellationToken) -> ToolCall<'_> {
            ToolCall {
                timeout_seconds: 30,
                config: &NONE,
                token,
                armed: false,
                secret_headers: &[],
                scrub: None,
                on_live: None,
                limited: false,
                confine_to: None,
            }
        }

        fn params(entries: Vec<(&str, Value)>) -> Value {
            let mut dict = Dict::new();
            for (k, v) in entries {
                dict.insert(Value::from(k), v);
            }
            Value::Dict(dict)
        }

        fn plugin(dir: &Path) -> ServicePlugin {
            ServicePlugin {
                state_dir: Some(dir.to_string_lossy().into_owned()),
            }
        }

        #[test]
        fn valid_name_matches_the_python_regex() {
            assert!(valid_name("web"));
            assert!(valid_name("web-1.local_test"));
            assert!(valid_name(&"a".repeat(100)));
            assert!(!valid_name(""));
            assert!(!valid_name(&"a".repeat(101)));
            assert!(!valid_name("_leading-underscore"));
            assert!(!valid_name("has space"));
        }

        #[tokio::test]
        async fn an_unknown_action_is_rejected() {
            let dir = tempdir();
            let token = CancellationToken::new();
            let plugin = plugin(dir.path());
            let err = plugin
                .execute(
                    params(vec![
                        ("action", Value::from("bogus")),
                        ("name", Value::from("x")),
                    ]),
                    &call(&token),
                )
                .await
                .unwrap_err();
            assert_eq!(
                err.message,
                "service: params['action'] must be one of start, stop, status, got 'bogus'."
            );
            assert_eq!(err.py_class, PyExc::ValueError);
        }

        #[tokio::test]
        async fn an_invalid_name_is_rejected() {
            let dir = tempdir();
            let token = CancellationToken::new();
            let plugin = plugin(dir.path());
            let err = plugin
                .execute(
                    params(vec![
                        ("action", Value::from("status")),
                        ("name", Value::from("a b")),
                    ]),
                    &call(&token),
                )
                .await
                .unwrap_err();
            assert!(err.message.contains("params['name'] is required"));
            assert_eq!(err.py_class, PyExc::ValueError);
        }

        #[tokio::test]
        async fn status_of_an_unknown_name_is_not_running() {
            let dir = tempdir();
            let token = CancellationToken::new();
            let plugin = plugin(dir.path());
            let result = plugin
                .execute(
                    params(vec![
                        ("action", Value::from("status")),
                        ("name", Value::from("nope")),
                    ]),
                    &call(&token),
                )
                .await
                .unwrap();
            let value = result.value.as_dict().unwrap();
            assert_eq!(get(value, "running"), Some(&Value::from(false)));
            assert_eq!(get(value, "ready"), Some(&Value::from(false)));
        }

        #[tokio::test]
        async fn stop_of_an_unknown_name_is_idempotent() {
            let dir = tempdir();
            let token = CancellationToken::new();
            let plugin = plugin(dir.path());
            let result = plugin
                .execute(
                    params(vec![
                        ("action", Value::from("stop")),
                        ("name", Value::from("nope")),
                    ]),
                    &call(&token),
                )
                .await
                .unwrap();
            let value = result.value.as_dict().unwrap();
            assert_eq!(get(value, "stopped"), Some(&Value::from(false)));
            assert!(result.ok);
        }

        #[tokio::test]
        async fn start_a_command_without_a_ready_check_and_then_stop_it() {
            let dir = tempdir();
            let token = CancellationToken::new();
            let plugin = plugin(dir.path());
            let name = format!("svc-{}", std::process::id());
            let start_result = plugin
                .execute(
                    params(vec![
                        ("action", Value::from("start")),
                        ("name", Value::from(name.as_str())),
                        (
                            "command",
                            Value::List(vec![Value::from("sleep"), Value::from("30")]),
                        ),
                    ]),
                    &call(&token),
                )
                .await
                .unwrap();
            assert!(start_result.ok);
            let value = start_result.value.as_dict().unwrap();
            assert_eq!(get(value, "running"), Some(&Value::from(true)));
            assert_eq!(get(value, "already_running"), Some(&Value::from(false)));
            let pgid = value_as_i64(get(value, "pgid").unwrap()).unwrap() as u32;
            assert!(group_exists(pgid));

            // starting again is idempotent.
            let again = plugin
                .execute(
                    params(vec![
                        ("action", Value::from("start")),
                        ("name", Value::from(name.as_str())),
                        (
                            "command",
                            Value::List(vec![Value::from("sleep"), Value::from("30")]),
                        ),
                    ]),
                    &call(&token),
                )
                .await
                .unwrap();
            let again_value = again.value.as_dict().unwrap();
            assert_eq!(
                get(again_value, "already_running"),
                Some(&Value::from(true))
            );

            let stop_result = plugin
                .execute(
                    params(vec![
                        ("action", Value::from("stop")),
                        ("name", Value::from(name.as_str())),
                    ]),
                    &call(&token),
                )
                .await
                .unwrap();
            let stop_value = stop_result.value.as_dict().unwrap();
            assert_eq!(get(stop_value, "stopped"), Some(&Value::from(true)));
            assert!(!group_exists(pgid));
        }

        #[tokio::test]
        async fn a_ready_timeout_tears_the_group_down() {
            let dir = tempdir();
            let token = CancellationToken::new();
            let plugin = plugin(dir.path());
            let name = format!("svc-timeout-{}", std::process::id());
            let result = plugin
                .execute(
                    params(vec![
                        ("action", Value::from("start")),
                        ("name", Value::from(name.as_str())),
                        (
                            "command",
                            Value::List(vec![Value::from("sleep"), Value::from("30")]),
                        ),
                        ("ready", Value::from(1)),
                        ("ready_timeout_ms", Value::from(300)),
                    ]),
                    &call(&token),
                )
                .await
                .unwrap();
            assert!(!result.ok);
            assert_eq!(result.value, Value::None);
            let raw = result.raw.as_dict().unwrap();
            assert_eq!(get(raw, "running"), Some(&Value::from(false)));
            let err = result.stderr.unwrap();
            assert!(err.contains("not ready"), "{err}");
        }

        #[tokio::test]
        async fn a_port_held_by_another_process_is_reported_and_never_signalled() {
            let dir = tempdir();
            let token = CancellationToken::new();
            let plugin = plugin(dir.path());
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let name = format!("svc-port-{}", std::process::id());
            let result = plugin
                .execute(
                    params(vec![
                        ("action", Value::from("start")),
                        ("name", Value::from(name.as_str())),
                        (
                            "command",
                            Value::List(vec![Value::from("sleep"), Value::from("5")]),
                        ),
                        ("ports", Value::List(vec![Value::from(port as i64)])),
                    ]),
                    &call(&token),
                )
                .await
                .unwrap();
            drop(listener);
            assert!(!result.ok);
            assert!(result.stderr.unwrap().contains("not started"));
            let raw = result.raw.as_dict().unwrap();
            assert!(get(raw, "port_holders").is_some());
        }

        /// A `start` call's own future dropped outright while it is
        /// still waiting for `ready` (`electricity_vm::exec::tool`'s
        /// own cancellation race, reproduced directly here rather than
        /// through a whole run) tears the group down the same way a
        /// plain `stop` does: `SIGTERM` first -- observed through a
        /// trap the child itself installs -- not an unconditional
        /// `SIGKILL`, and the record is gone afterward.
        #[tokio::test]
        async fn a_cancelled_start_tears_the_group_down_gracefully() {
            let dir = tempdir();
            let token = CancellationToken::new();
            let plugin = plugin(dir.path());
            let name = format!("svc-cancel-{}", std::process::id());
            let marker = dir.path().join("sigterm-seen");
            let script = format!("trap 'touch {} ; exit 0' TERM; sleep 30", marker.display());
            let record_path = dir.path().join(format!("{name}.json"));
            let the_call = call(&token);

            // `Box::pin`, not `tokio::pin!`: the latter leaves this
            // variable a mere `Pin<&mut F>>` to a hidden stack slot, so
            // `drop`ping it drops only the reference, never running the
            // future's own drop glue (`TeardownOnCancel`'s `Drop` among
            // its captured locals) -- an owned `Pin<Box<F>>` is what
            // `electricity_vm`'s own dispatch drops in the real
            // cancellation race this test reproduces.
            let mut start_fut = Box::pin(plugin.execute(
                params(vec![
                    ("action", Value::from("start")),
                    ("name", Value::from(name.as_str())),
                    ("command", Value::from(script.as_str())),
                    ("shell", Value::from(true)),
                    // Nothing ever listens on this port, so `wait_ready`
                    // keeps polling until this test drops the future.
                    ("ready", Value::from(1)),
                    ("ready_timeout_ms", Value::from(30_000)),
                    ("grace_ms", Value::from(2_000)),
                ]),
                &the_call,
            ));

            let find_pgid = async {
                loop {
                    if let Ok(text) = fs::read_to_string(&record_path) {
                        if let Ok(v) = loads(&text) {
                            if let Some(d) = v.as_dict() {
                                if let Some(p) = get(d, "pgid").and_then(value_as_i64) {
                                    return p as u32;
                                }
                            }
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            };
            let pgid = tokio::select! {
                _ = &mut start_fut => panic!("start finished before a pgid could be observed"),
                p = find_pgid => p,
            };
            drop(start_fut); // the dispatch cancellation this guards against

            // The actual teardown is spawned off that drop, never run
            // synchronously (`TeardownOnCancel::drop`'s own comment) --
            // joining every such task is exactly what
            // `electricity-cli`'s own `main` does, on the same
            // runtime, before the process's exit code is decided.
            join_pending_teardowns().await;
            assert!(
                !group_exists(pgid),
                "the group should be gone after cancellation"
            );
            assert!(
                marker.exists(),
                "SIGTERM should have reached the child before it died"
            );
            assert!(
                !record_path.exists(),
                "the record should be dropped once torn down"
            );
        }

        /// Two `start`s, then two `stop`s, run in parallel on one
        /// `current_thread` runtime -- electricity's own real dispatch
        /// shape (every branch of a run polled from one
        /// `FuturesUnordered` on a single thread). A blocking `flock`
        /// acquire would deadlock this outright: branch A takes the
        /// lock and yields; branch B's own blocking acquire attempt
        /// then parks the only thread, so A can never resume to release
        /// it. A non-blocking, polled acquire lets both progress.
        #[tokio::test(flavor = "current_thread")]
        async fn parallel_start_and_stop_branches_do_not_deadlock() {
            let dir = tempdir();
            let token = CancellationToken::new();
            let plugin = plugin(dir.path());
            let pid = std::process::id();
            let name_a = format!("svc-par-a-{pid}");
            let name_b = format!("svc-par-b-{pid}");
            let the_call = call(&token);

            let start = |name: &str| {
                plugin.execute(
                    params(vec![
                        ("action", Value::from("start")),
                        ("name", Value::from(name)),
                        (
                            "command",
                            Value::List(vec![Value::from("sleep"), Value::from("30")]),
                        ),
                    ]),
                    &the_call,
                )
            };
            let (a, b) = tokio::join!(start(&name_a), start(&name_b));
            let a = a.unwrap();
            let b = b.unwrap();
            assert!(a.ok, "{a:?}");
            assert!(b.ok, "{b:?}");

            let stop = |name: &str| {
                plugin.execute(
                    params(vec![
                        ("action", Value::from("stop")),
                        ("name", Value::from(name)),
                    ]),
                    &the_call,
                )
            };
            let (a, b) = tokio::join!(stop(&name_a), stop(&name_b));
            let a = a.unwrap();
            let b = b.unwrap();
            assert_eq!(
                a.value.as_dict().and_then(|d| get(d, "stopped")),
                Some(&Value::from(true))
            );
            assert_eq!(
                b.value.as_dict().and_then(|d| get(d, "stopped")),
                Some(&Value::from(true))
            );
        }

        /// The deadlock issue #721's own follow-up found: a `start`
        /// still armed and waiting for `ready` (branch A), and a
        /// `stop` holding the state directory's lock through its own
        /// `grace_ms` because its target ignores `SIGTERM` (branch B,
        /// same as `ServicePlugin._stop`'s own `with self._locked():`),
        /// dropped together -- exactly what `electricity_vm::exec::
        /// tool`'s own `select!` does to every branch in flight when a
        /// run is cancelled. A synchronous, blocking
        /// `TeardownOnCancel::drop` would park this test's only thread
        /// right there (`flavor = "current_thread"`, matching
        /// electricity's own single-threaded dispatch) while B still
        /// holds the lock, so neither branch could ever finish; this
        /// hung 3 runs out of 3 before the fix.
        #[tokio::test(flavor = "current_thread")]
        async fn cancelling_a_waiting_start_and_a_locked_stop_together_never_hangs() {
            let dir = tempdir();
            let token = CancellationToken::new();
            let plugin = plugin(dir.path());
            let pid = std::process::id();
            let stub_name = format!("svc-deadlock-stub-{pid}");
            let waiting_name = format!("svc-deadlock-wait-{pid}");
            let the_call = call(&token);
            let ignored_marker = dir.path().join("ignored-term");

            // B's own target: ignores `SIGTERM` (keeps running, same as
            // the conformance fake's own `ignore-term` mode), so `stop`
            // holds the lock through the whole `grace_ms`.
            let stub_script = format!(
                "trap 'touch {}' TERM; while true; do sleep 1; done",
                ignored_marker.display()
            );
            let start_stub = plugin
                .execute(
                    params(vec![
                        ("action", Value::from("start")),
                        ("name", Value::from(stub_name.as_str())),
                        ("command", Value::from(stub_script.as_str())),
                        ("shell", Value::from(true)),
                    ]),
                    &the_call,
                )
                .await
                .unwrap();
            assert!(start_stub.ok, "{start_stub:?}");
            let stub_pgid = start_stub
                .value
                .as_dict()
                .and_then(|d| get(d, "pgid"))
                .and_then(value_as_i64)
                .expect("a successful start always reports its own pgid")
                as u32;
            let stub_record_path = dir.path().join(format!("{stub_name}.json"));

            // B: stops the stub, with a grace period generous enough
            // that A below can still be mid-`wait_ready` once both are
            // dropped.
            let mut stop_fut = Box::pin(plugin.execute(
                params(vec![
                    ("action", Value::from("stop")),
                    ("name", Value::from(stub_name.as_str())),
                    ("grace_ms", Value::from(5_000)),
                ]),
                &the_call,
            ));
            let wait_for_ignored = async {
                while !ignored_marker.exists() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            };
            tokio::select! {
                _ = &mut stop_fut => panic!("stop finished before SIGTERM was even ignored"),
                () = wait_for_ignored => {}
            }
            // B now holds the lock, somewhere in `terminate_group`'s
            // own `grace_ms` wait.

            // A: a start still waiting for `ready`, armed, never
            // satisfied.
            let waiting_record_path = dir.path().join(format!("{waiting_name}.json"));
            let mut start_fut = Box::pin(plugin.execute(
                params(vec![
                    ("action", Value::from("start")),
                    ("name", Value::from(waiting_name.as_str())),
                    ("command", Value::from("sleep 30")),
                    ("shell", Value::from(true)),
                    ("ready", Value::from(1)),
                    ("ready_timeout_ms", Value::from(30_000)),
                    ("grace_ms", Value::from(2_000)),
                ]),
                &the_call,
            ));
            let find_pgid = async {
                loop {
                    if let Ok(text) = fs::read_to_string(&waiting_record_path) {
                        if let Ok(v) = loads(&text) {
                            if let Some(d) = v.as_dict() {
                                if let Some(p) = get(d, "pgid").and_then(value_as_i64) {
                                    return p as u32;
                                }
                            }
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            };
            let waiting_pgid = tokio::select! {
                _ = &mut start_fut => panic!("start finished before a pgid could be observed"),
                p = find_pgid => p,
            };

            // The cancellation this guards against: both branches
            // dropped together, exactly as `exec::tool`'s own `select!`
            // drops every loser when a run is cancelled.
            drop(start_fut);
            drop(stop_fut);

            tokio::time::timeout(Duration::from_secs(10), join_pending_teardowns())
                .await
                .expect("join_pending_teardowns must not hang");

            assert!(
                !group_exists(waiting_pgid),
                "the waiting start's group should be gone"
            );
            assert!(
                !waiting_record_path.exists(),
                "the waiting start's record should be gone"
            );
            assert!(
                !group_exists(stub_pgid),
                "the stub's group should be gone"
            );
            assert!(
                !stub_record_path.exists(),
                "the stub's record should be gone"
            );
        }

        #[test]
        fn format_g_matches_pythons_general_float_format() {
            assert_eq!(format_g(60.0), "60");
            assert_eq!(format_g(0.5), "0.5");
            assert_eq!(format_g(5.0), "5");
            assert_eq!(format_g(90.5), "90.5");
            assert_eq!(format_g(300.0), "300");
            assert_eq!(format_g(100000.0), "100000");
            assert_eq!(format_g(999999.0), "999999");
            assert_eq!(format_g(1000000.0), "1e+06");
            assert_eq!(format_g(1234567.0), "1.23457e+06");
            assert_eq!(format_g(1234.567), "1234.57");
            assert_eq!(format_g(0.0001), "0.0001");
            assert_eq!(format_g(0.00001), "1e-05");
        }

        #[tokio::test]
        async fn fetch_status_follows_a_redirect_to_its_final_response() {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            tokio::spawn(async move {
                for _ in 0..2 {
                    let (mut sock, _) = listener.accept().await.unwrap();
                    let mut buf = [0u8; 1024];
                    let _ = sock.read(&mut buf).await;
                    let request = String::from_utf8_lossy(&buf).to_string();
                    let response = if request.starts_with("GET /old") {
                        format!(
                            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{port}/new\r\n\r\n"
                        )
                    } else {
                        "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_string()
                    };
                    let _ = sock.write_all(response.as_bytes()).await;
                }
            });
            let parsed = parse_url(&format!("http://127.0.0.1:{port}/old")).unwrap();
            let status = fetch_status(&parsed).await;
            assert_eq!(status, Some(200));
        }

        fn tempdir() -> tempfile_shim::TempDir {
            tempfile_shim::TempDir::new()
        }

        /// A minimal, local `tempdir()` -- this crate has no
        /// `tempfile` dev-dependency of its own yet, and this test
        /// module's own needs (one fresh, self-removing directory per
        /// test) don't justify adding one.
        mod tempfile_shim {
            use std::path::{Path, PathBuf};

            pub struct TempDir(PathBuf);

            impl TempDir {
                pub fn new() -> Self {
                    let mut dir = std::env::temp_dir();
                    let unique = format!(
                        "electricity-service-test-{}-{}",
                        std::process::id(),
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_nanos()
                    );
                    dir.push(unique);
                    std::fs::create_dir_all(&dir).unwrap();
                    TempDir(dir)
                }

                pub fn path(&self) -> &Path {
                    &self.0
                }
            }

            impl Drop for TempDir {
                fn drop(&mut self) {
                    let _ = std::fs::remove_dir_all(&self.0);
                }
            }
        }
    }
}
