### Tools, Adapters, Runtime Plugins and the External Plugin Protocol — electricity v1

Source of truth: Circuitry, git commit `82239cd`.
All citations are `path:line` against that commit. This document specifies what
`electricity` (the Rust runner) must implement natively, and the input/output contract for
the external-plugin bridge that lets Circuitry's existing Python tool plugins (and
`python_eval`) keep running unchanged under the Rust host, under the project's decision that
"Everything else... runs as an external plugin over stdin/stdout."

Terminology: a **tool plugin** is what a `provider:` tool effect calls (`circuitry/plugins/*`).
A **model adapter** is what a `prompt` effect calls (`circuitry/adapters/*`). A **runtime
plugin** is a lifecycle hook (`circuitry/runtime_plugins/*`), distinct from a **persistence
backend** (`circuitry/core/store/*`), which is what `--out`/`--resume`/`--live-state` actually
read and write.

---

## 1. The tool plugin contract

### 1.1 Interface

A tool plugin is a `Protocol` with three members (`src/circuitry/plugins/base.py:107-114`):

```python
class ToolPlugin(Protocol):
    @property
    def name(self) -> str: ...
    def execute(self, *, params: dict[str, Any], timeout_seconds: int = 300) -> ToolResult: ...
    def check(self) -> CheckResult: ...
```

`check()` is optional in practice: a missing or non-callable `check` defaults to
`CheckResult(ok=True, missing=[])` (`src/circuitry/preflight.py:40-66`, `call_check`). A
`check()` that raises, or returns something other than a `CheckResult`, is also folded into a
non-crashing `CheckResult(ok=False, ...)` — the preflight walker must never itself crash on a
misbehaving plugin.

`CheckResult` (`src/circuitry/preflight.py:21-35`) is `{ok: bool, missing: list[str], message:
str | None}`. `missing` entries use a fixed grammar the CLI can render actionable next steps
from: `env:VAR_NAME`, `binary:name`, `library:dotted.path`, `host:url`.

### 1.2 `ToolResult` fields

`src/circuitry/plugins/base.py:69-83`:

```python
@dataclass(frozen=True)
class ToolResult:
    value: Any
    raw: dict[str, Any]
    stdout: str | None = None
    stderr: str | None = None
    exit_code: int | None = None
    ok: bool = True
```

- `value` — what lands at `<effect>.value` in state; the thing a document actually chains off.
- `raw` — a free-form dict carrying the provider's own response shape (HTTP status/headers/
  body, MCP `structuredContent`, process `args`/`binary`, etc). Always redacted and size-capped
  before it reaches state (§1.5).
- `stdout`/`stderr` — only meaningful for process-backed plugins; every stdlib-only plugin
  (`json`, `clock`, `hash`, `uuid`, `regex`, `env_vars`) passes `None` for both.
