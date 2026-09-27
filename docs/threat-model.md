# Threat Model

This document describes the attack surfaces of the Circuitry framework, the
mitigations in place, and the threats Circuitry deliberately does not defend
against. It is intentionally narrow: it covers the framework, not user-authored
orchestrations or model behavior. Reports of issues outside this scope should
go to the upstream maintainers (the model provider, the user's plugin author,
the user themselves).

For private vulnerability disclosure, see [`SECURITY.md`](../SECURITY.md).

---

## Privacy & telemetry

Circuitry **collects no telemetry**. There is no opt-in or opt-out toggle —
the code does not emit usage events, crash reports, model identifiers, or
network beacons of any kind. The only outbound traffic Circuitry initiates is
to:

- the configured LLM adapter (Ollama, OpenAI, Anthropic, LiteLLM)
- the configured tool plugin endpoint (ComfyUI for image generation, ffmpeg
  invoked locally)
- the shared-library service when `cof fetch`/`cof run-library` is invoked
  with a configured library URL
- the persistence backend (Postgres or SQLite) when configured

Users can audit this themselves with a sniffer; the framework itself adds no
hidden network calls.

---

## In-scope attack surfaces

### 1. Inline orchestration execution (`use(inline:)`)

The `use` effect can execute orchestration YAML produced at runtime — for
example, YAML rendered from a template, or YAML emitted by a previous LLM
call. This is intentional (it enables LLM-generated plans), but it is the
single most powerful primitive in the framework.

**Mitigation.** Inline YAML is validated against the orchestration JSON
Schema by default. The validation gate is in
[`src/circuitry/core/use.py:234-240`](../src/circuitry/core/use.py) and runs
unconditionally unless the orchestration sets `validate: false` on the `use`
effect. The validator's implementation is at
[`src/circuitry/core/use.py:56-104`](../src/circuitry/core/use.py).

**Residual risk.** A user who explicitly opts out (`validate: false`) bypasses
the schema check. Schema validation also does not protect against
*semantically* malicious orchestrations — for example, a `use(inline:)` that
references an attacker-controlled bundled tool plugin path. A model that
is jailbroken into emitting `validate: false` could escape this gate.

**Recommendation.** Never set `validate: false` for inline YAML produced by
an LLM unless you have an out-of-band check (e.g. a downstream `if` that
allow-lists the operations). Treat `use(inline:)` like `eval()` — keep it
on a tight leash.

### 2. Tool plugin shell-out

The bundled `ffmpeg` and `comfyui` plugins shell out to external binaries
(`ffmpeg`) or HTTP services (ComfyUI). Both take user-supplied parameters
and are therefore an injection surface.

**Mitigation — ffmpeg.** All arguments are passed as a list to
`subprocess.run` (no shell), see
[`src/circuitry/plugins/ffmpeg.py:138`](../src/circuitry/plugins/ffmpeg.py).
File path arguments and CLI scalars go through `_check_safe()` at
[`ffmpeg.py:17`](../src/circuitry/plugins/ffmpeg.py), which rejects shell
metacharacters even though the no-shell invocation already prevents
expansion. Drawtext text values go through `_escape_drawtext_text()` at
[`ffmpeg.py:36`](../src/circuitry/plugins/ffmpeg.py) — text is wrapped in
double quotes inside the filter graph; colons and apostrophes are safe
because no shell layer interprets them.

**Mitigation — ComfyUI.** Image-path parameters go through
`_validate_image_path()` at
[`src/circuitry/plugins/comfyui.py:31`](../src/circuitry/plugins/comfyui.py)
and image-directory parameters go through `_validate_image_dir()` at
[`comfyui.py:59`](../src/circuitry/plugins/comfyui.py). These reject paths
escaping the configured working area. The HTTP transport uses standard
`subprocess.run(list_args)` shape at
[`comfyui.py:135, 165, 202`](../src/circuitry/plugins/comfyui.py) — no
shell.

**Residual risk.** A user-authored plugin not bundled with Circuitry has no
forced sandbox and can do whatever Python lets it do. The `ToolPlugin`
Protocol is contract, not enforcement. Users who load third-party plugins
should treat them like any other dependency: read the code first.

### 3. Credential handling

Adapter credentials (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, etc.) are read
from the process environment by each adapter. The reads happen at adapter
instantiation, so a missing key fails loudly with a hint instead of silently
sending an unauthenticated request.

**Mitigation — error masking.** Every curl-based adapter (`openai`,
`anthropic`, `ollama`, `replicate`, `watsonx`, and the ~20 providers that
share transport via
[`adapters/_openai_compat.py`](../src/circuitry/adapters/_openai_compat.py))
raises through
[`adapters/_curl_errors.py`](../src/circuitry/adapters/_curl_errors.py),
which never echoes the curl command line — the one place an
`Authorization`/`api-key`/Bearer header value could leak — and instead
reports the adapter, model, target URL (credential-free) and a parsed form
of the provider's error body. Credential values passed on the request are
also stripped from that body/stderr text as a second layer, in case a
provider ever echoes back what it was sent. `meta.error` and
`meta.fallback_attempts[].error`, which land in run state on every prompt
failure, are additionally passed through the redaction helper (below)
before being stored.

**Mitigation — state serialization.** The `runtime.effective_settings`
snapshot embedded in run state (and surfaced via `--out`, `--json`,
`--live-state`, and the `~/.config/circuitry/last-run.json` replay file)
goes through a centralized redaction helper before being written. The
helper deny-lists keys matching `api_key`, `token`, `password`, `secret`,
`bearer`, and `authorization` (case-insensitive, with snake/kebab/dot
suffixes), and rewrites any URL containing userinfo
(`https://user:pass@host`) to strip the userinfo segment. JWTs and common
vendor key shapes (`sk-…`, `ghp_…`, `xox?-…`) in any string position are
also redacted. Implementation:
[`src/circuitry/cli/redaction.py`](../src/circuitry/cli/redaction.py).

