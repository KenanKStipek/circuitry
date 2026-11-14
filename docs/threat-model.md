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

- the configured LLM adapter — every compiled-in adapter (`cof list --extensions`), not just Ollama/OpenAI/Anthropic/LiteLLM
- whichever tool plugin an orchestration's `tool:` effects configure, when that plugin reaches the network or a local binary (not just ComfyUI/ffmpeg — see `docs/plugins/` and `src/circuitry/plugins/` for the full set)
- the shared-library service when `cof fetch`/`cof run-library` is invoked
  with a configured library URL
- the configured persistence backend (`jsonl-file`, `mongodb`, `postgres`, or `sqlite`) when configured
- any runtime plugin you enable (observability exporters such as Sentry, Datadog,
  Honeycomb, Loki or CloudWatch, and the persistence and pub/sub plugins such as
  S3, GCS or Redis): each sends run data to the service it names, and only once
  enabled. See [`runtime-plugins.md`](./runtime-plugins.md)
- the library sources you configure, when `cof library refresh` fetches them
  (for example a GitHub source)

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

**Mitigation — shell.** The `shell` plugin runs one binary from an allowlist
(tiny and read-only by default) with no shell layer (`shell=False`,
shell-meta characters rejected in args). A document's own `params` may
widen the allowlist per-effect, but only from a literal, unrendered list —
never `params_json` (runtime-built via `#203`, can carry model-generated
content) and never a list entry that is still a Mustache tag — both are a
hard error rather than silently accepted. This is the *author's own*
document's own widening; a document produced while the run executes — a
reflector's plan, a `use` whose `inline:` is rendered from state, or a
decomposition plan — is narrower still, since its own `allowed_commands`
can look exactly as literal as an author's own: see
[Section 11](#11-generated-documents). A host may additionally pin
`runtime.plugins.shell.allowed_commands` in config; when set, the effective
allowlist is that pin intersected with the effect's own list, so a *limited*
document can only narrow it further, never widen it past the host's pin. The
pin is a ceiling even for a *trusted* document (see
[Section 6](#6-host-settings-versus-orchestration-documents)): unlike every
other `runtime.plugins`/`runtime.adapters` key, which a trusted document's
own value overrides key by key, its `runtime.plugins.shell.allowed_commands`
intersects with the host's pin rather than replacing it — the host's
allowlist is the one thing no document, trusted or not, can widen.
Implementation: [`src/circuitry/plugins/shell.py`](../src/circuitry/plugins/shell.py),
[`src/circuitry/core/tool.py`](../src/circuitry/core/tool.py) (the
`params_json`/templated rejection).

**The `agent` tool is not sandboxed.** It runs a whole pi or Claude Code
session with the user's own permissions: the agent can edit or delete any
file the user can, run any program and reach the network, and its `cwd` is
where it starts, not a boundary. The narrowing is the engine's own
tool lists — pi's `--tools`/`--exclude-tools`; for Claude Code, `--tools`
(an allowlist, its entries also pre-approved with `--allowedTools`) under
`--permission-mode dontAsk` by default, where a specifier such as
`Bash(pytest:*)` limits its tool only in that mode, and `--disallowedTools`,
a hard deny in every mode — and, unlike `shell`'s allowlist, those params
may be templated or come from `params_json`. `extra_args` come after these
flags, so one of them (`--tools`, `--permission-mode`, `--setting-sources`) can
override them and widen the session. A Claude Code session also
ignores its repository's own settings (hooks, the API key helper, project MCP
servers) unless `trust_project_settings` is set, and gets the repository
root's `CLAUDE.md` appended to its prompt; without `tools`, Claude Code's own
default mode (`auto`) decides which calls run. Circuitry therefore tags `agent` with the `shell`,
`fs-write` and `network` capabilities, so a document that is not the
user's own needs the same consent to run one as to run `shell` (see
[Section 9](#9-capability-consent-for-a-document-that-is-not-the-users-own)),
and `enabled_tools` can leave it out altogether. Like the coding-agent
adapters below, it passes no credential and keeps the prompt off argv.
Implementation: [`src/circuitry/plugins/agent.py`](../src/circuitry/plugins/agent.py);
user-facing detail in [`docs/plugins/agent.md`](plugins/agent.md).

**Residual risk.** A user-authored plugin not bundled with Circuitry has no
forced sandbox and can do whatever Python lets it do. The `ToolPlugin`
Protocol is contract, not enforcement. Users who load third-party plugins
should treat them like any other dependency: read the code first.

### 3. Credential handling

Adapter credentials (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, etc.) are read
from the process environment by each adapter. The reads happen at adapter
instantiation, so a missing key fails loudly with a hint instead of silently
sending an unauthenticated request.

**Mitigation — curl never puts a secret, a URL, or a body on its own
command line.** Every curl-based adapter (`openai`, `anthropic`, `ollama`,
`replicate`, `watsonx` — including its IAM token exchange — and the ~20
providers that share transport via
[`adapters/_openai_compat.py`](../src/circuitry/adapters/_openai_compat.py))
and most of each curl-based tool plugin's calls (`comfyui`'s JSON calls,
`web_search`, `weather`) shell out through
[`circuitry/curl_support.py`](../src/circuitry/curl_support.py)'s
`run_curl()`, which always passes `-q` first (so a local user's
`~/.curlrc` can't silently redirect output or inject a proxy), sends the
JSON request body on stdin via `--data-binary @-` instead of `-d` on argv
(this also removes Linux's 128 KiB-per-argument ceiling for a large prompt
or base64 image), and sends the URL and every header — `Authorization`,
`x-api-key`, any provider-specific credential header, a query-string
credential such as `web_search`'s `extra_params`, `user:pass@` in a
configured `base_url` — through a `--config` file rather than argv or
`-H`. On POSIX that file travels through an inherited pipe file descriptor
(`--config /dev/fd/<n>`), never a temp file; Windows has no such fd, so
there it's a file written to the per-user temp directory (`%TEMP%`,
private to the user by default ACL) and removed in a `finally` once curl
is done with it. Argv carries no request data at all, so nothing from this
source is ever visible in `ps` for the duration of the call. `comfyui`'s
image fetch (`_curl_bytes`), its one multipart upload (`_upload_image`,
`-F image=@<path>` — the local file path, not its contents, on argv) and
its `check()` HEAD probe call curl directly rather than through
`run_curl()`, since none of the three sends a JSON body or a header that
needs to stay off argv; all three still pass `-q` first. Their target URL
is built from `base_url`, which is operator-configured (ComfyUI behind an
authenticating proxy, say) and so can itself carry `user:pass@` — unlike
the other curl-based adapters/plugins above, these three calls don't route
that case off argv.

**Coding-agent CLI adapters (`pi`, `claude_code`) pass no credential at
all.** They run the configured CLI binary, which authenticates with its
own login. The prompt goes on stdin (`claude_code`) or in a file inside a
fresh temporary directory, private to the user and removed after the call
(`pi`) — never on argv. The child's environment drops `ANTHROPIC_API_KEY`
and `ANTHROPIC_AUTH_TOKEN` by default (`unset_env`), so a key exported for
another adapter is not handed to the CLI. `claude_code` passes
`--strict-mcp-config`, because `--tools ""` turns off only Claude Code's
built-in tools: without it, MCP servers from the user's Claude Code config
or a project `.mcp.json` would start and their tools would be offered to
the model. Which binary runs is
`runtime.adapters.<name>.binary`, a host setting a limited document cannot
set (see [What a document can set](guidebook/04-configuration.md#what-a-document-can-set)).
An alias of either adapter (`runtime.adapters.<alias>.adapter`) can add
variables to that child's environment (`env`), for instance a config
directory that selects another login. An alias's target is not limited to
`claude_code`/`pi` — it can name any adapter except `host_claude` (supplied
at run time by `circuitry-mcp`, never built from config), so the same
boundary covers an alias of, say, `openai` pointed at a different
`base_url`. That is the same host-setting boundary: a limited document
cannot define or retarget an alias, and `env` values are blanked in the
saved `effective_settings` and kept out of events, logs, error messages
and the "Applied host settings" notice. With `enabled_adapters` set, a
document — trusted or not — cannot define or retarget an adapter alias at
all, even one already on the allowlist: without that rule, a trusted
document could point an allowlisted alias at an adapter the allowlist
doesn't list, such as `openai` with its own credential.

**`api_key_env` is config-only, regardless of `enabled_adapters`.** Any
keyed adapter's own block — built-in or alias — accepts an
`api_key_env`, naming the environment variable it reads its credential
from in place of the provider's default (`runtime.adapters.<name>
.api_key_env`). This is honoured only from config: a document's own
attempt to set it is always ignored, with a warning, whether or not
`enabled_adapters` is set, and whether or not the document is otherwise
trusted — unlike the alias-retarget rule above, there is no case in
which a document chooses it. The reason is narrower than the alias rule
but just as direct: a document run by path can already set `base_url`,
pointing a request at any host it names; if it could also choose
`api_key_env`, it could send any environment variable the host
process can read — a GitHub token, a cloud credential, anything, not
just that provider's own key — as the Bearer value to a URL the
document itself controls. Keeping the choice of *which* variable a
config-only decision means a document can at most misuse a key the
host already intended that adapter to send.

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
you control, or read the YAML before running it. [Section 9](#9-capability-consent-for-a-document-that-is-not-the-users-own)
covers the consent gate a `cof run-library` asset's `shell`/`python_eval`/
`fs-write`/`network` tool effects go through before they run.

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
`runtime:` block and `plugins:` list — unless that path resolves (symlinks
followed) inside a refreshable (e.g. `github`) library source's own cache
directory: that is fetched content regardless of what string named it, the
same content a bare library-name run already limits, so it stays limited
too (#343), on every surface built on `runtime_shim.run` (CLI, TUI, SDK, MCP,
REST) and on `cof check` / `validate_orchestration`'s own report of what a
run would do, and a `cof run <path> --resume <run-id>` lookup of the
document's `runtime.persistence` backend. A fetched, library, generated or
tool-chosen document is limited: `cof run <library name>`, `cof run-library`,
`run_shared_orchestration`, the MCP `run_orchestration` / `validate_orchestration`
tools, the REST trigger and the TUI Library view's "run this entry" (bundled,
folder or github source alike) cannot change host settings. The TUI Chat
view's "run it now" hand-off is limited too, even once the draft is saved to
disk: the document is still what the model just generated, not something you
named by path. Save it, then run that same file with `cof run f.yml` or pick
it from the Run view's local-file list, and it trusts like any other local
file. `cof fetch` followed by `cof run ./fetched.yml` is running the file by
path, so read a fetched file before you run it that way — unless the saved
copy's path itself happens to land inside a configured source's cache
directory, which stays limited for the reason above.

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
programmatic caller that does not say otherwise is limited — and
`runtime_shim.effective_document_trust` overrides it back to `False` itself
when the resolved path is a library source's cache path (above), so a caller
that passes `trust_document=True` for a path it did not actually choose (the
SDK's default) cannot apply a fetched document's settings by believing it
owns the path. `run()`, `validate()` (so `cof check` and
`validate_orchestration` agree with what a run would do) and the CLI's
`--resume <run-id>` persistence lookup (which resolves before `run()` ever
sees the document) all go through this one function. Implementation:
[`src/circuitry/cli/effective_settings.py`](../src/circuitry/cli/effective_settings.py)
and [`src/circuitry/cli/runtime_shim.py`](../src/circuitry/cli/runtime_shim.py).

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
enables — so the allowlists remain the way to narrow that. Because
`plugins`/`adapters` merge key by key rather than block by block, a trusted
document that sets only one field deep inside a block — an MCP server's
`command`/`url` under `plugins.mcp.servers.<name>`, say, or an adapter's
`base_url` — inherits the rest of that block (`env`, headers, credentials)
from the host rather than dropping it; this is the deep merge working as
intended for a trusted document, not a leak, but it means a trusted
document's endpoint choice can run against the host's existing credentials
for that block.

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

The global config and `.env` `cof setup` writes carry the same credential risk
on a shared machine: both are created mode `0600` in a `0700`
`~/.config/circuitry/` (tightening either file's mode if it already existed
looser from before this), and `cof doctor` warns if either is still group- or
world-readable. Implementation:
[`src/circuitry/cli/setup.py`](../src/circuitry/cli/setup.py).

Every host entry point (the `cof` CLI, the TUI, the MCP server, the REST
trigger service) loads that one `.env` at startup — never a `.env` from the
working directory or a document's directory, and never automatically by the
SDK (`circuitry.api`), since an embedding program owns its own environment.
A variable already set in the real environment always wins. A file not
owned by the current user, or writable by group or others, is refused rather
than loaded, with one warning naming the file and `chmod 600`; a file merely
readable by group or others is still loaded, with the same warning. `cof
doctor` reports whether the file was loaded and the variable *names* it
supplied or skipped — never a value. Implementation:
[`src/circuitry/cli/config.py`](../src/circuitry/cli/config.py) (`load_user_env`).
(#348)

### 8. The REST trigger service

`circuitry.service.RestTriggerService` is a building block for embedding a
network-reachable trigger into a host's own HTTP stack ([Surfaces](guidebook/12-surfaces.md#rest-and-scheduling)) — it is not a deployment
by itself, but a host that wires it up is exposing `orchestration_path` (and
optionally `out_path`) to whoever can reach the endpoint.

**Mitigation.** A falsy `auth_token` no longer means "no check": construction
raises unless the embedder passes a token or explicitly opts out with
`allow_unauthenticated=True`, so there is no accidental open trigger from an
unset env var. Every request's `orchestration_path`/`out_path` is resolved
(relative paths against `orchestration_root`, absolute paths as given) and
must land inside `orchestration_root` — the service's working directory at
construction, or an explicit path — or the request is refused with a 400
before anything runs. `config=`, when the embedder omits it, is no longer a
bare, allowlist-open `CircuitryConfig()`: the service resolves the host
config the same way `cof run` would for a document under
`orchestration_root` (global config, then a project config discovered there
if trusted — §7's trust rules — then environment variables), so a host's
allowlists, `runtime.plugins.shell.allowed_commands` pin, and every other
host setting apply, and preflight — gated on a non-`None` config — always
runs for a non-dry-run request. An embedder that passes an explicit `config`
has that win outright. Implementation:
[`src/circuitry/service/rest.py`](../src/circuitry/service/rest.py).

**Residual risk.** `orchestration_root` confines *which file* a request can
name; the document it points to is otherwise limited the same way any
REST/MCP-reached document is (see §6 above). A host that wires this up still
needs to choose its own transport-level protections (TLS, network ACLs) —
this class has none.

### 9. Capability consent for a document that is not the user's own

Section 6's trust rule decides whose *host settings* a document's
`runtime:`/`plugins:` can touch. It says nothing about the document's own
effects: before #275, a `cof run-library` asset or a `use: ref:` child ran
its `shell`/`python_eval`/filesystem-write/network tool effects exactly like
a document the operator wrote themselves — no prompt, no record that anyone
saw it coming.

**Mitigation.** Every bundled tool plugin is tagged, in the one place
(`circuitry.plugins.capabilities.PLUGIN_CAPABILITIES`, next to the plugin
registry so the tag can't drift from it), with what it can do to the host:
`shell` (runs an external binary with effect-supplied arguments — the
`shell` plugin and every "subprocess wrapper" in `circuitry.plugins.factory`),
`python_eval` (evaluates document-supplied Python), `fs-write` (writes or
deletes an arbitrary local path without shelling out), and `network`
(reaches a remote host: the HTTP family, a cloud/SaaS SDK, a DNS/ping-style
probe, a download). Before the first run of a document that did not come
from the user's own disk — a `cof run-library`/`run_shared_orchestration`
asset, a remote (refreshable, e.g. `github`) library source run by bare
name (`cof run hub:entry`), or any `use: ref:` child, reached from *any*
document, trusted or not — `circuitry.cli.document_consent.enforce_consent`
works out, statically,
every capability the compiled document and its reachable `use` children need,
and gates it: an interactive `cof run`/`cof run-library` lists them and asks
y/N; anything else (MCP, REST, CI, no TTY) refuses outright, naming
`cof trust <document>` (extended by #275 to accept a `.yml`/`.yaml` path, not
just a project config) and the `--allow-capabilities shell,network,...`
scripted/CI escape hatch (never persisted — it approves one run, not the
document). A yes is recorded by the document's own content digest in the
same store `cof trust` keeps project-config trust in
(`~/.config/circuitry/trusted.json`, a different top-level key), so an
edited document asks again. A `ref:` child gets its own entry, independent of
whatever document pulled it in — including one reached from a path-trusted
document, since the child itself is still someone else's content. A plan a
reflector or decomposition step generates inside a consented document is
held to a ceiling: the same capability set already consented for the
enclosing document, never its own prompt. Implementation:
[`src/circuitry/cli/document_consent.py`](../src/circuitry/cli/document_consent.py),
[`src/circuitry/capability_gate.py`](../src/circuitry/capability_gate.py),
[`src/circuitry/plugins/capabilities.py`](../src/circuitry/plugins/capabilities.py).

**Residual risk.** The tag table is current best judgement, not a formal
proof: a plugin not bundled with Circuitry carries no tag at all (needs
none, by the same rule untagged stdlib-only plugins do) and a user-authored
one should be read before it is trusted the way any new dependency would be.
Consent is about *capability class*, not about what a specific call does
with it — approving `network` for a document approves every networked tool
it uses, not just the one the user had in mind. A `use: path:` child and a
generated (`inline:`) plan are not independently gated — only bound to
whatever ceiling the run already carries — because they are the enclosing
document's own content, the same reasoning that limits them from setting
their own host settings (§6). `cof fetch` writing a file to disk, then a
later `cof run ./that-file.yml`, is a run by path (§6) and is never gated
here, whatever the file's origin — read a fetched file before running it
that way, or run it through `cof run-library`.

**The remote-library-source gate now covers every surface (#334).** It was
wired only into `cof run`'s own resolution at first; the residual this
noted — the TUI Library view's "run this entry", the SDK, MCP, and REST
reaching a remote source's resolved path without going through it — is
closed: the TUI's Library view (and the Runs view's replay of a stashed
`cof run`) resolves a bare name the same way `cof run` does and carries
whether it came from a refreshable source through the hand-off;
`circuitry.api.run_orchestration`/`run_shared_orchestration` take an
`allow_capabilities=` parameter, since the embedding program is itself the
host deciding what runs; and the MCP server's `run_orchestration` tool
resolves a bare name against the same registry `cof run` builds and applies
the same gate. Neither MCP nor the REST trigger exposes any field a caller
can use to grant capabilities on their own behalf — only `cof trust`'s
store, consulted by content digest, can do that; a document whose digest
was never consented there simply refuses, naming `cof trust <document>` the
same way `cof run` does from a script or CI. The TUI refuses the same way
rather than opening a dialog — a deliberate choice, not a technical limit
(a modal could run on the UI thread before the worker starts): the same
refusal every scripted/CI surface gets keeps one message and one path to
approve a document (`cof trust <document>`) instead of a second,
TUI-only grant mechanism. REST's own
`orchestration_path` is always a file under the service's
`orchestration_root`, never a name resolved against a library source, so it
stays a run by path (§6) at the top level regardless of surface — a
`use: ref:` child reached from it is still independently gated, same as
every other surface.

**The cache-path hole (#337).** A refreshable (`github`-type) `library.sources`
entry is served from a SHA-pinned local cache directory
(`circuitry.cli.github_source.GitHubSource.cache_dir`); naming that cached
file by its raw, resolved (symlinks followed) path — instead of the library
name `cof run hub/entry` resolves — used to look exactly like a run by path
(§6): trusted, ungated, no consent asked. An MCP caller can name an absolute
path just as easily as a name, and is not the host. `circuitry.cli.
runtime_shim.run()` now classifies a resolved `orchestration_path` the same
way regardless of which string named it: a path inside any configured
refreshable source's cache directory
(`LibraryRegistry.is_cache_path`) gets the whole-document gate, on every
surface, since that check runs once centrally rather than per-surface. A
`cof fetch -o file.yml` copy (a *different*, filesystem-backed "shared
library", see `circuitry.cli.shared_library`) stays an ordinary run by path
as documented above — its destination is the caller's own `-o` choice, not
a source's cache directory, unless deliberately pointed there. Replaying a
`cof run-library` run via `cof run --last` or the TUI Runs view's replay
now also carries that run's own `remote_library_source` classification
through the stash, closing a second hole (also #337) where a replay skipped
the gate the original run applied.

### 10. Prompt files (`{file: <path>}`)

A declared prompt, a `prompt`/`yield` effect's `template`, or a message's
`content` can name a file instead of carrying its text inline (#396). The
file is read when the document is loaded and compiled — before any effect
runs. Capability consent (§9) runs before that compile step, but its own
digest (`document_content_digest`) reads every prompt file the raw,
uncompiled document names, independently of the later compile — so a
document that can name an arbitrary path could otherwise read anything the
host process can: credentials, another project's source, `/etc/passwd`.

**Mitigation.** The path is a literal relative path (no `{{ }}` — a path
built at render time can never reach this; it is read at compile time, from
the document's own text) resolved against the directory of the document
that names it, then — after symlinks are resolved, so a symlink inside the
project pointing outside it is still caught — checked against *that
document's project*: the directory of the nearest
`circuitry.config.json`/`config.json` at or above it, or its own directory
when there is none. A library document (`use: ref:`, `cof run-library`)
resolves the same way, against *its own* location rather than the parent
document's — with one exception: a `github` source is confined to the
cached tree at the pinned commit instead, never anywhere else on disk,
since that cache has no project config of its own to discover. A
folder/curation source already on disk (or a plain `path:`/`orchestration:`
field) gets the ordinary rule above, applied to the child's own directory —
not necessarily the whole source's tree: a source with no
`circuitry.config.json`/`config.json` anywhere above the specific document
confines to that document's own directory, the same as any other file
without one. An absolute path, or one that resolves outside the confinement
root, is a `cof check`/compile error naming the field, same as a missing,
unreadable, non-UTF-8, or over-1-MiB file. A document generated at run time
(a reflector/decompose plan, a `use: inline` child) cannot use `file:` at
all — it has no file of its own to resolve a relative path against, so this
is a compile error there too, not a narrower check. Implementation:
[`src/circuitry/core/prompt_files.py`](../src/circuitry/core/prompt_files.py).

**Residual risk.** Confinement is to a *project*, not to the one file a
`use: ref:`/`path:` child itself is — a prompt file anywhere else under the
same project (or, for a `github` source, the same cached commit) is
reachable, by design (so `../shared/voice.md` works across sibling
documents). A host that
trusts a document at all is already trusting everything under its project
by this same reasoning §1 and §6 already apply to `use: inline`/`path:`
children and project config discovery. Content a prompt file carries (and a
declared prompt's own text) is itself prompt text once rendered — it is
never treated as something else — and a `{{> name}}` splicing it in never
re-renders the result, so a file's own content cannot carry a template tag
that executes again (see [Mustache Template Interpolation](./orchestration-reference.md#mustache-template-interpolation)).
The one place resolved prompt text *does* become something else is a child
document's own source: `{{> gen}}` inside a `use: inline` string becomes
part of what gets parsed and compiled as that child's YAML, exactly as
`{{{prime.gen.value}}}` already does there — not new to this feature, and
gated the same way (`use: inline`'s own content is this document's own,
never independently trusted).

### 11. Generated documents

A document produced while a run executes — a reflector's plan, a `use`
whose `inline:` is rendered from state, or a decomposition plan — can carry
a tool effect that looks exactly like a document the author wrote by hand:
the same `params.allowed_commands` on a `shell` effect, the same
`provider:` tokens. Section 2's shell allowlist widens "only from a
literal, unrendered list" — but in a generated document, every list is
rendered content that merely *looks* literal once parsed. Tools that run
commands without going through the `shell` plugin's allowlist at all are
open to the same problem: `agent`, `service`, `python_eval`, `git`,
`docker`, `kubectl`, `gh`, `yt_dlp`, `ripgrep`, `pytest`, `awk`, `sed`,
`pandoc`, `mediainfo`, `imagemagick`, `exiftool`, `7z`, `ping`,
`traceroute`, `linter`, `ocr`, `gpg`, `diff_patch`, `pdf_render` and
`ffmpeg` each shell out to their own fixed binary with effect-supplied
arguments, with no allowlist of their own to widen or narrow.

**Telling a generated document from an author's own.** An author-written
`inline:` whose *structure* is literal — the same keys, the same number of
effects, the same list lengths as its own unrendered template — with only
scalar values templated in, counts as the author's own content and keeps
working exactly as before. The test: parse the `use` effect's own `inline:`
text *before* Mustache rendering as YAML; if that parse succeeds and its
shape (dict keys, list lengths, recursively) matches the rendered, parsed
document exactly, every difference is a templated *value*, not generated
structure — except at a security-sensitive key (`type`, `provider`,
`allow_tools`, `allowed_commands`, `shell`, `inline`, `path`, `ref`,
`orchestration`), where even a value that merely *renders to* what the
author could have written counts as generated: a Mustache tag that renders
to `shell` today can render to anything else tomorrow, and a nested
`inline:`/`path:`/`ref:`/`orchestration:` that renders to a different
document than the author wrote must never pass as the author's own just
because the surrounding structure still matches. A parse failure (the
common case — the whole `inline:` is one `{{{...}}}` tag) or a shape
mismatch anywhere means some or all of the document only exists because it
was rendered: generated. A reflector's plan has no author template of its
own to compare against at all — it is always generated. **Generated-ness
is inherited**: every effect textually inside an already-generated
document is itself generated too, whatever its own shape looks like — a
document the model wrote has no "author's own template" anywhere inside
it, including a nested `use: inline:` whose own unrendered text happens to
match its rendered shape by coincidence, and including a `use:
{path:}`/`ref:`/`orchestration:` child that loads from disk: it no longer
gets a fresh ceiling of its own just because it names a separate file,
once the document that carries it is itself generated. A `path:`/`ref:`
child of a document that is *not* itself generated is unaffected — it
stays its own, independently trusted document, exactly as before. Both
engines apply exactly this test, so a document that is generated on one is
generated on the other.

**Where the check runs.** A generated document's own text is checked
twice. First, statically, when the generated document is compiled, before
any of its effects run: a document whose own text already breaks a rule —
a literal `allowed_commands` entry outside the allowlist, a literal
`shell: true` on a `service` effect, a tool provider outside the ceiling —
is refused up front, under the generating `use`/`reflector` effect's own
`on_error`, naming the generated effect, the provider or command, and the
fix. That static check can only ever be as strict as the parsed document
lets it be: it cannot see what `params_json` will merge in once rendered,
and it cannot resolve a `{from: ...}` reference to the value it will
actually dispatch with. Second, and authoritatively, right before every
tool effect inside a generated document actually dispatches, on every
retry: the final provider name (normalized exactly as plugin lookup
normalizes it) and the final params, after Mustache rendering and the
`params_json` merge, are checked against the same rules. A violation only
this second check can see — a value `params_json` set, or a `{from: ...}`
reference that only resolved to an unauthorized value once rendered — is
refused at that point: the effect fails that one attempt and is never
retried, recorded on the effect the same way any other failure is, under
the generating effect's own `on_error`. The two checks call the same rule
functions, so the first is never stricter than the second — they can only
ever agree, or the first simply can't decide yet what the second
authoritatively will.

**What is checked.** A generated document's `shell` effects may run only
what the host config and the document that generated it already allow: the
union of the default/host-pinned allowlist and every literal `shell`
effect's own `allowed_commands` anywhere in the enclosing, non-generated
document's own text. A generated effect's own `params.allowed_commands`
narrows that set further when it names fewer commands, but never widens
past it — entries outside it are refused by name, naming the fix (add the
command to a literal shell effect's `allowed_commands` in the document, or
to `runtime.plugins.shell.allowed_commands`). A generated `service`
effect's `params.shell` is decided the same way the `service` plugin
itself decides it (never a separate, narrower boolean test that could
disagree with what actually runs) and a true value is refused outright; a
`service` effect that starts a process without `shell: true` has its
executable (the first element of its argv list) checked the same way the
`shell` plugin checks its own `command`: a bare name, present in the same
allowlist a `shell` effect would be checked against — a path never counts
as a member of that allowlist, however it compares. `params_json` can
never set `allowed_commands` or `shell`; the two checks above are what
decide a generated `service` effect's `shell`, never a value `params_json`
carried in. A generated document may use only the tool providers the
document that generated it uses itself, statically (every `type: tool`
effect anywhere in that document's own text, including inside a literal
`use: inline:` child), plus whatever the generating `use`/`reflector`
effect's own literal `allow_tools:` field adds — never a rendered value,
`params_json`, or a state reference; `cof check` rejects any of those, and
validates each name against the known tool-plugin providers. Because
generated-ness is inherited, a nested `use`/`reflector` effect's own
`allow_tools:` is itself part of the enclosing generated document's text
whenever that document is itself generated, and can never grant anything
beyond what the enclosing document was itself allowed — naming something
beyond it is refused, not silently ignored. A provider that isn't a real
plugin at all is not refused here — it still fails the ordinary way (an
"Unknown plugin" error) once the generated document actually tries to
dispatch it, since adding an unknown name to `allow_tools` would itself be
a `cof check` error.

**Decomposition plans.** A decomposition plan (`runtime.complexity.decomposition`)
is generated too and runs with limits of its own: when the context it runs
in already has limits (the plan is itself nested inside an already-generated
document), it inherits them unchanged; otherwise its limits are the
document it serves uses itself, statically, plus a fixed set of
side-effect-free data tools every plan is always allowed, since it has no
`use`/`reflector` effect of its own to carry an `allow_tools:` and its own
merge step commonly needs a tool to combine chunk results the document it
serves doesn't otherwise use: `json`, `regex`, `math`, `cel`, `clock`,
`base64`, `hex`, `uuid`, `validate_yaml`. `csv` and `hash` are deliberately
not in that set: both support reading an arbitrary local file by path
(`from_path`), which is not a side-effect-free data transform.

**Plainly: a tool provider that runs code by design passes that same power
to a generated plan, once the document uses it.** The ceiling above is a
tool-provider allowlist, not a sandbox — a document that itself uses a
shell interpreter the document allows (`node`, `python`, `env`, `find`,
`xargs`, and the like), an `awk` program, one of the `agent` CLIs, or a
`service` with a given executable already allowed, hands a generated plan
or a rendered `inline:` the same power to run arbitrary code through that
same provider, inside whatever that interpreter/program/CLI itself can do —
the tool-provider ceiling only ever decided *which* tool a generated
document can reach, never what that tool itself is capable of once
reached. If a generated plan must be held to less than the document's own
literal effects can already do, put the generated path in a separate
document with its own, narrower tool providers.

Implementation: [`src/circuitry/generated_doc.py`](../src/circuitry/generated_doc.py),
and the equivalent Rust module,
[`electricity/crates/electricity-vm/src/generated_doc.rs`](../electricity/crates/electricity-vm/src/generated_doc.rs).
See [the orchestration reference](./orchestration-reference.md) for the
author-facing `use`/`reflector`/`allow_tools:` fields.

**Residual risk.** The structural test is deliberately coarse (whole-document
shape, not per-field): an author-written `inline:` that happens to add or
drop a key between its unrendered template and the rendered result — a
`{{#if}}`-style block helper producing a different-length list, say —
counts as generated even though a human wrote every line of it, and then
needs `allow_tools:` for anything beyond what the document already uses.
That is the conservative direction to err in.

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