- `exit_code` — a real process exit code only; `None` for every plugin that spawns no process,
  including every HTTP-family and MCP plugin (`src/circuitry/core/tool.py:421-423`: "`exit_code`
  means a process exit code only... it is `None` for every other plugin").
- `ok` — "the one signal `ToolRuntime` treats as a failure without an exception"
  (`src/circuitry/plugins/base.py:76-82`). Defaults to `True`.
  `validate_tool_result` (`:121-159`) additionally enforces: `raw` must be a `dict`;
  `stdout`/`stderr` must be `str | None`; `exit_code` must be `int | None` and `>= 0`; `ok` must
  be `bool`. electricity's Rust `ToolResult` equivalent must enforce the same shape, not just
  the types — a negative exit code or a non-dict `raw` is a plugin bug, not a user error, and
  should fail loudly in whatever electricity's debug/CI mode is, not silently coerce.

### 1.3 Timeouts

`ToolRuntime._resolve_timeout_seconds` (`src/circuitry/core/tool.py:472-499`): the effect's own
`timeout_ms` (rounded **up** to whole seconds, `max(1, ceil(ms/1000))`) wins when set; otherwise
`runtime.tools.timeout_seconds` from config; otherwise `DEFAULT_TOOL_TIMEOUT_SECONDS = 300`
(`:396`). This is deliberately independent of the run's model-adapter timeout
(`runtime.adapters.<name>.timeout_seconds`) — a tool on the no-op adapter must not inherit a
model's socket budget. Every tool plugin receives this resolved integer as
`execute(..., timeout_seconds=N)` and is responsible for enforcing it itself (subprocess
`timeout=`, `urlopen(timeout=)`, `asyncio.wait_for`, the `python_eval` forked-child deadline).

### 1.4 Failures → `meta.error`

`ToolRuntime.execute` (`src/circuitry/core/tool.py:453-854`) writes a node shaped
`<name>.value` + `<name>.meta{created_at, completed_at, provider, params_rendered, stdout,
stderr, exit_code, error, raw, status_code?, binary?, waiting_for, retries_used?, expect?}`.
A tool effect fails — `meta.error` is set, `on_error` (`fail`/`skip`/`continue`) applies —
whenever:
1. `plugin.execute(...)` raises any `Exception` (`:703-722`: `meta["error"] = str(failure)`), or
2. it returns a `ToolResult` with `ok=False` (`:764-772`):
   `failure = RuntimeError(result.stderr or f"{provider} tool reported failure (ok=False)")`.

HTTP-family plugins (`http`, `web_fetch`, `webhook`, `linear` — `_HTTP_FAMILY_PROVIDERS`,
`:38-40`) set `ok=False` on a 4xx/5xx by default (opt-out per effect, see each plugin). Every
other "soft-failure" plugin (`wikipedia`, `dns`, `port_check`, `validate_yaml` — not among
the tools production documents use, noted for parity completeness) reports its own outcome in `value`/`raw` while
keeping `ok=True`.

`meta.raw` is always set for a tool effect that returned a result (never for prompts);
`meta.binary` is set only when `result.raw` carries a `"binary"` key (the resolved absolute
executable path — see §2's subprocess plugins); `meta.status_code` is set only for
HTTP-family providers and only when `raw["status"]` is an `int` (`:38-40`, `:743-746`).

Retries: a tool with `retries:` set uses the same `RetryPolicyDef`/backoff curve as prompts
(`max_attempts`, `backoff_ms`) — see `core/prompt.RetryPolicyDef`. Retryability for HTTP-family
providers is classified by status (429/408/5xx retryable, §3.6); for every other provider,
`_is_retryable_failure` (`:401-420`) says every failure is retryable — "a process failing once
...is exactly the case #273 exists for." A provider's own `Retry-After` response header
overrides the computed backoff when the failed attempt's `raw.headers` carries one
(`:774-779`).

### 1.5 Redaction and `meta.raw` size cap

Every `ToolResult.raw` is piped through `_capped_raw` (`src/circuitry/core/tool.py:50-71`)
before it reaches `meta.raw`:
1. `redact(raw)` — deny-list redaction (§1.5.1), recursive over dicts/lists/tuples/strings.
2. JSON round-tripped with `default=str` (so e.g. a stray `bytes`/`datetime` a plugin forgot to
   coerce doesn't crash `--out`/SQL persistence downstream rather than failing at the point of
   generation).
3. If the encoded size exceeds `_RAW_META_MAX_BYTES = 64 * 1024` (`:47`), replaced with
   `{"_truncated": true, "_original_bytes": N, "_preview": "<first 64KiB>"}`.

`meta.params_rendered` is independently redacted (`redact(rendered)`, `:665`) before storage —
the plugin itself still receives the unredacted params.

**1.5.1 Redaction deny-list** (`src/circuitry/cli/redaction.py`):
- `_SENSITIVE_KEY_RE` (`:22-30`) — case-insensitive match on a dict key's final dotted/snake/
  kebab segment against `api_key|access_key|secret_key|auth_token|access_token|bearer_token|
  id_token|refresh_token|session_token|csrf_token|authorization|password|passphrase|
  client_secret|set_cookie|cookie|secret|token|credentials?`. A matching key's value becomes
  `"***REDACTED***"` regardless of content.
- `_JWT_RE` (`:37`) — a bare string matching `^eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$`
  (three base64url segments, header must start `eyJ`) is redacted even when its key isn't
  sensitive.
- `_KEYISH_RE` (`:42-47`) — `sk-...`(20+), `xox[abposr]-...`(20+), `ghp_...`(30+), or a bare
  `Bearer <16+ chars>` string.
- A string containing `://` and `@` is treated as a URL and has its userinfo stripped
  (`_redact_url`, `:51-62`): `scheme://user:pass@host` → `scheme://***REDACTED***@host`.
- `redact()` (`:81-106`) recurses through dicts/lists/tuples; every other scalar passes through
  unchanged. This is explicitly "a deny-list, not a guarantee" (`:1-9`) — defense in depth, not
  a secrets-management system.

electricity's Rust redaction must replicate all four rules bit-for-bit (the regex, the JWT/key
prefixes, the URL-userinfo strip) since the conformance suite will diff redacted state against
`cof run --out`.

### 1.6 `_as_bool` coercion

`src/circuitry/plugins/base.py:86-105`. Exists because "Mustache renders a templated `false`
as the literal string `"False"`, and Python's `bool("False")` is `True`." Rule: `None` →
`default`; an actual `bool` passes through; a string is **false** iff, case-insensitively after
stripping whitespace, it is one of `"false"`, `"0"`, `"no"`, `"off"`, `"n"`, or empty — **true**
otherwise; any other type falls back to `bool(value)`. electricity must replicate this exact
string set (not Python truthiness) for every boolean tool param: `allow_nonzero`, `fail_on_error`,
`epoch`, `from_path`, `include_secrets`, `hex`, `create_dirs`, `recursive`, `use_tls`.

### 1.7 Capability tags

`src/circuitry/plugins/capabilities.py`. Four tags: `shell`, `python_eval`, `fs-write`,
`network` (`:35-38`, `CAPABILITIES` tuple `:42`). `PLUGIN_CAPABILITIES` (`:47-116`) maps each
registered plugin name to the `frozenset` of tags it needs; a name absent from that map and
present in `NO_CAPABILITIES` (`:121-141`) needs none by explicit classification — every
registry entry must be in exactly one of the two sets (enforced by
`test_every_plugin_registry_entry_is_classified`, so a newly added plugin fails CI until
classified). Tags matter for electricity only insofar as `runtime.plugins.<name>.allowed_commands`-
style host pins are tag-adjacent; a full capability-consent *prompt* gate
(`cli.document_consent` + `capability_gate.py`) is a `cof`-side, interactive-trust concept that
does not apply to electricity's explicit-config-is-trusted model. **Superseded regarding the
run-time half** this paragraph originally carved out as a need: DESIGN.md §14 settles this
differently from this document's first draft — a capability ceiling is only ever installed, in
the reference, for a library-sourced document or a `use: ref:` child reached from one, neither
of which electricity ever runs (it rejects `ref:` outright, §4 step 4 there), so the ceiling is
always unrestricted for electricity, including for reflector/decomposition-generated plans; it
is not a gate electricity enforces.

The tools production documents use and their tags, for reference:
`shell` → `{shell}`; `awk`, `imagemagick`, `ffmpeg` → `{shell}`; `json`, `clock`, `hash`,
`env_vars`, `regex`, `uuid` → no tag (`NO_CAPABILITIES`); `http` → `{network}`; `mcp`,
`surrealdb`, `email_smtp` → `{network}`; `python_eval` → `{python_eval}`; `fs` → `{fs-write}`
(`capabilities.py:50,56-77,88-112`).

### 1.8 Allowlists (`enabled_tools`)

`src/circuitry/allowlist_gate.py`. `enabled_tools: list[str] | None` in config
(`CircuitryConfig.enabled_tools`, `src/circuitry/cli/config.py:134`) is `None` =
default-open (every compiled-in tool allowed); an empty list locks every tool out; a populated
list only allows those names. Resolved once per run and installed into the shared
`runtime_config["_allowlists"]` (`ALLOWLISTS_KEY`, `:26`); every tool-building call site
(`ToolRuntime.execute`, `src/circuitry/core/tool.py:580`) calls `require_tool(provider,
allowed_tools(runtime_config))` **before** the first attempt, not per-retry — "a tool blocked
by either should never even start its backoff schedule" (`tool.py:573-579`). Matching is
case-insensitive on the canonicalized (`.strip().lower()`) name (`allowlist_gate.py:52-56`).
This check is the run-time backstop that also covers a provider name only knowable after
templating, or reached through a `use` child — it's not just a static YAML lint.

### 1.9 Building a plugin from config

`src/circuitry/plugins/factory.py:596-616`, `build_plugin(*, plugin_name, runtime)`:
1. Canonicalize `plugin_name` (`.strip().lower()`).
2. Look up `PLUGIN_REGISTRY[plugin_name]` (`:507-593`, a `dict[str, PluginBuilder]` where
   `PluginBuilder = Callable[[dict[str, Any]], ToolPlugin]`); an unknown name raises `ValueError`
   naming every supported plugin.
3. Read `cfg = runtime["plugins"].get(plugin_name) or {}` — i.e. `runtime.plugins.<name>` in
   config.json — and call the builder with it.

Most builders ignore `cfg` entirely (stdlib-only plugins: `clock`, `math`, `regex`, `json`,
`fs`, `hash`, `uuid`, `env_vars`, ...; also `http`, `email_smtp`). Builders that *do* consume
`cfg`: every `GenericSubprocessTool`-backed plugin reads `binary`/`env` overrides
(`_build_awk`/`_build_imagemagick`/etc. → `plugin_binary_override`/`plugin_env_override`,
`:131-154`, `:273-278`); `shell` reads `allowed_commands` as a **host pin**
(`_build_shell`, `:263-271`); `ffmpeg` reads `binary`/`env` directly (not via
`GenericSubprocessTool`, `:131-134`); `surrealdb` reads `url`/`namespace`/`database` only — "
Credentials are deliberately absent here" (`:303-310`); `mcp` reads `servers`
(`:454-455`). This `runtime.plugins.<name>` → per-plugin constructor-arg mapping is the
contract electricity's own tool-plugin factory must reproduce key-for-key.

---

## 2. Tool plugins production documents use

Every plugin below, unless noted, builds with an empty config dict in the reference impl's
registry entry (`factory.py` builders cited per-plugin).

### 2.1 `shell`

`src/circuitry/plugins/shell.py`. Params: `command` (required str, must be bare
alphanumeric+`-_`, checked **before** PATH lookup, `:78-90`); `args` (list[str], each rejected
if it contains `\x00` or `\n`, `_DANGEROUS_ARG_CHARS`, `:55`, `:137-143`); `allowed_commands`
(optional per-effect override, default `_DEFAULT_ALLOWED = ("ls","cat","head","tail","wc",
"echo","pwd","date")`, `:52-53`); `cwd` (str); `stdin` (str); `allow_nonzero` (bool via
`_as_bool`).

**Host pin**: `runtime.plugins.shell.allowed_commands` (`_build_shell`, `factory.py:263-271`,
validated as `list[str]` → `tuple`) becomes `ShellPlugin.pinned_allowed_commands`. The effective
allowlist is `effect_allowed_set & pinned_set` (`shell.py:99-103`) — **never a union**: "a
*limited* document... can only narrow the allowlist further, never widen it past the host's
pin." If the command is in the effect's own list but not the pin's intersection, the error
explicitly names the pin and refuses the widen (`:106-115`); otherwise a generic "not in
allowlist" error (`:116-120`).

**Literal-only rule** (enforced in `core/tool.py`, not in the plugin itself):
`_SECURITY_SENSITIVE_PARAM_KEYS = frozenset({"allowed_commands"})` (`tool.py:260-266`).
`_reject_templated_security_params` (`:267-293`) raises before render if the effect's literal
`params.allowed_commands` is a `{from: ...}` reference, or any list item is a `{from:...}`, or
any string item contains `{{` (a Mustache tag). `_reject_params_json_security_overrides`
(`:296-308`) separately raises if a rendered `params_json` tries to set `allowed_commands` at
all — only the document's literal, unrendered `params:` block is ever honoured for this key.
electricity must enforce this check **before** Mustache rendering, at the exact same boundary
(literal params vs. `params_json` vs. templated strings).

**argv building**: `resolve_binary([command])` (`_subprocess.py:40-51`) does a `shutil.which`
lookup (absolute paths must already exist) — this happens **after** the allowlist gate, so an
unresolvable binary never leaks path info for a disallowed command. `run_binary`
(`_subprocess.py:166-224`) calls `subprocess.run([binary, *args], shell=False, capture_output=
True, text=True, timeout=int(timeout_seconds), cwd=cwd, env=env, input=stdin, check=False)`.
Non-zero exit raises `RuntimeError(f"{binary} failed (exit {code}): {err}")` unless
`allow_nonzero`. `FileNotFoundError`/`TimeoutExpired` are translated to clearer `RuntimeError`s.

Output: `value = stdout`; `raw = {"args": cmd[1:], "cwd": cwd, "binary": binary}`;
`stdout`/`stderr`/`exit_code` populated.

### 2.2 `json`

`src/circuitry/plugins/json.py`. Modes: `parse` (default — `json.loads(input)`, raises
`ValueError` on decode failure), `stringify` (`json.dumps(input, indent=int(indent) if indent is
not None else None, ensure_ascii=False, default=str)` — **exact formatting electricity must
match**: `ensure_ascii=False` (non-ASCII passes through literally, not `\uXXXX`-escaped),
`default=str` (unknown types stringified, never a crash), `indent=None` unless the param is
set), `extract` (dotted/indexed path walk, `_PATH_TOKEN = r"([A-Za-z_][A-Za-z0-9_-]*)|\[(-?\d+)\]"`,
`:26`, supports negative list indices via Python slicing semantics; returns `params.get("default")`
on a miss, never raises for a missing path). `raw = {"mode": mode}`.

### 2.3 `fs`

`src/circuitry/plugins/fs.py`. Modes: `read`/`write`/`append`/`list`/`stat`/`exists`/`delete`.
Path rule: `path` must be a non-empty string without `\x00` (`_validate_path`, `:28-32`); `~` is
expanded (`Path(raw).expanduser()`). "All operations stay within whatever path the caller
specifies; there is no implicit sandbox" (`:1-6`) — electricity must not add one either; parity
means matching the *absence* of a jail, not inventing one.

Write semantics: `write`/`append` require `content: str`; `create_dirs` (default `True` via
`_as_bool(..., default=True)`) does `path.parent.mkdir(parents=True, exist_ok=True)` before
opening; `raw["bytes_written"] = len(content.encode(encoding))`. `delete` is idempotent on a
missing path (`value=False`, not an error); deleting a directory requires `recursive=True`
(`_as_bool(..., default=False)`) or raises `IsADirectoryError`. `list` returns
`sorted(p.name for p in path.iterdir())`; `stat` returns `{size, mtime: int(st_mtime),
mode, is_dir, is_file}`.

### 2.4 `awk` / `imagemagick`

Both are `GenericSubprocessTool` instances (`_subprocess.py:225-281`), differing only in
`binary_candidates`: `awk` → `("awk", "gawk", "mawk")` (`plugins/awk.py:17-23`); `imagemagick`
→ `("magick", "convert")`, preferring ImageMagick 7's `magick` subcommand form over IM6's
`convert` (`plugins/imagemagick.py:1-11,20-26`). Both take `args: list[str]` (required),
`cwd`, `stdin`, `allow_nonzero`; `binary`/`env` come only from
`runtime.plugins.<name>.binary`/`.env` config, never from params ("a machine-specific
executable path... belongs in config, not in orchestration YAML", `_subprocess.py:258-262`).
`resolve_plugin_binary` (`:101-127`): a configured `binary` must be an absolute, existing,
executable path or it raises naming the setting and the resolved path; otherwise PATH search
over the candidates, raising if none found, naming `runtime.plugins.<name>.binary` as the
escape hatch.

### 2.5 `ffmpeg`

`src/circuitry/plugins/ffmpeg.py`. Not a `GenericSubprocessTool` — it builds its own argv from
structured params: `input` (required), `output` (required), `flags` (str, `shlex.split` into
argv), `extra_inputs` (list, each becomes `-i <path>`), `filter_complex` (str, mutually
exclusive in practice with `vf_drawtext`/`flags` in the branch order), `map`, `vf_drawtext`
(dict → built into a `-vf drawtext=...` filter string, `_build_drawtext_filter`, `:67-90`, with
careful escaping of `\`, `%`, `'`, `"` in the text value, `_escape_drawtext_text`, `:47-59`).

**Shell-metacharacter checks** — two tiers, both run even though `shell=False` is hard-coded
(defense in depth against a filter string that could otherwise smuggle a shell-interpreted
character into a different consumer): `_SHELL_METACHARACTERS = ("&&","||","|",";",">","<","`",
"$(","\n")` (`:13`) applied to `input`/`output`/`flags`/each `extra_inputs[i]`
(`_check_safe`, `:19-26`); `_FILTER_METACHARACTERS = ("&&",">","<","`","$(","\n")` (`:16`,
*excludes* `|`/`;` because those are legitimate ffmpeg filter-graph separators) applied to
`filter_complex` (`_check_filter_safe`, `:28-35`). Command built as `["ffmpeg","-y","-i",input,
*extra_input_pairs, (-filter_complex X | -vf drawtext... | shlex.split(flags)), (-map Y)?,
output]` (`:128-144`), binary resolved via `resolve_plugin_binary` with `configured=self.binary`.
On non-zero exit: `RuntimeError` including the full quoted command string and stderr
(`:165-170`) — note this is **not** redacted; ffmpeg args are not expected to carry secrets, but
electricity should still run this error text through the redaction deny-list for parity-safety.
`value = output_path`; `raw = {"binary": binary}`.

### 2.6 `http`

`src/circuitry/plugins/http.py`. Params: `url` (required), `method` (default `"GET"`,
uppercased), `headers` (dict), `json` (dict, mutually exclusive with `body`, sets
`Content-Type: application/json` if absent and `json.dumps`-encodes), `body` (raw str, sent
as-is), `params` (dict, URL-encoded query string appended with `urllib.parse.urlencode(...,
doseq=True)`, `&` or `?` separator chosen based on whether the URL already has a `?`), `parse`
(`"json"|"text"|"auto"`, default `"auto"` — parses JSON only when the response Content-Type
contains `"json"`, else text; `parse="json"` forces a parse and raises `RuntimeError` if the
body isn't valid JSON), `fail_on_error` (bool via `_as_bool`, default `True`).

Transport: stdlib `urllib.request.urlopen`, not `requests` ("the standard library is sufficient
...and keeps the dependency footprint narrow", `:7-11`). A 4xx/5xx is caught as
`urllib.error.HTTPError` and **not re-raised** — it still produces `status`/`headers`/`body`;
`stderr = f"HTTP {status}: {reason}"` plus the bounded redacted excerpt (§2.6.1) when one is
available. A `urllib.error.URLError` (DNS/connection failure) **is** a real raised
`RuntimeError` — the distinction between "got an HTTP response, just not a 2xx" (returned,
`ok=False`) and "never got a response at all" (raised) is load-bearing for retry classification
(§3.6) and electricity must preserve it.

Output: `value` per `parse`; `raw = {"status": int, "headers": {lowercased name: value},
"url": str, "body": str}`; `exit_code` always `None`; `ok = not (fail_on_error and status >=
400)` (`:153`).

**2.6.1 `http_error_excerpt`** (`src/circuitry/plugins/base.py:16-62`): bounded to 500 chars
(`_ERROR_EXCERPT_MAX_CHARS`). Prefers, in order: a JSON body's `error.message` (nested, the
ComfyUI shape), a flat `error` string, `message`, `detail`; falls back to the whole
**redacted** body (never the raw body — "a sibling credential-shaped field... must not reach
the failure message just because it wasn't under `error`/`message`/`detail`", `:57-62`) for an
unrecognized JSON shape; falls back to the first 500 chars of the raw text (through `redact()`
too) for a non-JSON body. Shared by `http`, `web_fetch`, `webhook`, `linear`.

### 2.7 `clock`

`src/circuitry/plugins/clock.py`. Params: `format` (strftime, default
`"%Y-%m-%dT%H:%M:%S%z"`), `timezone` (IANA name via stdlib `zoneinfo.ZoneInfo`, default UTC;
unknown name raises `ValueError`), `epoch` (bool — `value` becomes `int(now.timestamp())`
instead of the formatted string). `raw = {"iso": now.isoformat(), "epoch": int(...), "timezone":
str(tz)}`. electricity needs an IANA tz database (the `tz`/`chrono-tz` crate) to match
`timezone` parity, not just a UTC-offset model.

### 2.8 `hash`

`src/circuitry/plugins/hash.py`. Algorithms: `md5, sha1, sha224, sha256, sha384, sha512,
sha3_256, sha3_512, blake2b, blake2s` (`_SUPPORTED_ALGORITHMS`, `:21-24`), default `sha256`.
`input` (str) hashed via `encoding` (default `utf-8`) unless `from_path` (bool) is true, in
which case `input` is a file path read in 64KiB chunks. `output_format`: `hex` (default) or
`base64` (standard, not urlsafe, b64).

### 2.9 `env_vars`

`src/circuitry/plugins/env_vars.py`. **Read-only by design** ("Writing to `os.environ`... would
be a foot-gun", `:1-10`). `mode: "get"` — `os.environ.get(name, default)`, `raw = {"name",
"present": name in os.environ}`. `mode: "list"` — optional `prefix` filter; `include_secrets`
(bool via `_as_bool`, default `False`) — when false, any variable name matching
`_SECRET_PATTERN = re.compile(r"(?:API[_-]?KEY|TOKEN|PASSWORD|PASSWD|SECRET|CREDENTIAL|
PRIVATE[_-]?KEY)", re.IGNORECASE)` (`:21-24`) has its value replaced with `"***"` in the
returned dict — note this is a **separate, narrower** pattern than the central
`cli.redaction._SENSITIVE_KEY_RE` (§1.5.1); both must exist in electricity, not just one.

### 2.10 `regex`

`src/circuitry/plugins/regex.py`. Params: `pattern` (required), `input` (required str), `mode`
(`match|search|findall|sub`, default `findall`), `flags` (list of names from
`IGNORECASE|MULTILINE|DOTALL|VERBOSE|ASCII`, `_FLAG_MAP`, `:21-27`), `replacement` (required for
`sub`), `count` (int, default 0 = all, for `sub`).

**Python `re` features actually reachable**: this plugin does nothing but
`re.compile(pattern, flags)` and call `.match`/`.search`/`.findall`/`.sub` — **the full Python
`re` grammar is exposed**, including lookahead `(?=...)`/`(?!...)`, lookbehind
`(?<=...)`/`(?<!...)`, backreferences `\1`/`(?P=name)`, named groups `(?P<name>...)`, and
non-greedy quantifiers. Rust's `regex` crate deliberately excludes backreferences and
lookaround for linear-time guarantees — **this is the single largest tool-parity gap in the
whole set of tools production documents use** (see §6 risk list, item 1); electricity needs `fancy-regex` or an
embedded PCRE/Oniguruma binding, not the `regex` crate alone, or it must document which production
documents' patterns would silently behave differently.

`match`/`search` return `list(groups)` if the match has capture groups, else `group(0)`, else
`None`. `findall` returns the raw `re.findall` result (list of strings, or list of tuples if
multiple groups). `sub` returns the substituted string. `raw = {"pattern", "mode", "flags":
params.get("flags")}` (the flag **names** as given, not the resolved int).

### 2.11 `uuid`

`src/circuitry/plugins/uuid.py`. `version` (`1|4|5`, default 4); `count` (int ≥1, default 1 —
returns a bare string if `count==1`, else a list); `hex` (bool — `.hex` form, no dashes).
v5 requires `namespace` (one of `dns|url|oid|x500` named constants, or a parseable UUID string)
and `name`. v1 uses `uuid.uuid1()` (MAC address + timestamp — **not reproducible across
hosts**, a conformance-suite gotcha, see §7).

### 2.12 `mcp`

`src/circuitry/plugins/mcp_client.py`. Config: `runtime.plugins.mcp.servers` — a map of server
name → `{command, args?, env?, cwd?}` (stdio) or `{url, headers?}` (http/sse), optionally with
an explicit `transport` (`stdio|http|sse`; `streamable_http` aliased to `http`)
(`resolve_transport`, `:98-130`). Transport is otherwise **inferred**: `command` present →
stdio; `url` present → http. Credential-bearing `env`/`headers` values are redacted by the
standard central redaction before any state artifact is written — **never excluded from the
config block itself**, so electricity's config loader must not special-case MCP server configs
out of the redaction pass.

Params: `server` (required), `tool` (required unless `operation: list_tools`), `arguments`
(dict, string values Mustache-rendered like every other tool param upstream), `operation`
(`call|list_tools`, default `call`), `parse` (`auto|json|text`, default `auto`), `fail_on_error`
(bool, default `True`).

`parse: auto` behavior (`:289-301`): prefer the server's `structuredContent` when present; else
try `json.loads` on the joined text content, keeping the parse only if it's a dict/list (a bare
JSON scalar like `42` or `"foo"` stays text — "a text-only reply is otherwise indistinguishable
from a plain string"); else fall back to the raw text. `isError` sets `ok = not (fail_on_error
and is_error)` (`:321`); `stderr` is set to the joined text (or a generic message) when
`is_error`. `exit_code` is always `None` — "stdio MCP servers are a session, not a one-shot
exec" (`:81-84`).

Lifecycle: **one fresh connection per call** — a new subprocess spawn for every stdio-transport
invocation, deliberately not pooled ("correct and thread-safe under tree flow... a per-run
session cache is a future optimization", `:90-92`). electricity's v1 should match this (no
session cache) unless a later milestone revisits it — pooling changes failure/restart semantics
in ways the conformance suite would need new cases for.

`list_tools` returns `[{name, description, input_schema}, ...]`
(`_describe_tool`, `:133-138`); `raw` for `call` is `{server, tool, transport, operation,
is_error, content: [dumped blocks], structured}`.

### 2.13 `surrealdb` (tool plugin)

`src/circuitry/plugins/surrealdb.py`. Config (`runtime.plugins.surrealdb`): `url` (default
`DEFAULT_URL = "ws://localhost:8000/rpc"`, `:43`), `namespace`, `database` — **credentials are
never read from this config block**, only from `SURREAL_USER`+`SURREAL_PASS` or
`SURREAL_TOKEN` env vars (`_credentials`, `:78-95`), "so they cannot reach
`runtime.effective_settings` or persisted run state" — this is a stronger rule than the general
redaction deny-list: the value is never even read into the config object, let alone redacted
after the fact. electricity's SurrealDB tool builder must keep this asymmetry (config has no
credential fields at all).

Modes (`MODES = ("query","select","create","upsert","delete")`, `:44`): `query` (`query` str +
optional `params` dict of SurrealQL variables); `select` (`target`/`record`/`table` alias);
`create` (`table`/`target` + `data` dict); `upsert` (`record`/`target` + `data` dict); `delete`
(`record`/`target`). `namespace`/`database` can be overridden per-effect via params, falling
back to the configured pair; both are required (one way or another) or it raises.

Auth tries signin scopes most-specific-first — `{namespace, database}`, then `{namespace}`,
then `{}` (bare/ROOT) — because "SurrealDB's signin has no cross-level fallback server-side"
and the client can't tell which auth level a credential belongs to from config alone
(`_signin_scopes`, `:187-203`). SurrealQL per-statement errors (`status: "ERR"` in a list
response) are raised even though the RPC call itself didn't throw
(`_raise_for_statement_errors`, `:146-151`). Values are coerced JSON-safe before returning
(`_jsonable`, `:120-133`, stringifies anything not `None|bool|int|float|str|dict|list|tuple|
set`).

### 2.14 `email_smtp`

`src/circuitry/plugins/email_smtp.py`. stdlib `smtplib`. Required: `host`, `from_addr`,
`subject`, `body`, `to` (str or list[str]). Optional: `port` (default 587), `username`/
`password` (both-or-neither — no auth attempted if either is missing), `use_tls` (bool, default
`True` — STARTTLS), `content_type` (`text/plain`|`text/html`, default `text/plain`), `cc`/`bcc`
(str or list[str]). Connection timeout is the tool's own `timeout_seconds` budget
(`smtplib.SMTP(host, port, timeout=int(timeout_seconds))`). `value = len(all_recipients) -
len(refused)` — count of accepted recipients, not a boolean. `raw = {host, port, to, cc, bcc,
refused: list(refused.keys())}`.

### 2.15 `python_eval`

`src/circuitry/plugins/python_eval.py`. **electricity cannot reimplement this natively** — it
is RestrictedPython + a Python sandbox; it is the canonical case for the external plugin
protocol (§6). Specified here so the bridge's contract (§6) can be checked against it.

Params: `code` (required), `inputs` (dict, keys must be valid identifiers not starting with
`_`, mirrored into both globals and locals so comprehension bodies see them —
`_validate_input_names`, `:188-200`), `mode` (`eval|exec`, default `eval`; `exec` returns the
`result` local var or `None`).

Sandbox: `RestrictedPython.compile_restricted` + a curated `_SAFE_BUILTINS` set (`:161-171`,
math/string/collection builtins only — no `open`, `__import__`, `eval`, `exec`, `compile`,
`getattr` unguarded). `_write_` = `full_write_guard` (permits item assignment on plain
`list`/`dict` only); `_inplacevar_` is a custom guard (`:179-185`) that only uses a real in-place
operator for `list`/`dict`, falling back to the plain binary operator for everything else so
`x += y` can't call a target's `__iadd__` to bypass `_write_`. `__import__` isn't in the
builtins, so a plain `import` statement compiles but fails at runtime with `ImportError`.

**Process isolation**: the compile+eval/exec runs in a `multiprocessing.get_context("fork")`
child (`:120-124`, fork required because `inputs` may carry live Python objects — closures,
locally-defined classes — that can't cross a `spawn` pickle boundary). Parent/child talk over an
explicit `Pipe` (not a `SimpleQueue` — a queue would keep the parent's write-end fd open forever,
so EOF never arrives if the child dies silently); the parent polls with a deadline **before**
`join()`, not after, so a result larger than the pipe's OS buffer (tens of KiB) can still drain
while the child is still writing (`execute`, `:383-461`). `rlimit`s (POSIX only): `RLIMIT_CPU`
set to `wall_seconds + _CPU_LIMIT_MARGIN_SECONDS(5)` (`:114`, `:395`) so the wall-clock deadline
— with the clearer error message — wins the race, not a bare `SIGXCPU`; `RLIMIT_AS` set to the
child's own baseline VM size at fork time (read from `/proc/self/status`, Linux-only) plus
`_MEMORY_HEADROOM_BYTES = 1 GiB` (`:107`, `:225`) — relative, not absolute, since an absolute
cap could already be below what the parent process has mapped. macOS does not enforce
`RLIMIT_AS` at all; this is a best-effort backstop, not a hard guarantee on every platform.

Returns: `status, payload` over the pipe — `"ok"` (payload = result, must be picklable) or
`"error"` (payload = the exception, re-raised as-is in the parent). `raw = {"mode": mode}`.

---

## 3. Model adapters

### 3.1 Adapter interface

`src/circuitry/adapters/base.py:140-154`:

```python
class Adapter(Protocol):
    @property
    def name(self) -> str: ...
    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> GenerateResult: ...
    def check(self) -> CheckResult: ...
```

Two more hooks are *optionally* implemented and called only through shims so pre-existing
adapters keep working: `list_models() -> list[str]` (`adapters/models.py:30-37`, `ModelLister`
Protocol, called via `call_list_models` which never raises — an unreachable daemon or garbage
return degrades to `[]`), and `generate(..., options: GenerateOptions)` — called via
`call_generate` (`base.py:184-212`), which inspects `generate`'s signature
(`_accepts_options`, `:176-182`) and only passes `options` to adapters that declared the kwarg
(or `**kwargs`); an adapter that doesn't take `options` still runs, with a warning recorded on
the result naming what was ignored (`ignored_options_warning`, `:118-130`).

### 3.2 `GenerateResult`

`src/circuitry/adapters/base.py:24-34`:

```python
@dataclass(frozen=True)
class GenerateResult:
    text: str
    raw: dict[str, Any]
    tokens_sent: int | None = None
    tokens_received: int | None = None
    finish_reason: str | None = None
    warnings: tuple[str, ...] = ()
```

`finish_reason` is the provider's own stop reason, unnormalized (`"stop"`, `"length"`,
`"end_turn"`, `"max_tokens"`, ...); `TRUNCATED_FINISH_REASONS = {"length", "max_tokens"}`
(`:20`) is the set `core/prompt.py` checks to append a "cut off by the length limit" warning.

### 3.3 `GenerateOptions`

`src/circuitry/adapters/base.py:67-108`. Portable knobs every adapter maps to its own wire
names: `temperature: float | None`, `max_tokens: int | None`, `stop: tuple[str, ...]`. `params:
Mapping[str, Any]` — every other `params:` key from the prompt effect, passed through unchanged
in whatever slot the provider's API keeps "extra" options (Ollama's `options` sub-object, the
request body directly for OpenAI/Anthropic-shaped APIs). `deterministic: bool` — the *runtime*
(`core/prompt._generation_options`, `src/circuitry/core/prompt.py:1127-1128`) turns this into
`temperature=0` unless `params` already set one; a seed (`DETERMINISTIC_SEED = 0`,
`base.py:17`) is only sent by adapters whose provider actually accepts a seed field (`ollama`
via `options.seed`, `:1172` in `ollama.py`; `openai`/`anthropic-family` via `params: {seed:
...}` only if the caller sets it explicitly — OpenAI's adapter does `payload.setdefault("seed",
DETERMINISTIC_SEED)` only when `deterministic`, `openai.py:63-64`). `messages:
tuple[ChatMessage, ...]` — role-tagged turns that replace the flattened `prompt` string when
present. `images: tuple[ImageInput, ...]` — attached to the last user turn.

`ImageInput` (`:46-64`): exactly one of `data: bytes | None` / `url: str | None` is set;
`base64_data()`/`data_url()` helpers. `adapter_accepts_images(adapter)` (`:157-158`) checks a
class-level `accepts_images: ClassVar[bool] = True` marker — set on `anthropic`, `openai`,
`ollama` among the adapters production documents use.

### 3.4 The OpenAI-compatible family

`src/circuitry/adapters/_openai_compat.py` — shared transport for ~20 providers speaking
`POST /chat_completions_path` with Bearer auth and the `choices[0].message.content` /
`usage.{prompt,completion}_tokens` response shape.

`OpenAICompatibleConfig` (`:32-47`): `base_url`, `api_key_env` (may be empty string for
self-hosted endpoints that need no auth — `vllm`, `llamacpp`, `lmstudio`), `default_model`,
`chat_completions_path` (default `/chat/completions`; may contain a `{model}` placeholder,
substituted via `str.format` — used by `azure-openai` for
`/openai/deployments/{model}/chat/completions?api-version=...`).

Request mapping (`chat_messages`, `:51-77`): role-tagged `messages` pass through as-is except
`role: "tool"`, which the Chat Completions API has no bare slot for, so it's sent as
`{"role": "user", "content": f"tool: {content}"}`; no messages at all → one
`{"role": "user", "content": prompt}`. Images become OpenAI's `image_url` content-part shape
(`{"type": "image_url", "image_url": {"url": image.data_url()}}`) appended after any text on
the last user turn (adding an empty user turn first if none exists).

`sampling_fields` (`:82-94`): `temperature`, `max_tokens` (key name overridable —
`max_completion_tokens` for `openai`'s own adapter since vanilla `max_tokens` is "deprecated
...and rejected by reasoning models", `openai.py:61`), `stop`, then every remaining `params` key
spread in verbatim, in that field order (later keys can't override the portable ones since
`fields.update(options.params)` runs last — a `params.temperature` would have already been
popped out by `core/prompt._generation_options` before reaching here anyway).

Response mapping (`parse_chat_response`, `:97-108`): `text = choices[0].message.content or ""`,
stripped; `finish_reason = choices[0].finish_reason` if a string. Token usage from
`raw["usage"]["prompt_tokens"]`/`["completion_tokens"]`, only kept if they're already `int`
(no coercion from numeric strings).

**Providers in this family and their env var names** (`adapters/factory.py` `_build_*`
functions, `api_key_env=` in each adapter file):

| adapter name | `api_key_env` | default `base_url` |
|---|---|---|
| `groq` | `GROQ_API_KEY` | `https://api.groq.com/openai/v1` |
| `openrouter` | `OPENROUTER_API_KEY` | `https://openrouter.ai/api/v1` |
| `perplexity` | `PERPLEXITY_API_KEY` | `https://api.perplexity.ai` |
| `xai` | `XAI_API_KEY` | `https://api.x.ai/v1` |
| `deepseek` | `DEEPSEEK_API_KEY` | `https://api.deepseek.com/v1` |
| `together` | `TOGETHER_API_KEY` | `https://api.together.xyz/v1` |
| `fireworks` | `FIREWORKS_API_KEY` | `https://api.fireworks.ai/inference/v1` |
| `nvidia-nim` | `NIM_API_KEY` (configurable key *name* via `api_key_env` cfg) | `https://integrate.api.nvidia.com/v1` |
| `vllm` | `""` (no auth) | `http://localhost:8000/v1` |
| `llamacpp` | `""` (no auth) | `http://localhost:8080/v1` |
| `lmstudio` | `""` (no auth) | `http://localhost:1234/v1` |
| `mistral` | `MISTRAL_API_KEY` | `https://api.mistral.ai/v1` |
| `ai21` | `AI21_API_KEY` | `https://api.ai21.com/studio/v1` |
| `huggingface-inference` | `HF_TOKEN` | `https://router.huggingface.co/v1` |
| `tgi` | `""` by default, configurable | `http://localhost:3000/v1` |
| `qwen-dashscope` | `DASHSCOPE_API_KEY` | `https://dashscope-intl.aliyuncs.com/compatible-mode/v1` |
| `cohere` | `COHERE_API_KEY` | `https://api.cohere.com` |
| `cloudflare-workers-ai` | `CF_API_TOKEN` | (requires `account_id`) |
| `gemini` | `GOOGLE_API_KEY` | `https://generativelanguage.googleapis.com/v1beta/openai` |
| `azure-openai` | `""` (uses `api-key` header via `extra_headers`, not Bearer) | `AZURE_OPENAI_ENDPOINT` env or `endpoint` cfg |

`check_dependencies` (`_openai_compat.py:212-220`): `binary:curl` if `curl` isn't on PATH;
`env:<api_key_env>` if that var is required and unset. electricity, using a native HTTP client
instead of curl, drops the `binary:curl` check entirely and should instead verify its own TLS
stack / DNS resolution is functional if it wants an equivalent liveness signal — not specified
further here since it's an implementation detail, not a wire-format one.

### 3.5 `anthropic`

`src/circuitry/adapters/anthropic.py`. `ANTHROPIC_API_KEY` env var (`:121-127`). Config
(`runtime.adapters.anthropic`): `base_url` (default `https://api.anthropic.com`),
`default_model` (default `"claude-sonnet-5"`), `max_tokens` (default 4096). `accepts_images =
True`. `KNOWN_MODELS` (`:96-100`, for `list_models()` — no network call, no API key needed):
`("claude-opus-5", "claude-sonnet-5", "claude-haiku-4-5")`.

Wire shape (`_request_body`, `:28-76`): Anthropic's Messages API (`POST /v1/messages`), headers
`x-api-key: <key>` + `anthropic-version: 2023-06-01` (**not** `Authorization: Bearer`). System
turns are joined into a separate `system` field, not sent as a `messages` turn. A bare `tool`
role turn is sent as `{"role": "user", "content": "tool: " + content}` (API has no bare tool
role outside tool-use blocks). The API rejects an empty `messages` array, so a system-only
prompt with no images becomes one user turn carrying the system text, clearing `system`. Images
become `{"type": "image", "source": {"type": "url"|"base64", ...}}` blocks placed *before* the
trailing text on the last user turn (reverse order vs. the OpenAI-compatible family, which puts
text first).

Response: `text` is the concatenation of every `content[i].text` where `type == "text"`.
`tokens_sent`/`tokens_received` from `usage.input_tokens`/`output_tokens`.
`finish_reason` = `raw["stop_reason"]`.

### 3.6 `openai`

`src/circuitry/adapters/openai.py`. Not built on `_openai_compat` — hand-rolled with the same
shape (duplicated, not shared, in the reference implementation). `OPENAI_API_KEY`. `base_url`
default `https://api.openai.com/v1`, `default_model` default `"gpt-4o-mini"`. `accepts_images =
True`. Uses `max_completion_tokens`, not `max_tokens` (reasoning-model compatibility, `:61`).
`Authorization: Bearer <key>`.

### 3.7 `ollama`

`src/circuitry/adapters/ollama.py`. No API key. `base_url` default `http://localhost:11434`.
`accepts_images = True`. Two endpoints depending on whether the prompt carries role-tagged
turns: `/api/chat` (when `options.messages` is non-empty) vs `/api/generate` (bare prompt
string) — `_request`, `:35-70`. Request-level fields vs. model-option fields are split by a
fixed set: `_REQUEST_FIELDS = frozenset({"format", "keep_alive", "think"})` (`:23`) — any
`params` key in that set goes at the top level of the payload; everything else (including
`temperature`, `max_tokens`→`num_predict`, `stop`) goes under `payload["options"]`.
`deterministic` sets `options.seed = DETERMINISTIC_SEED` only if `params` didn't already set a
seed (`setdefault`, `:68-69`).

`list_models()` (`:151-175`) — hits `GET /api/tags` with stdlib `urllib` (not curl) and a short
2-second timeout, returns the sorted list of installed tag names; any exception (daemon down)
degrades to `[]`, matching the `ModelLister` contract.

Error hinting (`_curl_json`, `:95-141`) distinguishes curl exit 7 ("not reachable" — daemon
down or wrong `base_url`), exit 28 (--max-time elapsed — model still generating, raise the
timeout config instead of assuming it's down), and exit 22 (4xx/5xx — if the error body mentions
"not found" and a model was requested, hint `ollama pull <model>`). electricity's native HTTP
client should produce the equivalent three-way classification (connection refused / request
timeout / HTTP error-with-body) and keep the model-specific "pull it" hint for a 404-shaped
"not found" response, since that's the single highest-friction local-model error.

### 3.8 `cyberdiner`

`src/circuitry/adapters/cyberdiner.py`. Config (`runtime.adapters.cyberdiner`): `expo_url`
(required, no default), `token` (required, a `ck_...` bearer string — no prefix validation
client-side), `default_tier` (default `"cheap"`), `valid_tiers` (optional client-side allowlist;
empty = pass-through, server validates), `poll_interval_ms` (default 500), `timeout_seconds`
(per-HTTP-request socket timeout, default 30 — distinct from `generate()`'s own
`timeout_seconds`, which bounds the whole submit+poll sequence), `max_in_flight` (default 0 =
unbounded; >0 installs a `threading.Semaphore` shared across the adapter instance to prevent a
`flow: tree` loop from submitting more jobs than the backing service can claim within its
claim window, driving excess jobs into a server-side `timedOut`).

**What it is**: a job-queue broker, not a direct completion API — `generate()` hides a
submit-then-poll loop (`POST {expo_url}/beta/jobs {prompt, tierName}` → poll `GET
{expo_url}/beta/jobs/{jobId}` every `poll_interval_ms` until a terminal status). Model name
maps to a vendor-specific *tier* name (`_resolve_tier`, `:116-137`), not a model string — the
orchestration's `model:` field is reinterpreted entirely. Terminal statuses, normalized case/
punctuation-insensitively (`_normalize_status`, `:159-171`, folds `TimedOut`/`timed_out`/
`timedOut` onto one key): `complete`/`completed` (success — both spellings accepted since the
service's write paths differ), `timedout` (server-declared dead job,
distinct from a client-side timeout), `failed`, `cancelled`. `tokens_sent` is always `None`;
`tokens_received` carries the service's single `tokensProcessed` counter as an approximation
("it includes the prompt's tokens too", `:367-370`).

Transport: stdlib `urllib.request`, not curl — `AdapterCallError`/`classify_curl_exit` do
**not** apply to this adapter; its own `_request` raises plain `RuntimeError`s, which
`classify_exception`'s cause-chain walk (§3.6) still classifies correctly via the wrapped
`urllib.error.HTTPError`/`URLError`.

**Does a production runner need it?** Yes if production orchestrations route through
this adapter's tiers (confirm against the actual documents); the protocol (submit/poll, tier
mapping, terminal-status folding, semaphore backpressure) is small enough that electricity
should implement it natively rather than defer to the external-plugin bridge, since it's a
model adapter (the bridge is scoped to *tool* plugins per the project's plugin list, which does not
include a generic "adapter" bridge).