**Mitigation — `--last` env-var stash.** The `cof run -e KEY=VALUE` list is
also redacted before being written to `last-run.json`. If a previous run
included a redacted secret, `cof run --last` aborts loudly with guidance
rather than silently passing the redaction sentinel through as a literal
value.

**Residual risk.** The redaction helper is a deny-list, not a guarantee. A
user who passes an unusual secret-bearing key (e.g. `mySigningJwt` —
mixed-case, no separator, no canonical suffix) might slip through. The
recommended posture is unchanged: pass credentials via environment
variables, never via `-e`, and never check a populated `config.json` into
a git repo.

### 4. Persistence backends

When `runtime.persistence` is configured, run snapshots are written to
SQLite or Postgres. The redaction step (above) runs before serialization,
so the persisted snapshot also has redacted credentials. The persistence
adapter trusts the configured connection string — Circuitry does not
sanitize it beyond the standard `redact()` URL-userinfo strip on the
*serialized* copy.

### 5. Shared-library fetch

`cof fetch` and `cof run-library` download orchestration assets from a
configured shared-library service. The fetched orchestration is loaded and
then validated against the JSON Schema before execution. There is no
signature verification today — anyone with write access to the shared
library can publish an asset. Treat shared-library assets the same way you
would treat any third-party orchestration: prefer fetching from a library
you control, or read the YAML before running it.

### 6. Host settings versus orchestration documents

An orchestration document is not always the operator's own: `cof fetch`,
`cof run-library`, `use: ref:` and github library sources all run documents
someone else wrote. The trust boundary is that **host settings come only
from config** (`config.json` layers and `CIRCUITRY_*` environment variables);
a document cannot change them.

**Mitigation.** A document's `runtime:` block contributes only the
author-level keys in `ORCHESTRATION_RUNTIME_KEYS` — `runtime.complexity` and
`runtime.state`. Every other key (`adapters`, `plugins`, `persistence`,
`library`, `mcp`, unknown keys) is dropped from the effective settings, with
one warning per key in `cof run` (stderr) and `cof check` output. A
document's top-level `plugins:` list only loads runtime-plugin modules config
already lists in `plugins` or `enabled_plugins`. A `use:` child runs on the
parent's resolved runtime and its own `runtime:` / `plugins:` keys are never
read. So a document decides what a run does, but not where an adapter sends
prompts and credentials, which binary or environment a tool plugin gets,
which MCP server commands start, where state is persisted, or which Python
modules are imported. Implementation:
[`src/circuitry/cli/effective_settings.py`](../src/circuitry/cli/effective_settings.py).

**Residual risk.** `trust_orchestration_runtime: true` in config (or
`CIRCUITRY_TRUST_ORCHESTRATION_RUNTIME=1`) switches the boundary off: every
document the host runs, including everything reachable through `use: ref:`,
can then set any host setting and import any module. Enable it only on a host
that runs nothing but documents its operator wrote or has reviewed. Within the
boundary, a document still uses whatever config allows — the adapters, tools
and plugins config enables — so the allowlists remain the way to narrow that.

---

## What Circuitry does NOT defend against

These are deliberate non-goals. Reports about them will be acknowledged but
treated as expected behavior, not vulnerabilities.

- **Malicious user-authored orchestrations.** A user with the ability to
  write a `.yml` file in the project directory can do anything the host's
  config lets Circuitry do — invoke the configured models, shell out via
  enabled tool plugins, download from the shared library, etc. Circuitry runs
  YAML it is told to run; what a document cannot do is change the host's
  config (see 6 above).
- **Models that exfiltrate data via tool calls.** A model can choose to
  emit any arbitrary string for any tool argument. If the orchestration
  trusts model output enough to feed it into a tool call, Circuitry will
  carry out that call.
- **Prompt injection.** If untrusted text reaches a prompt template,
  the model may follow injected instructions. The framework provides no
  prompt-injection mitigation; that responsibility belongs to the
  orchestration author (input sanitization, output schema enforcement,
  defensive system prompts).
- **Resource exhaustion from runaway loops.** `max_iterations` is a cap the
  orchestration author sets, not a built-in floor — a loop with no cap runs
  until its collection or condition ends it. An `each` loop walks a
  collection resolved before its first pass, so a large one can run long
  but not forever; a `while` loop whose condition never goes false runs
  indefinitely.
- **Network-level attacks on adapter endpoints.** TLS validation is
  delegated to the underlying transport (`urllib`, `httpx`, or the vendor
  SDK). Misconfigured TLS in the user's environment is the user's problem.
- **Side-channel attacks against models.** Timing, token-count, or
  cache-state leaks from third-party model providers are out of scope.

---

## Known limitations of the redaction helper

The deny-list is intentionally conservative; it favors false negatives over
false positives so that benign config doesn't get garbled. Specifically:

- It does not detect arbitrary high-entropy strings.
- It does not cover every vendor-specific API-key format — common ones are
  hardcoded; rarer ones may pass through.
- It does not strip secrets that appear inside *non-string* values (the
  helper walks dicts/lists/strings only).
- It does not cover secrets that arrive as part of a model response and
  end up persisted in `prime.<effect>.value`.

If you need stronger guarantees, use a real secret-scanner (e.g. `gitleaks`)
on artifacts before publishing them.
