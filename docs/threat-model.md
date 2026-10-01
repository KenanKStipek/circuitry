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
`subprocess.run(list_args)` shape — no shell — and goes through the same
[`circuitry/curl_support.py`](../src/circuitry/curl_support.py) plumbing
described below, so its workflow payload (which embeds the rendered prompt)
never lands on the curl command line either.

**Residual risk.** A user-authored plugin not bundled with Circuitry has no
forced sandbox and can do whatever Python lets it do. The `ToolPlugin`
Protocol is contract, not enforcement. Users who load third-party plugins
should treat them like any other dependency: read the code first.

### 3. Credential handling

Adapter credentials (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, etc.) are read
from the process environment by each adapter. The reads happen at adapter
instantiation, so a missing key fails loudly with a hint instead of silently
sending an unauthenticated request.

**Mitigation — curl never puts a secret or a body on its own command
line.** Every curl-based adapter (`openai`, `anthropic`, `ollama`,
`replicate`, `watsonx` — including its IAM token exchange — and the ~20
providers that share transport via
[`adapters/_openai_compat.py`](../src/circuitry/adapters/_openai_compat.py))
and curl-based tool plugin (`comfyui`, `web_search`, `weather`) shells out
through [`circuitry/curl_support.py`](../src/circuitry/curl_support.py)'s
`run_curl()`, which always passes `-q` first (so a local user's
`~/.curlrc` can't silently redirect output or inject a proxy), sends the
request body on stdin via `--data-binary @-` instead of `-d`/`-F` on argv
(this also removes Linux's 128 KiB-per-argument ceiling for a large prompt
or base64 image), and sends every header — `Authorization`, `x-api-key`,
any provider-specific credential header — through an inherited pipe file
descriptor via `--config /dev/fd/<n>` rather than `-H`. None of this is
ever visible in `ps` for the duration of the call.

**Mitigation — error masking.** Every curl failure raises through
[`circuitry/curl_support.py`](../src/circuitry/curl_support.py)'s
`curl_failure_message()`, which never echoes the curl command line — the
one place a secret could otherwise have leaked before the mitigation
above — and instead reports the adapter/plugin name, model (adapters
only), target URL (userinfo stripped, credential-like query parameters
such as a search API key masked) and a parsed form of the provider's error
body. Credential values passed on the request are also stripped from that
body/stderr text as a second layer, in case a provider ever echoes back
what it was sent. `meta.error` and `meta.fallback_attempts[].error`, which
land in run state on every prompt failure, are additionally passed through
the redaction helper (below) before being stored.

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

An orchestration document is not always the operator's own: `cof run-library`,
a library name served by a github or folder source, `use: ref:`, an MCP or REST
caller and a model-generated plan all run documents someone else wrote or
chose. Host settings (`config.json` layers and `CIRCUITRY_*` environment
variables) decide where an adapter sends prompts and credentials, which binary
or environment a tool plugin gets, which MCP server commands start, where state
is persisted and which Python modules are imported.

**The rule.** A file you run by path is trusted like a script you run:
`cof run ./my.yml`, `cof check` / `cof score` on a path, a local file picked
in the TUI's Run view, the SDK's `run_orchestration(orchestration_path=...)` /
`validate_orchestration` and scheduler jobs apply the document's whole
`runtime:` block and `plugins:` list. A fetched, library, generated or
tool-chosen document is limited: `cof run <library name>`, `cof run-library`,
`run_shared_orchestration`, the MCP `run_orchestration` / `validate_orchestration`
tools, the REST trigger and the TUI Library view's "run this entry" (bundled,
folder or github source alike) cannot change host settings. The TUI Chat
view's "run it now" hand-off is limited too, even once the draft is saved to
disk: the document is still what the model just generated, not something you
named by path. Save it, then run that same file with `cof run f.yml` or pick
it from the Run view's local-file list, and it trusts like any other local
file. `cof fetch` followed by `cof run ./fetched.yml` is running the file by
path, so read a fetched file before you run it that way.

**Mitigation.** A limited document's `runtime:` block contributes only the
author-level keys in `ORCHESTRATION_RUNTIME_KEYS` — `runtime.complexity` and
`runtime.state`. Every other key (`adapters`, `plugins`, `persistence`,
`library`, `mcp`, unknown keys) is dropped from the effective settings, with
one warning per key in `cof run` (stderr) and `cof check` output, and its
top-level `plugins:` list only loads runtime-plugin modules config already
lists in `plugins` or `enabled_plugins`. A trusted document that applies host
settings is announced: `cof run` (stderr) and `cof check` print one line,
`Applied host settings from <file>: ...`, naming the key paths (never values,
so no secret appears in it). A `use:` child runs on the parent's resolved
runtime and its own `runtime:` / `plugins:` keys are never read, whether the
parent is trusted or not; generated reflector and decompose plans never
contribute them either. Trust is an explicit input (`RunRequest.trust_document`,
default `False`), set only by the entry points above, so an internal or
programmatic caller that does not say otherwise is limited. Implementation:
[`src/circuitry/cli/effective_settings.py`](../src/circuitry/cli/effective_settings.py).

**Residual risk.** A path is trusted whoever wrote the file: a document
cloned with a repository or downloaded runs with full host authority once you
name it by path — including a document the TUI's Chat view generated, once
you save it and then run that saved file by path instead of through Chat's
own "run it now" hand-off. The notice makes that visible but does not stop
it. `trust_orchestration_runtime: true` in config (or
`CIRCUITRY_TRUST_ORCHESTRATION_RUNTIME=1`) extends trust to every document the
host runs, including library names, MCP and REST callers and everything
reachable through `use: ref:`. Enable it only on a host that runs nothing but
documents its operator wrote or has reviewed. Within the boundary, a document
still uses whatever config allows — the adapters, tools and plugins config
enables — so the allowlists remain the way to narrow that.

### 7. Project config files

`circuitry.config.json` or `config.json` in the working directory is
discovered automatically, and a config file carries full host authority:
adapter endpoints (and so where prompts and credentials go), tool plugin
binaries and environments, MCP server commands, persistence targets, library
sources and runtime plugin imports. A cloned or downloaded repository can ship
one, so "run anything in this directory" would otherwise mean "apply settings
nobody reviewed".

**Mitigation.** A discovered project config applies only once the user has
trusted it with `cof trust`, which prints every setting the file makes and
flags the host-sensitive ones before asking. Trust is recorded in
`~/.config/circuitry/trusted.json` (created `0600` in a `0700` directory,
beside the global config, which is never rewritten) as the file's resolved
absolute path plus the SHA-256 of its contents, so any edit — or the same
contents at another path — needs trusting again. An untrusted or changed file
is skipped whole, with one warning naming the file and the `cof trust` command;
the run continues on the remaining layers and exit codes do not change.
`cof doctor` and the TUI show the file's trust state. The global config and a
file named with `--config` or `CIRCUITRY_CONFIG` are always trusted: the user
chose them. Implementation:
[`src/circuitry/cli/config_trust.py`](../src/circuitry/cli/config_trust.py).

**Residual risk.** `CIRCUITRY_TRUST_PROJECT_CONFIG=1` trusts every discovered
project config without a trust entry — the protection is off wherever it is
set. Use it only in CI jobs and containers whose checked-out repository is the
user's own, never in a shell profile. Trust is by content, not by review:
`cof trust --yes` records a file nobody read. Nothing stops a *trusted* file's
directory from also containing hostile orchestrations; trust covers the config
file only.

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