### 3.9 `host_claude`

`src/circuitry/adapters/host_claude.py`. **Not needed by electricity v1.** It has no HTTP
transport at all — `generate()` calls an injected `request_handler(HostPromptRequest) -> str`
callback that only exists when circuitry is embedded inside an MCP server driven by a live
Claude session (`circuitry-mcp`), so the "model" is literally the Claude instance orchestrating
the run. `build_adapter("host_claude", ...)` always raises in the reference implementation
(`factory.py:316-325`) — "it requires a `request_handler` injected at runtime... Run
`circuitry-mcp`." electricity's production-only surface excludes the MCP server; electricity
should register the name in its adapter registry only to produce the same clear "not buildable
from config" error, not implement a working transport.

### 3.10 Retry classification and `Retry-After`

`src/circuitry/adapters/_retry.py`. `RetryInfo{retryable: bool, status: int | None,
retry_after: str | None}` (`:72-87`). `_status_is_retryable` (`:63-64`): `status == 429 or
status == 408 or 500 <= status <= 599` — every other 4xx (400/401/403/404/422) is **not**
retryable.

`AdapterCallError(RuntimeError)` (`:137-151`) carries a `retry_info: RetryInfo` set at raise
time by adapters that can classify their own failure. For the reference implementation's
curl-based adapters this comes from `classify_curl_exit(returncode, stderr)` (`:92-109`):
`RETRYABLE_CURL_EXIT_CODES = {6, 7, 28, 35, 52, 56}` (DNS failure, connect refused, --max-time
elapsed, SSL connect error, server stopped mid-transfer, network failed mid-transfer) are
retryable regardless of what the request was; curl's own `--fail-with-body` collapses every
HTTP error onto exit 22, whose *actual* status is parsed back out of curl's stderr text
(`returned error:\s*(\d{3})`, `_CURL_STATUS_RE`) since `--fail-with-body` never surfaces it on
`proc.returncode`. electricity, using a native HTTP client, reads the real status code directly
— it does not need this stderr-scraping step, but it must reproduce the *same retryable set*
(429/408/5xx + "never got a reply" conditions) exactly, since that set is shared by every
retrying effect (prompt, tool, use), not adapter-specific.

For a plain `RuntimeError(...) from exc`-raising adapter (`cyberdiner`'s `urllib`-based
requests, `watsonx`'s IAM exchange), `classify_exception` (`:194-210`) walks the exception's
`__cause__`/`__context__` chain (`_classify_cause_chain`, `:155-192`) looking for a
`urllib.error.HTTPError` (classify by `.code`) or a bare `URLError`/`TimeoutError`/
`ConnectionError` (no status attached — still retryable, "the request never got a reply at
all"). Anything else, or a chain that runs out without finding one, is **not** retryable — "
guessing wrong in the retryable direction could spin on a request that will never succeed."

`Retry-After` is read from the failing response's headers when available (`extract_retry_after`
from curl's `--write-out %header{retry-after}` capture for curl-based adapters;
`HTTPError.headers.get("Retry-After")` for `urllib`-based ones) and, when present,
**overrides** the computed exponential backoff outright (`next_backoff_delay_ms`, `:233-249`),
still capped at `RETRY_BACKOFF_CAP_MS = 60_000` (`:48`) — "a provider asking for longer than
this is still only waited out this long." Backoff without a `Retry-After` is full-jitter
exponential: `random.uniform(0, min(cap, base_ms * 2**attempt_index))`.

### 3.11 Preflight checks

Every adapter's `check()` returns a `CheckResult` following the same `missing` grammar as tool
plugins (§1.1): `env:<VAR>` for an unset required key, `host:<url>` for an unreachable endpoint
(curl-based adapters generally skip a live reachability probe — only `ollama.check()` actually
probes with a 2-second `--head` request, `ollama.py:220-246` — most others just check the env
var and `curl` binary presence). electricity's preflight should probe liveness more aggressively
than the reference where it's cheap (native HTTP is cheaper than spawning curl for a HEAD), but
must not *require* network reachability to pass `check()` for a provider the reference doesn't
probe, or a conformance case built around an intentionally-unreachable `base_url` would diverge.

### 3.12 Curl hygiene — security intent for electricity's native HTTP client

`src/circuitry/curl_support.py`. electricity replaces every curl invocation with a native Rust
HTTP client, so none of curl's own mechanics (config-file-via-pipe-fd, `--data-binary @-`,
`-q`) apply directly. The **security properties** a native client must still guarantee:
1. **No secret on argv or in a logged command line.** The reference's whole reason for
   `--config`-via-pipe instead of `-H`/`-d` flags is that argv is visible in `ps` to any local
   user; a native Rust client achieves the same by simply never constructing a shell command —
   headers and body go directly into the HTTP library's request object. electricity must still
   ensure no logging/tracing path (OTel span attributes, verbose-mode printing) ever echoes a
   header value or request body verbatim.
2. **No local `~/.curlrc`-style ambient config can change request behavior.** Not applicable to
   a library-based client, but the equivalent risk is a stray proxy env var (`HTTP_PROXY`,
   `NO_PROXY`) silently rerouting a request — electricity should document (and pin, where the
   host config specifies a `base_url`) whether it honors system proxy env vars at all.
3. **Failure messages must mask secrets and credential-shaped URL components**, mirroring
   `curl_failure_message`/`_mask_secrets`/`_strip_url_userinfo`/`_mask_credential_query_params`
   (`curl_support.py:106-143`, `:33-69`): a failure message names the source, target host
   (userinfo and credential-looking query params masked), HTTP status, and the provider's own
   (already-redacted) error detail — never the raw request or response body, and never a secret
   value even when truncation would otherwise let a partial match survive.
4. **`Retry-After` must be captured from the real response headers** a native client already
   has direct access to (no `--write-out` scraping needed) and fed into the same backoff
   override logic (§3.10).
5. **A response body excerpt in a failure message is bounded and redacted** the same way
   `parse_error_body`/`http_error_excerpt` do (§2.6.1) — a large or secret-bearing error body
   must not reach a log line or `meta.error` unbounded.

---

## 4. Runtime plugins electricity v1 needs

Two independent subsystems share the name "persistence" in the reference implementation;
electricity's design doc for this must keep them separate, since only one of them is read by
`--resume`.

### 4.1 State-snapshot persistence (`runtime.persistence`) — what `--resume` reads

`src/circuitry/core/store/persistence.py`. `PersistenceBackend` Protocol (`:19-38`):
`backend_name`, `describe() -> dict`, `load_latest_state(orchestration_path) -> dict | None`,
`load_run(orchestration_path, run_id) -> dict | None`, `save_run_snapshot(orchestration_path,
run_id, ok, error, state) -> None`. `build_persistence_backend(runtime)` (`:44-76`) reads
`runtime.persistence` — requires `enabled: true` (default false — persistence is opt-in);
`backend` (default `"postgres"` if `enabled`, but no implicit default backend is actually usable
without the required keys below) selects among aliases: `jsonl-file`/`jsonl_file`/`jsonl`/
`file`; `mongodb`/`mongo`; `postgres`/`postgresql`; `sqlite`/`sqlite3`.

**Backends and their record shape** (each writes the **full JSON-serialized run state**, not a
structured relational row — this is the key difference from §4.2):

- **`jsonl-file`** (`core/store/jsonl_file.py`): `runtime.persistence.path` (or `db_path` alias,
  required) — appends one line per `save_run_snapshot` call:
  `{"run_id", "orchestration_path", "created_at", "ok", "error", "state": <full state dict>}`
  (`:132-151`). `load_latest_state` scans the file **backwards**, returning the first record
  matching `orchestration_path` with `ok: true` (`:50-83`). `load_run` scans **forwards**,
  matching both `run_id` and `orchestration_path` (never cross-document, `:91-116`). Malformed
  lines are skipped with a warning, not fatal — "an append log can be truncated mid-write."
- **`sqlite`** (`core/store/sqlite.py`): `runtime.persistence.db_path` (or `path` alias,
  required), `table` (default `"circuitry_runs"`, validated as a safe SQL identifier). One row
  per run, `state_json` as a `TEXT` column holding the full serialized state; `run_id` is **not**
  a primary key constraint shown in the DDL fragment read but is looked up uniquely by
  `WHERE run_id = ? AND orchestration_path = ?`. `load_latest_state` orders by
  `created_at DESC, rowid DESC LIMIT 1` filtered to `ok = 1`.
- **`postgres`** (`core/store/postgres.py`): `runtime.persistence.dsn` (required), `table`
  (default `"circuitry_runs"`), `sslmode` (default `"require"` — `disable`/`allow`/`prefer`
  are rejected unless `allow_insecure: true` is also set, `:33-41` — electricity must keep this
  secure default and the explicit opt-out, not silently allow plaintext).
- **`mongodb`** — a document store; same four-method contract, not read in detail here since it
  is not cited by any production document today — note its existence for completeness, defer its
  spec until a document actually needs it.

`--resume <run-id>` (`cli/app.py:586`): `persistence.load_run(orchestration_path=str(path),
run_id=resume)` — **requires** `runtime.persistence` to be configured on the orchestration
(there's no fallback to the B-prime SQL tables in §4.2). `--resume last` instead reads the
previous invocation's own `--out` file via a separate "last run" stash
(`cli/last_run.py`), independent of `runtime.persistence` entirely. An effect is resumable
(`core/resume.effect_completed_ok`, `src/circuitry/core/resume.py:44-51`) exactly when its node
shows `meta.completed_at` set and `meta.error` falsy — "never started, still mid-flight... or
finished with an error" all rerun. A named loop keeps its own `meta.completed_passes` list
(not a generic node check) and resumes at the first missing contiguous index — this loop-specific
resume logic lives in `core/loop.py`, out of scope for this plugin/adapter spec but load-bearing
for `--resume` correctness.

`document_sha256` (`resume.py:33-42`) hashes the orchestration file at run start
(`state.runtime.last_run.document_hash`) and `--resume` refuses to continue if the document
changed since, unless `--force`.

### 4.2 B-prime structured persistence (`runtime.runtime_plugins.*`) — observability only, not resumed from

This is a `RuntimePlugin` (§4.3), loaded via the generic `plugins:` list + `enabled_plugins`
allowlist, **not** `runtime.persistence`. It writes relational records for querying/analytics
after the fact; `--resume` never reads it.

**`sqlite`/`postgres`/`mysql`/`duckdb`/`mssql`/`cockroachdb` share `SqlPersistenceBase`**
(`runtime_plugins/_sql_persistence.py:99-100`) and the two-table schema in
`runtime_plugins/_sql_schema.py`:
- `runs(run_id PK, orchestration_path, status, started_at, ended_at, error, inputs JSON)`
  (`runs_ddl`, `:126-138`) — `inputs` is `extract_inputs(state)`, the `state.input` namespace
  (or, for pre-namespace snapshots, a filtered set of root keys) — i.e. the run's seed values,
  not its full trace.
- `effect_results(id PK autoinc, run_id FK, state_path, effect_name, effect_type, parent_path,
  iteration_index, value JSON, raw JSON, tokens_sent, tokens_received, started_at, ended_at,
  status, error)` (`effect_results_ddl`, `:140-160`) — one row **per effect**, written from
  `on_effect_complete`. `effect_type` is inferred heuristically from the meta shape
  (`infer_effect_type`, `_sql_persistence.py:78-88`: `prompt_type`/`prompt_sent` in meta →
  `"prompt"`; `stdout`/`exit_code` in meta → `"tool"`; `"flow"` in meta → `"dynamic"`; a
  value dict with an `"iterations"` key → `"loop"`; else `"effect"`). `state_path` is
  decomposed into `(effect_name, parent_path, iteration_index)` via `parse_effect_path`
  (`_sql_schema.py:174-199`) rather than stored as an opaque string, so SQL queries can
  reconstruct dynamic/loop structure without string parsing.

Each dialect's differences are isolated in `SqlDialect` (`_sql_schema.py:31-50`): autoincrement
syntax, JSON column type, timestamp type, placeholder style (`?` vs `%s`), and any `pre_ddl`
(DuckDB needs a `CREATE SEQUENCE` first). `clickhouse` has its own plugin file — its driver
isn't DBAPI-cursor-shaped, so it bypasses `SqlPersistenceBase` entirely (noted, not detailed
here; not cited by production documents).

`store_raw` cascade, shared by every SQL-family plugin **and** `surrealdb`'s B-prime plugin
(`_sql_persistence.resolve_environment`/`_resolve_store_raw`, `:62-71,137-155`; mirrored in
`runtime_plugins/surrealdb.py:230-243`): `CIRCUITRY_<NAME>_STORE_RAW` env var (truthy:
`1/true/yes/y/on`, case-insensitive) → `runtime.runtime_plugins.<name>.store_raw` config bool
→ `default_store_raw(environment)` = `true` iff `environment == "dev"`, else `false`
(`_sql_schema.py:203-205`). `environment` itself resolves `CIRCUITRY_ENV`/
`CIRCUITRY_ENVIRONMENT` env var → `PluginContext.environment` (from `CircuitryConfig.environment`,
§5) → `"dev"`. When `store_raw` is false, the `raw` column/field is omitted (`None`), dropping
the largest/most sensitive payload (full provider responses, full tool `raw`) from long-term
storage by default outside dev.

**`surrealdb` (runtime plugin, distinct from the `surrealdb` tool plugin in §2.13)**
(`runtime_plugins/surrealdb.py`): schemaless `runs`/`effects` tables with the same field names
as the SQL schema but as **native SurrealDB objects**, not JSON-as-text — "SurrealDB has no need
for a JSON-as-string column." No `id`/autoincrement management (SurrealDB assigns its own
record id per `CREATE`). Config precedence per field, env var first then
`runtime.runtime_plugins.surrealdb.*`: `SURREAL_URL`/`url` (default `ws://localhost:8000/rpc`),
`SURREAL_NAMESPACE`/`namespace` (default `"circuitry"` — **different default from the tool
plugin**, which has no default namespace and requires one explicitly), `SURREAL_DATABASE`/
`database` (default `"circuitry"`), `SURREAL_TOKEN`/`token` (priority over user/pass),
`SURREAL_USER`+`SURREAL_PASS`/`user`+`password` (signin with the same most-specific-first scope
fallback as the tool plugin, `_signin_scopes`, `:261-279`).

**`jsonl-file` (runtime plugin)** (`runtime_plugins/jsonl_file.py`) is a **third, independent**
JSONL writer — not the same file/format as the `jsonl-file` *persistence backend* in §4.1 ("the
two are independent and can be enabled at the same time", `:9-11`). It's an **event stream**,
one line per lifecycle hook firing (`run_start`, `effect_complete`, `run_success`,
`run_failure` — exact per-event field sets at `:77-128`), not a full-state snapshot; suited to
`grep`/`jq`-style tailing, not resume. Config: `CIRCUITRY_JSONL_PATH`/
`runtime.runtime_plugins.jsonl-file.path` (default `./circuitry-runs.jsonl`);
`CIRCUITRY_JSONL_INCLUDE_EFFECTS`/`.include_effects` (bool, default `True` — when `False`, only
run-level events are recorded, dropping the per-effect lines).

**`postgres` (runtime plugin)** config precedence (`runtime_plugins/postgres.py:7-11`):
`DATABASE_URL` env → `CIRCUITRY_POSTGRES_DSN` env → standard libpq vars (`PGHOST`, `PGUSER`,
`PGPASSWORD`, `PGDATABASE`, `PGPORT`) → `runtime.runtime_plugins.postgres.dsn` config.
**`sqlite` (runtime plugin)** config precedence (`runtime_plugins/sqlite.py:10-17`):
`CIRCUITRY_SQLITE_PATH` env → `runtime.runtime_plugins.sqlite.db_path` config → default
`./circuitry-runs.db`.

### 4.3 Hook points

`src/circuitry/core/runtime_plugins.py`. `PluginContext` (`:17-26`): `{run_id: str,
orchestration_path: Path, dry_run: bool, validate_only: bool, runtime_config: dict,
environment: str = "dev"}`. `RuntimePlugin` Protocol (`:29-74`) — three **required** hooks
(enforced by `_validate_plugin`, `:216-222`, at load time — a plugin missing any of these three
fails to load with a clear error) and two **optional** ones (checked via `hasattr`/`callable` at
call time, never required):

- **`on_run_start(*, state, context)`** — required. Fires once, after persistence's
  `load_latest_state` has already hydrated `state` (if applicable) but before the first effect
  runs. This is where every B-prime plugin opens its connection, ensures its schema, and inserts
  the `runs` row with `status: "running"`.
- **`on_effect_start(*, state, context, effect_path, effect_node)`** — optional. Fires
  immediately before an effect dispatches, carrying the node as it stands at that moment
  (including any complexity score, if scoring is enabled). The OTel plugin uses this to open a
  span (§4.4); the SQL/jsonl plugins do **not** implement it (they record only on completion).
- **`on_effect_complete(*, state, context, effect_path, effect_result)`** — optional.
  `effect_result` is `{value, meta}` — the same two keys the effect's own state node carries.
  Fires after `node["value"]` is written. This is the hook every B-prime/observability plugin
  actually implements.
- **`on_run_success(*, state, context)`** / **`on_run_failure(*, state, context, error:
  str)`** — required (the pair closes out the `runs` row / emits the final span status / etc).
  Exactly one of the two fires per run, after every effect (or the failing one) has completed.

`invoke_plugins` (`:118-191`) calls every loaded plugin for a given hook, catching and logging
(never propagating) any exception from an individual plugin — a plugin's own failure must never
abort the run or block another plugin from firing. It returns a list of
`{plugin, hook, ok, error}` event records, which the reference implementation surfaces at
`state.runtime.plugins.events`.

`load_plugins(plugin_ids, *, allowed)` (`:81-115`): each id is either a dotted module path
exposing a module-level `plugin` **callable or instance** (`_load_single_plugin`, `:193-213` —
if `module:` has no `:attr`, look for `module.plugin`; if `module:attr` is given, use that
attribute, calling it if callable), or `module:attr` explicitly. `allowed` (from
`enabled_plugins` in config, §5) gates by exact id string — not in the list → the plugin is
never imported, recorded as a load error naming the allowlist, not a crash.

### 4.4 OpenTelemetry

`src/circuitry/runtime_plugins/opentelemetry.py`. **Span model**: one root span
`"circuitry.run"` per run (attributes: `circuitry.run_id`, `circuitry.orchestration_path`,
`circuitry.dry_run`), opened in `on_run_start` (`:155-172`). One child span `f"effect:
{effect_path}"` per effect, opened in `on_effect_start` and closed in `on_effect_complete`
(`:174-295`) — **parented by nested state path**, not by call order: `_parent_context`
(`:127-153`) walks the effect path's dotted prefixes outside-in looking for the nearest
still-open ancestor span (so a loop's own span, opened at e.g. `prime.shots`, correctly parents
its body's effects at `prime.shots.iter_3.handle` even though the intermediate `iter_3` segment
never gets its own start/complete event); falls back to the run span's context for a top-level
effect.

**Timing**: span start/end times come from the effect's own `meta.created_at`/`meta.completed_at`
(converted to nanoseconds, `_iso_to_ns`, `:36-46`), **not** from whenever the plugin happens to
be invoked — "a span's duration is the effect's real wall time."

**Attributes** set on each effect span: `circuitry.run_id`, `circuitry.effect_path`, plus
whichever of `circuitry.adapter`, `circuitry.model`, `circuitry.flow` (from `meta.flow` or
`meta.mode`), `circuitry.provider` are present in the effect's meta (`:188-208`). On completion,
`circuitry.tokens_sent`/`circuitry.tokens_received` (only if present and genuinely `int`), and
on error, `span.set_status(Status(StatusCode.ERROR, error))` + `circuitry.error` attribute
(`:234-247`).

**Thread-safety for concurrent branches sharing an unnamed path**: `_spans` maps
`effect_path -> list[(thread_id, span, context)]`, not a single entry — because an unnamed
`loop`/`dynamic` running `flow: tree` doesn't namespace its body's state path per branch, so two
concurrent branches can report the identical `effect_path` at once; `_pop_own_span` (`:306-320`)
matches by `threading.get_ident()` so a completing call always closes *its own* span, never a
sibling branch's.

**Cleanup**: `_finalize` (`:297-327`), called from `on_run_success`/`on_run_failure`, force-closes
any span whose `on_effect_complete` never fired (a crash mid-effect) with a best-effort "now" end
time, then ends the run span (with error status if the run failed) and shuts down the exporter.

Config: standard `OTEL_*` env vars only (`OTEL_SERVICE_NAME`, `OTEL_EXPORTER_OTLP_ENDPOINT` —
OTLP/HTTP exporter if set, else a console exporter for local debugging) — no
`runtime.runtime_plugins.opentelemetry.*` config keys in the reference implementation.

### 4.5 Live state

`src/circuitry/cli/live_state.py`. Not a `RuntimePlugin` — a `Store.on_write` callback
(`LiveStateMirror`, `:66-187`) wired directly into the CLI's run loop via `--live-state <path>`,
independent of the `runtime_plugins`/`enabled_plugins` subsystem entirely. Writes the **current
full state** (same serialization as `--out`, `core.saved_state.dumps_saved_state`) to a path
atomically (`tempfile.mkstemp` in the same directory + `os.replace`, never a predictable
sibling path — avoids a symlink-preplant race, `:30-50`). Coalesced: at most one write per
`LIVE_STATE_INTERVAL_SECONDS = 0.5` (`:19`) regardless of how many effects complete in that
window — every `on_write` call just updates "the newest pending snapshot" under a condition
variable; a dedicated writer thread drains it no faster than the interval. The very first write
happens synchronously, outside the run (so an unwritable path fails the run immediately, before
any effect executes) and always happens with the initial snapshot. `close(final_state)`
(`:163-186`) stops the writer thread and performs one final synchronous write of the run's true
final state (so the mirror "ends equal to `--out`"), returning whether any write across the
whole run failed, folded by the caller into one warning rather than left silent. electricity's
equivalent should preserve: coalescing (don't serialize on every single effect write under
heavy fan-out), the atomic-rename write, and the guaranteed-synchronous first and last write.

---

## 5. Config keys that change run behavior

`src/circuitry/cli/config.py` (`CircuitryConfig`, `:117-183`) + the generic `runtime` pass-through
dict every subsystem above reads from directly.

| Key | Type | Default | Effect |
|---|---|---|---|
| `default_model` | `str \| None` | `None` (SANE_DEFAULTS ships `"llama3.1:8b"`, `:49`) | Model used when a prompt effect has no `model:` pin. |
| `default_adapter` | `str \| None` | `None` (SANE_DEFAULTS ships `"ollama"`, `:50`) | Adapter used when a prompt effect has no `provider:`. |
| `plugins` | `list[str]` | `[]` | Dotted module ids of **runtime plugins** to load (§4.3) — not tool plugins, which need no such list (tool plugins are referenced by `provider:` directly). |
| `enabled_adapters` | `list[str] \| None` | `None` (default-open) | Allowlist of model-adapter names a run may build. `[]` locks out every adapter. Case-insensitive (lowercased at load, `:177`). |
| `enabled_plugins` | `list[str] \| None` | `None` (default-open) | Allowlist of **runtime-plugin ids** (exact dotted-path match, case-sensitive). |
| `enabled_tools` | `list[str] \| None` | `None` (default-open) | Allowlist of tool-plugin names. Case-insensitive. |
| `environment` | `"dev"\|"prod"\|"test"` | `"dev"` | Drives `store_raw` defaults for every B-prime persistence plugin (§4.2) — `true` only in `dev`. Invalid values fall back to `"dev"` with a warning (`:166-174`). |
| `runtime` | `dict` | `{}` | Pass-through namespace every subsystem below reads its own sub-keys from. |
| `runtime.adapters.<name>` | `dict` | `{}` | Per-adapter config (`base_url`, `default_model`, `timeout_seconds`, provider-specific keys — e.g. `max_tokens` for anthropic, `expo_url`/`token`/`default_tier`/`valid_tiers`/`poll_interval_ms`/`max_in_flight` for cyberdiner). |
| `runtime.adapters.<name>.timeout_seconds` | `int` | adapter-specific, generally 120 | Per-attempt dispatch budget; `0`/negative/unparseable counts as unset, not unlimited (`adapters/factory.py:87-116`). A prompt effect's own `timeout_ms` can only **shorten** this, never lengthen it (`core/prompt.py:1088-1106`). |
| `runtime.plugins.<name>` | `dict` | `{}` | Per-**tool**-plugin config (§1.9) — `binary`/`env` overrides for subprocess wrappers, `allowed_commands` host pin for `shell`, `url`/`namespace`/`database` for `surrealdb`, `servers` for `mcp`. |
| `runtime.plugins.shell.allowed_commands` | `list[str] \| None` | `None` (no pin) | Host ceiling on the `shell` tool's allowlist — intersected with, never widened by, an effect's own `params.allowed_commands` (§2.1). |
| `runtime.runtime_plugins.<name>` | `dict` | `{}` | Per-**runtime**-plugin config (§4.2/§4.4) — `db_path`/`dsn`/`url`/`store_raw`/`path`/`include_effects`, read directly by each plugin module, not modeled as a `CircuitryConfig` field. |
| `runtime.persistence` | `dict` | `{"enabled": false}` implicitly (absent dict → `None` backend) | State-snapshot backend selection for `--resume`/auto-hydration (§4.1): `enabled` (bool, default `false`), `backend` (`jsonl-file\|mongodb\|postgres\|sqlite`), plus backend-specific keys (`path`/`db_path`, `dsn`, `table`, `sslmode`, `allow_insecure`). |
| `runtime.max_concurrency` | `int \| None` | `None` (unbounded run-wide) | Run-wide semaphore cap on concurrently-dispatching effects (`core/concurrency.py:43-52`, must be a positive integer or it's a config error). |
| `runtime.concurrency_groups` | `dict[str, int] \| None` | `None` (no named groups) | Named sub-pools an effect can opt into via its own `group:` key; each value must be a positive integer (`core/concurrency.py:54-76`). A `group:` naming an undefined group is a config error, not a silent no-op (`:168-171`). |
| `trust_orchestration_runtime` | `bool` | `False` | `cof`-only: lets an orchestration's own `runtime:` block set host-level settings. electricity's "both files operator-chosen = trusted" model means electricity's config.json is **always** effectively trusted for its own settings — this flag's `cof`-side nuance (letting the *orchestration YAML itself* override host config) likely does not need a v1 equivalent; flag for confirmation if any production document relies on an orchestration-level `runtime:` override today. |

Environment-variable overlays that win over all of the above when set (`CONFIG_ENV_VARS`,
`config.py:72-81`): `CIRCUITRY_MODEL`, `CIRCUITRY_ADAPTER`, `CIRCUITRY_ADAPTER_URL`,
`CIRCUITRY_COMFYUI_URL`, `CIRCUITRY_ENABLED_ADAPTERS`, `CIRCUITRY_ENABLED_PLUGINS`,
`CIRCUITRY_ENABLED_TOOLS`, `CIRCUITRY_ENVIRONMENT` — plus every per-plugin env var documented in
§2–§4 (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `SURREAL_*`, `CIRCUITRY_<NAME>_STORE_RAW`,
`CIRCUITRY_JSONL_PATH`, `DATABASE_URL`, `CIRCUITRY_SQLITE_PATH`, ...), which take precedence over
their `runtime.*` config counterparts in every case cited above.

---

## 6. Plugin protocol: input for an out-of-process bridge

**Goal**: let Circuitry's existing Python tool plugins — concretely `python_eval` (used
extensively in production documents) and, as a fallback for any tool not yet reimplemented natively in Rust —
run unmodified under the Rust host, communicating over stdin/stdout.

### 6.1 Shape of the bridge

Follow the MCP/LSP precedent: **JSON-RPC 2.0 over newline-delimited JSON on stdin/stdout**, one
long-lived child process per run (not per call — matches `python_eval`'s existing per-call
process-fork-and-die semantics for *its own* sandboxing, but the bridge process itself should be
a persistent dispatcher so repeated tool calls in one run don't pay Python interpreter startup
cost on every call). The Rust host is the JSON-RPC **client**; the Python process is the
**server**, speaking a small custom method set over stdio, exactly analogous to how an MCP
server exposes tools and the Circuitry MCP client (`plugins/mcp_client.py`, §2.12) already talks
to one. This also means electricity's own MCP-client tool plugin and this bridge can share most
of their transport-layer code once ported to Rust.

### 6.2 Methods

**Superseded.** The method names below (`plugin/execute`, and the health-check/logging RPCs
further down this section) are this document's own first-draft sketch, written before the
method set actually settled on. DESIGN.md §8.5 is the current design: `describe` / `invoke` /
`progress` / `cancel`, with this section's health-check behavior folded into a re-callable
`describe` and its logging behavior carried over the plugin process's own stderr stream instead
of a dedicated RPC. The request/response shapes below still illustrate the same underlying
execute-a-plugin-call exchange; only the method names and the extra RPCs are stale.

- **`plugin/execute`** (request, Rust → Python):
  ```json
  {
    "jsonrpc": "2.0",
    "id": 7,
    "method": "plugin/execute",
    "params": {
      "plugin": "python_eval",
      "params": { "code": "result = 2 + 2", "mode": "exec", "inputs": {} },
      "timeout_seconds": 300,
      "config": { }
    }
  }
  ```
  - `plugin` — the canonical lower-case tool-plugin name (same namespace as `provider:` in
    YAML), so one bridge process can serve every unported plugin, not just `python_eval`.
  - `params` — exactly the rendered `params` dict `ToolRuntime` would otherwise hand to
    `plugin.execute(params=...)` natively (§1.3) — Mustache rendering, `{from: ...}` reference
    resolution, and the `allowed_commands` literal-only check (§2.1) all happen on the **Rust**
    side, before this message is sent; the Python side never sees an unrendered template or a
    reference marker.
  - `timeout_seconds` — the resolved budget from §1.3. The Python side is responsible for
    enforcing it internally exactly as `python_eval` already does (forked child + pipe +
    poll-with-deadline, §2.15) **and** the Rust host independently enforces a hard kill of the
    whole bridge *call* (not the whole process) at `timeout_seconds + grace`, so a Python-side
    bug in its own timeout logic can't hang the Rust host indefinitely. This is the same
    belt-and-suspenders the reference implementation already applies inside `python_eval` itself
    (CPU rlimit *and* wall-clock poll, §2.15) one level up.
  - `config` — the **slice** of `runtime.plugins.<plugin>` config relevant to this one plugin,
    not the whole `runtime` config tree. Never includes adapter configs, other plugins'
    configs, or credentials for plugins other than this one — minimizing what crosses the
    process boundary is itself a security property (a compromised/buggy bridge process can only
    leak what it was handed).

- **`plugin/execute` response** (Python → Rust), success:
  ```json
  {
    "jsonrpc": "2.0",
    "id": 7,
    "result": {
      "value": 4,
      "raw": { "mode": "exec" },
      "stdout": null,
      "stderr": null,
      "exit_code": null,
      "ok": true
    }
  }
  ```
  Field-for-field the `ToolResult` shape from §1.2 — the Rust host applies the **same**
  redaction (§1.5), size-capping, and `meta.error`/`on_error` logic to a bridged result as it
  does to a natively-executed one; the bridge must not pre-redact (the Rust side's redaction
  deny-list is the single source of truth, so it must see the real value) but **must** still
  avoid writing an unredacted copy to its own stderr/logs (§6.5).

  Failure (the plugin raised in the reference implementation): a JSON-RPC **error** object, not
  a `result` with `ok: false` — that distinction matters because `ToolRuntime` treats a raised
  exception and an `ok: false` result slightly differently for retry classification (§1.4, §3.6):
  ```json
  {
    "jsonrpc": "2.0",
    "id": 7,
    "error": {
      "code": -32000,
      "message": "python_eval: rejected by sandbox: ...",
      "data": { "exception_type": "PermissionError", "retryable": false }
    }
  }
  ```
  `data.retryable` lets the Python side opt into the same `AdapterCallError`-style explicit
  classification tool effects already get (§1.4's `_is_retryable_failure`) instead of the Rust
  host having to pattern-match the Python exception's message text.

- **`plugin/check`** (request, Rust → Python): `{plugin: str, config: dict}` → response
  `{ok: bool, missing: list[str], message: str | None}` — the `CheckResult` shape (§1.1),
  called at preflight time for every plugin the bridge is asked to serve, exactly mirroring
  `call_check`'s never-raises contract (§1.1) — a Python-side exception during `check()` must
  still produce a well-formed `{ok: false, ...}` response, not kill the bridge process or
  surface as a transport-level error.

- **`plugin/cancel`** (notification, Rust → Python, no response expected): `{id: <request id to
  cancel>}` — best-effort. For `python_eval` specifically, cancellation should map directly onto
  the existing forked-child-kill path (§2.15's `proc.terminate()`/`proc.kill()`) rather than
  inventing new cancellation plumbing; for a plugin with no sub-process of its own, cancellation
  may simply be unsupported (the bridge acknowledges it's unsupported for that plugin, the Rust
  host falls back to waiting out `timeout_seconds` as it would anyway). This is needed for Ctrl-C
  / SIGTERM propagation (the runtime's exit code 130/143 contract, §8.4 of the
  runtime-semantics spec) to reach a bridged call that's mid-flight, not just a
  natively-executed one.

- **`plugin/log`** (notification, Python → Rust, no response expected): `{level: "debug"|"info"|
  "warning"|"error", message: str}` — so a Python plugin's own `logging` calls (every reference
  plugin uses the stdlib `logging` module for non-fatal warnings) surface through the Rust
  host's own structured logging/OTel pipeline instead of being silently dropped, or — worse —
  written directly to the shared stdout stream where they'd corrupt the JSON-RPC framing. **No
  Python-side code may ever write to stdout except via this JSON-RPC channel** — this is the
  single most important invariant of the whole bridge; a stray `print()` in a third-party Python
  plugin breaks line-delimited JSON framing for every subsequent message. The bridge's own
  bootstrap must redirect the Python process's real stdout fd to stderr (or a log file) at
  startup, before any plugin code can run, and use a dedicated fd/pipe pair for the actual
  JSON-RPC traffic — mirroring the `python_eval` sandbox's own discipline of talking over an
  explicit `Pipe`, never inheriting a shared stream (§2.15).

### 6.3 How the Python side loads a plugin by name

Reuse the existing registry, not a new mechanism: the bridge process imports
`circuitry.plugins.factory` and calls `build_plugin(plugin_name=params["plugin"],
runtime={"plugins": {params["plugin"]: params["config"]}})` (§1.9) — i.e. the bridge is a thin
JSON-RPC server wrapped around the exact same `PLUGIN_REGISTRY` lookup `cof` uses today. This
means **every** plugin in the registry (not just `python_eval`) is bridgeable without new Python
code, which gives electricity a complete fallback path for any tool plugin not yet ported to
Rust, not only a hand-picked subset.

### 6.4 Config slice

Only `runtime.plugins.<plugin>` (plus, if the plugin is `mcp`, the relevant subset of
`runtime.plugins.mcp.servers` — scoped to the servers the specific call's `params.server` names,
not the whole server map, by the same least-privilege logic as §6.2's `config` field) ever
crosses the boundary. Adapter configs (`runtime.adapters.*`), persistence configs, and other
tool plugins' configs are never sent — the Rust host is the only process that ever holds the
full config tree.

### 6.5 Security implications

1. **No secrets on the wire in cleartext logs.** JSON-RPC messages themselves necessarily carry
   unredacted `params`/`config` (the Python plugin needs the real value to do its job) — this is
   the same trust boundary `ToolRuntime` already crosses when calling a native plugin's
   `execute()` directly (§1.5: "the plugin itself still receives the unredacted values"). The
   bridge must never **persist** the raw JSON-RPC traffic to disk (no transcript logging by
   default) and must ensure any debug-mode traffic dump goes through the same redaction pass as
   state before it's written anywhere.
2. **The child process is a second trust boundary with its own blast radius**, not a sandbox
   around the parent. A Python plugin that shells out (`shell`, `git`, `ffmpeg`-via-bridge-
   fallback) has the same host access it would running natively — the bridge adds process
   isolation (a crash doesn't take down the Rust host) and a kill switch (SIGKILL the whole
   bridge process on an unresponsive `plugin/cancel`), not a security sandbox. `python_eval`'s
   own RestrictedPython + forked-child + rlimits sandboxing (§2.15) is what actually bounds that
   one plugin; the bridge does not add an additional sandbox layer on top, and must not be
   described to users as one.
3. **Allowlist/capability checks happen before the message is sent, not after.** `require_tool`
   (§1.8), the `shell.allowed_commands` literal-only rule (§2.1), and the capability ceiling
   (§1.7) are all enforced on the Rust side using the Rust host's own config, before a
   `plugin/execute` request is ever constructed — a compromised or buggy Python plugin process
   has no path to widen its own permissions by lying in a response, because the response's
   `ok`/`value` is not itself a capability grant.
4. **Process lifecycle must not create a confused-deputy window.** If the bridge process is
   reused across multiple tool calls within one run (§6.1's "long-lived child" choice), a bug
   that lets one call's state (e.g. a Python-level global, an open file handle) leak into the
   next call's execution is a real risk unique to the long-lived design, traded for lower
   per-call latency. The bridge should reset or re-`import` risky global state between calls to
   the same plugin, or document explicitly which of the reference plugins are not safe to call
   twice in the same process (RestrictedPython's `compile_restricted` output is stateless per
   call and safe to reuse; a plugin holding an open DB connection as module-level state, e.g. if
   `surrealdb`'s tool plugin were ever bridged instead of ported, is not).
5. **Transport must be a dedicated pipe pair, not inherited stdio**, per §6.2's `plugin/log`
   discussion — this is a security property as much as a correctness one: a plugin that can
   write arbitrary bytes to the *same* stream the Rust host is JSON-parsing is one `print()` away
   from a parser-confusion bug, which is a more severe failure mode than a crash.

---

## 7. Conformance suite

Concrete cases, each run against both `cof run` (scripted fakes, no real network/model calls)
and electricity, diffing final state (and, where applicable, the exact string of a failure
message) byte-for-byte. Organized by the divergence it would catch.

**Tool plugins**

1. `shell` — host pin narrower than effect list: config `allowed_commands: ["ls"]`, effect
   `params.allowed_commands: ["ls", "cat"]`, command `cat` → must raise the **pin-specific**
   `PermissionError` message (§2.1), not the generic "not in allowlist" one. Catches a Rust port
   that unions instead of intersects.
2. `shell` — templated `allowed_commands` rejected: effect `params.allowed_commands:
   ["{{user_cmd}}"]` → must raise before any process spawns, naming the literal-only rule,
   regardless of what `user_cmd` resolves to. Catches a Rust port that renders before checking.
3. `json` stringify — `indent: null` vs `indent: 2` vs non-ASCII input (`"café"`) → byte-exact
   `ensure_ascii=False` output; a Unicode string must not become `\uXXXX` escapes.
4. `fs` delete — missing path (`value: false`, no error) vs. directory without `recursive`
   (raises) vs. with `recursive: true` (succeeds) — three branches, one case each.
5. `ffmpeg` — `filter_complex` containing `|` and `;` must succeed (filter-graph syntax);
   `flags` containing the same two characters must raise (shell-metacharacter check applies to
   `flags` but not `filter_complex`). This is the one place the two safety-check tiers diverge
   and a naive single-tier Rust port would get wrong in one direction or the other.
6. `http` — a 404 JSON body shaped `{"error": {"message": "not found"}}` vs. a flat
   `{"error": "not found"}` vs. `{"message": "not found"}` vs. an unrecognized shape (e.g.
   GraphQL-style `{"errors": [...]}`) → four distinct `http_error_excerpt` extraction paths,
   each must produce the exact same excerpt string.
7. `http`/`mcp` — `fail_on_error: false` / `fail_on_error: true` (default) on the same 500
   response → `ok` flips, `meta.error`/`on_error` applies or doesn't, exactly as scripted.
8. `regex` — a pattern using a lookbehind (`(?<=foo)bar`) and one using a backreference
   (`(\w+)\1`) against the same input in both engines. **Expected to fail today** against a
   bare Rust `regex`-crate port — this case exists specifically to force the fancy-regex-vs-
   `regex`-crate decision (§6 risk list item 1) rather than let it surface later as a silent
   divergence.
9. `uuid` v1 — run twice in the same process; assert the conformance harness does **not** compare
   v1 output byte-for-byte (it's host/time-dependent by design) but does assert both are
   well-formed v1 UUIDs with a monotonically increasing timestamp component. v4/v5 **are**
   compared byte-for-byte given a fixed name/namespace (v5) or a scripted RNG seed (v4, if the
   harness can inject one on both sides).
10. `python_eval` (via the bridge) — (a) a tight `while True: pass` → must raise the exact
    `"exceeded timeout of {N}s"` message at the configured wall-clock budget, not the CPU-limit
    margin's extra 5 seconds later; (b) `__import__("os")` → compiles but raises `ImportError` at
    run time, not at compile time; (c) a dunder-prefixed attribute access → rejected at compile
    time with a `PermissionError` naming "rejected by sandbox"; (d) `inputs` carrying a
    locally-defined class instance → must round-trip correctly (proves `fork`, not `spawn`, is
    in use on the bridge's host platform).
11. `mcp` — a scripted stdio server that returns `structuredContent` vs. one that returns only a
    JSON-shaped text block vs. one that returns a bare JSON scalar as text (`"42"`) → three
    `parse: auto` paths, the third must stay a string, not become the integer `42`.
12. `env_vars` list — a variable named `MY_API_KEY_SUFFIX` (matches `_SECRET_PATTERN` as a
    substring) vs. `MY_APIKEYLESS_VAR` → confirms the regex's `API[_-]?KEY` boundary behavior
    exactly, not an approximation of it.

**Model adapters**

13. `ollama` — a scripted local server returning curl-equivalent connection-refused vs. a
    4xx-with-JSON-body vs. a slow response that exceeds `timeout_seconds` → three distinct hint
    strings (§3.7), each checked verbatim.
14. `anthropic` vs. an OpenAI-compatible adapter (e.g. `groq`) — the same `options.messages`
    including a `tool`-role turn and one image → confirms the two different image-ordering and
    tool-turn-relabeling conventions (image-before-text for Anthropic, image-after-text for the
    OpenAI family) are each implemented, not one copied onto the other.
15. Retry classification — a scripted 429 with `Retry-After: 3` vs. a 429 with no such header
    vs. a 401 (not retryable) vs. a connection-refused (retryable, no status) → four cases
    through `classify_*`/`next_backoff_delay_ms`, asserting both the `retryable` boolean and
    (where applicable) the exact wait duration honoring the header over the computed curve.
16. `cyberdiner` — scripted expo responses cycling `pending` → `assigned` → `completed`, and a
    separate run cycling straight to `timedOut` → confirms both terminal-status spellings
    (`complete`/`completed`) and the `timedOut`/`timed_out`/`TimedOut` folding (§3.8) all land on
    the same two outcomes.
17. `host_claude` — `build_adapter("host_claude", ...)` must raise the documented "cannot be
    built from config" error, never attempt a network call or silently no-op.

**Runtime plugins / persistence**

18. `--resume <run-id>` against a `sqlite` **persistence backend** (§4.1), not the B-prime
    `sqlite` runtime plugin (§4.2) — confirms electricity reads the right one of the two
    same-named-sounding subsystems. A run that fails partway, then resumes, must skip every
    effect with `meta.completed_at` set and no `meta.error`, and rerun everything else,
    byte-identical final state to an uninterrupted run.
19. B-prime `store_raw` cascade — `CIRCUITRY_SQLITE_STORE_RAW=false` env var overriding a config
    `store_raw: true` in `environment: dev` → confirms env beats config beats environment
    default, in that exact precedence order (§4.2).
20. OpenTelemetry span parenting under an unnamed `flow: tree` loop with ≥2 concurrent branches
    sharing one state path → confirms the `(thread_id, span, context)` disambiguation (§4.4) by
    asserting each branch's span has the correct parent and none is double-closed or orphaned.
21. `--live-state` under a run with effects completing faster than `LIVE_STATE_INTERVAL_SECONDS`
    → the mirror file must still equal `--out` exactly at run end (coalescing never drops the
    *final* state, only intermediate ones) — assert by comparing the mirror file's content
    immediately after the run exits to the `--out` file's content, not by counting writes.

**Redaction (cross-cutting, run against every case above)**

22. Every scripted fake that includes an `Authorization: Bearer sk-live-...`-shaped value,
    somewhere in a tool/adapter's `raw`, must come out of `meta.raw`/`meta.params_rendered`
    as `"***REDACTED***"` — run this assertion as a blanket post-check on *every* conformance
    case's final state dump, not as a separate case, so a newly added tool/adapter can't
    accidentally skip it.

---

## Open items for confirmation

**Superseded.** Both items below were open when this section was first written; DESIGN.md §13
and §14 record them as settled from the production document sets this project targets: no
production document's `runtime:` block relies on being filtered (it only ever sets
`state.record_children`), and `cyberdiner` **is** in use, so §3.8 is in scope for v1, not
deferred.

- Whether any current production document relies on `trust_orchestration_
  runtime` or an orchestration-level `runtime:` override reaching electricity — electricity's
  "both files operator-chosen = trusted" model suggests not, but this spec can't confirm it
  without reading those documents' `runtime:` blocks directly.
- Whether `cyberdiner` is actually used by production documents (confirm against the project's
  own tool/model list) — if not in use, §3.8 can be deferred past v1 despite being fully
  specified here.
