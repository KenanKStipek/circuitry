# Tools, adapters, and persistence

Three kinds of extension carry a run beyond the process, and all three are behind one small protocol each. **Tool plugins** reach outward from inside a document — a computation whose result is not a token stream. **Adapters** carry the conversation to a model provider. **Runtime plugins** are pure observers, and the durable ones — persistence backends — are the reason a run can be resumed, queried, and audited after the process is gone.

## Tool effects

```
Tool ::= { type: 'tool', name: NAME, provider: PLUGIN_NAME,
           prompt?: TEMPLATE, model?: STRING,
           params?: MAP,                                 — string values Mustache-rendered; a {from: PATH} leaf passes a native value; wins over prompt/model
           params_json?: TEMPLATE,                       — rendered, parsed as JSON, deep-merged over params
           group?: STRING,                               — names a runtime.concurrency_groups key; leaf only
           timeout_ms?: INT, on_error?: 'fail'|'skip'|'continue', description?: STRING }
```

A tool effect is a plugin call. Reading a file, fetching a URL, querying SQL, sending a Slack message, running ffmpeg — each fits the same monadic shape as a prompt, writes to the same deterministic path, and participates in control flow indistinguishably from one. An `if` can branch on a tool's result; a loop can iterate it; a prompt can read it.

```yaml
- type: tool
  name: search
  provider: ripgrep
  params:
    args: [--line-number, --fixed-strings, "{{input.error}}", "."]
    cwd: "{{input.repo}}"
    allow_nonzero: true
```

`provider` names the plugin. `params` is the plugin's own vocabulary — every string value is a Mustache template — and for the two media plugins that predate `params`, `prompt` and `model` are top-level shorthand (`params` wins when both are given). The result lands at `prime.<name>.value` in whatever shape the plugin documents: a path, a parsed document, a list of records. A plugin that runs a program returns its standard output, and records `stdout`, `stderr` and `exit_code` in `meta`. A non-zero exit fails the step unless `allow_nonzero: true` is set — `rg` exits 1 when nothing matches, and `pytest` exits 1 when a test fails, and an agent wants to read both answers rather than stop on them.

One failure contract covers every plugin: a tool effect fails — `meta.error` set, `on_error` applies — exactly when the plugin raises or returns a result with `ok: false`, with no other way to signal it. `meta.exit_code` means a process exit code and nothing else; it is `null` for every plugin that doesn't wrap a binary. `http`/`web_fetch`/`webhook`/`linear` fail on a 4xx/5xx response by default (the status lands on `meta.status_code`, not `exit_code`) — pass `fail_on_error: false` on the effect to get the old always-succeeds behaviour back. `wikipedia`/`dns`/`port_check`/`validate_yaml` treat a negative result (page not found, NXDOMAIN, closed port, invalid document) as information, not a tool failure: they stay `ok: true` and report it on `value`/`raw` instead. Every tool effect also gets `meta.raw` — the plugin's own raw response, redacted and size-capped — reachable on an `ok: false` failure (the plugin still returned a result); a plugin that raises leaves `meta.raw` unset, since there's no result to read it from. A prompt effect never sets it.

A value rendered into `params` is always a string, so a list from an earlier step arrives as its string form. When a tool needs a real array or object built at run time, write `params_json`: a template that renders to a JSON object, which is parsed and deep-merged over `params` (its keys win). A list or object from state renders into it as JSON:

```yaml
- type: tool
  name: open_issue
  provider: mcp
  params:
    server: github
    tool: create_issue
    arguments: {owner: "{{input.owner}}", repo: "{{input.name}}", title: "Follow-up: {{prime.follow_up.value}}"}
  params_json: '{"arguments": {"labels": {{{prime.labels.value}}}}}'
```

Here `arguments.labels` is the label list an `array` prompt chose, the list itself, next to the three strings from `params`. A template that does not render to a JSON object fails the tool; it never runs with other parameters than the ones you wrote.

`params_json` is one way to pass a native value through untouched; a `{from: <path>}` leaf anywhere inside `params` itself is the other, and it reads better when the value is already sitting at a known state path rather than built from a template:

```yaml
- type: tool
  name: get_equity_quotes
  provider: mcp
  params:
    server: robinhood
    tool: get_equity_quotes
    arguments:
      symbols: {from: prime.symbol_list.value}
```

`arguments.symbols` arrives as the native array `prime.symbol_list.value` holds, not its Mustache-rendered string form. It resolves against the same scope a `use` effect's by-reference `inputs` do (state namespaces, an enclosing loop's bindings) and fails `cof check` the same way on an unrooted path; add `default: <value>` alongside `from:` to fall back instead of failing when the path doesn't resolve. See [Params by reference](../orchestration-reference.md#params-by-reference).

The division of labour is strict. Tools are for what a model *cannot* do — side effects and observations. Text work — summarising, extracting, classifying, writing, coding — is a prompt, and the pattern that joins them is **prompt-then-tool**: a `json` prompt produces the parameters, a tool executes them.

```yaml
- type: prompt
  name: plan_search
  prompt_type: json
  schema:
    type: object
    properties:
      pattern: {type: string}
      glob: {type: string}
    required: [pattern, glob]
  template: "Choose a search for the code that raises the error in this issue. Return ONLY JSON with pattern (a literal string from the error) and glob (such as *.py). Issue: {{input.issue}}"

- type: tool
  name: search
  provider: ripgrep
  params:
    args: [--line-number, --fixed-strings, --glob, "{{prime.plan_search.value.glob}}", "{{prime.plan_search.value.pattern}}", "."]
    cwd: "{{input.repo}}"
    allow_nonzero: true
```

### The built-in providers

Seventy-odd ship in-tree. By purpose:

| Group | Providers |
| --- | --- |
| Time / utility | `clock`, `math`, `regex`, `json`, `uuid`, `hash`, `base64`, `hex`, `validate_yaml` |
| Filesystem / data | `fs`, `csv`, `tar`, `zip`, `gzip`, `7z` |
| Internet | `http`, `web_search`, `web_fetch`, `webhook`, `wikipedia`, `rss`, `weather` |
| Browser | `playwright`, `screenshot` |
| Communication | `email_smtp`, `slack`, `discord` |
| Productivity SaaS | `github`, `jira`, `linear`, `notion`, `gcalendar`, `gdrive` |
| Storage | `s3`, `surrealdb` |
| Audio / image / video | `ffmpeg`, `comfyui`, `imagemagick`, `exiftool`, `ocr`, `yt_dlp`, `mediainfo` |
| PDF / documents | `pdf_extract`, `pdf_render`, `pandoc` |
| Embeddings / RAG | `embed`, `rerank`, `vector_search` |
| Code / dev | `git`, `gh`, `ripgrep`, `pytest`, `linter`, `docker`, `kubectl` |
| Coding agents | `agent` — a delegated pi or Claude Code session |
| Sandboxed execution | `python_eval`, `shell` |
| Text processing | `awk`, `sed`, `diff_patch` |
| Network | `dns`, `whois`, `ping`, `traceroute`, `port_check` |
| System | `system_info`, `process_list`, `env_vars` |
| Crypto / markup | `gpg`, `xml`, `html_extract` |
| MCP | `mcp` — any tool on any MCP server |

`cof list --extensions` prints the exact compiled-in set. Plugins that need optional PyPI packages or external binaries lazy-import; `cof doctor` reports each one's readiness, and `pip install circuitry-cof[<plugin>]` (`[playwright]`, `[github]`, `[embed]`, …) pulls what a plugin needs. `docs/plugins/` documents the media plugins' parameters in full.

The plugins that wrap a command-line program (`git`, `gh`, `ripgrep`, `pytest`, `ffmpeg`, `imagemagick`, and the other subprocess tools) search `PATH` for it by default. Config can name the exact executable and add to its environment — both machine-specific, so both stay out of the document. Here the agent's `pytest` is the one in the project's own virtual environment, so the tests run against the project's dependencies:

```json
{
  "runtime": {
    "plugins": {
      "pytest": {
        "binary": "~/src/durations/.venv/bin/pytest",
        "env": {"PYTHONHASHSEED": "0"}
      }
    }
  }
}
```

`binary` is an absolute path (`~` is expanded) and replaces the `PATH` search; `env` is merged over the inherited environment. A configured binary that does not exist fails the tool and preflight with a message that names the setting. Each tool node records the executable that actually ran in `meta.binary`. [Binary tool plugins](../plugins/binary-tools.md) lists the plugins that read these settings.

Two are gated on purpose. `shell` runs a single binary from an allowlist that is deliberately tiny and read-only by default, with a per-effect `allowed_commands` override the author must write down; `python_eval` is likewise sandboxed. Shell metacharacters are rejected in `ffmpeg` paths. And the `enabled_tools` allowlist in config is the deployment-level gate: a document that references a tool outside it — itself or through a `use` child — fails validation before anything runs, and a tool that only appears at run time, in a rendered `inline:` child or a generated plan, is refused before it is built.

`shell`'s own allowlist is a document setting — a per-effect `allowed_commands` replaces the tiny default, and the author who writes it down is trusted to have meant it. What a document *cannot* do is widen that past a host pin: `runtime.plugins.shell.allowed_commands` in config, when set, intersects with whatever the effect lists (or the default), so an effect can only narrow it further. A *limited* document — a library, a `use` child, a generated plan, or one reached through REST/MCP — cannot set the pin itself, for the same reason it cannot set any other `runtime.plugins.*` key (see [Host settings versus orchestration documents](../threat-model.md#6-host-settings-versus-orchestration-documents)). A *trusted* document (named by path, `cof run ./f.yml`, or any document once the host sets `trust_orchestration_runtime`) keeps its whole `runtime:` block, including `runtime.plugins`, and `runtime.plugins`/`runtime.adapters` merge key by key — a document setting one plugin's (or adapter's) block never drops another's. The host's shell pin is the one exception even there: a trusted document's own `runtime.plugins.shell.allowed_commands` intersects with the pin rather than replacing it, the same ceiling a limited document hits, just one level up the trust ladder. The pin is honoured only from an effect's literal, unrendered `params.allowed_commands`: a value that arrived via `params_json`, or a literal list with a still-unrendered Mustache tag in it, is a hard error rather than silently accepted, so a prior step's (or a model's) output can never be the source of what commands a later step is allowed to run.

```json
{
  "runtime": {
    "plugins": {
      "shell": {"allowed_commands": ["ls", "cat", "ffmpeg"]}
    }
  }
}
```

### Delegating to a coding agent

A `prompt` is one model call. Some work needs an agent: read the code, edit it, run the tests, read the failure, try again — for minutes, sometimes hours, in a working directory. The `agent` provider runs one such session of pi or Claude Code as a tool effect, through the CLI's own login:

```yaml
- type: tool
  name: fix_test
  provider: agent
  timeout_ms: 3600000
  params:
    engine: claude_code
    cwd: "{{input.repo}}"
    prompt: |
      Make the failing test pass without changing the test itself.

      {{prime.search.value}}
    tools: [Read, Grep, Glob, Edit, Write, "Bash(pytest:*)"]
    exclude_tools: [WebFetch, WebSearch]
    result_file: .agent/result.json
    result_schema:
      type: object
      properties:
        summary: {type: string}
        tests_pass: {type: boolean}
      required: [summary, tests_pass]
```

The prompt goes to the CLI in a file (pi) or on stdin (Claude Code), never on the command line, so it can be as long as the task needs. `result_file` is the session's contract: the agent is told to write that JSON file, the plugin validates it against `result_schema`, and `prime.fix_test.value` is the parsed object — here a real `tests_pass` boolean a later `if` can branch on. A missing or invalid file gets exactly one repair turn in the same session, quoting the errors; still invalid, the effect fails with them. Without `result_file`, `value` is the agent's final reply. `meta.raw` records the session id (a later effect's `session` param resumes it), the turns, tool calls, tokens and cost, and the path of a compact transcript — the transcript itself stays out of state. The session runs in its own process group: `timeout_ms` (here an hour, for the whole session) or a cancelled run stops the CLI and everything the agent started.

**The agent is not sandboxed.** It runs with your permissions: it can edit or delete any file you can, run any program, and reach the network. The engine's own tool lists are how you narrow it, and the two engines differ in detail. For pi, `tools` is an allowlist and `exclude_tools` a denylist (`--tools`/`--exclude-tools`). For Claude Code, `tools` (`--tools`, its entries also pre-approved with `--allowedTools`) is an allowlist: the session has only the built-in tools it names, and under the default `dontAsk` mode every other call is denied. `exclude_tools` (`--disallowedTools`) is a hard deny in every permission mode. The example lists what the session needs (file tools, and `Bash` limited to `pytest`) and leaves `permission_mode` out; `Write` is what lets it write `result_file` (`Edit` cannot create it: the file is deleted before the session starts). A Claude Code session also ignores its repository's own settings (hooks, the API key helper, project MCP servers) unless `trust_project_settings: true` is set, and gets the repository's root `CLAUDE.md` appended to its prompt. For the same reason `agent` carries the `shell`, `fs-write` and `network` capabilities, so a document that is not your own needs the same consent to run one as to run `shell`. Every param, the config block that picks the default engine and each CLI's binary, and the full result contract are in [`docs/plugins/agent.md`](../plugins/agent.md).

### MCP servers as tool providers

The `mcp` provider is the client-side complement to the MCP *server* in [Surfaces](12-surfaces.md): it calls tools on external Model Context Protocol servers. Servers are declared in config — named, like adapters, and referenced from YAML by name only, so credentials never enter a document and are redacted before any state is written:

```json
{
  "runtime": {
    "plugins": {
      "mcp": {
        "servers": {
          "github": {"command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": {"GITHUB_PERSONAL_ACCESS_TOKEN": "…"}},
          "internal": {"url": "https://mcp.example.com/mcp", "headers": {"Authorization": "Bearer …"}}
        }
      }
    }
  }
}
```

```yaml
- type: tool
  name: open_issue
  provider: mcp
  params:
    server: github
    tool: create_issue
    arguments:
      owner: "{{input.owner}}"
      repo: "{{input.name}}"
      title: "Follow-up: {{prime.follow_up.value}}"
```

Transport is inferred from the shape (`command` → stdio, `url` → HTTP). The value is the tool's structured content when the server sends it; otherwise, under the default `parse: auto`, its text is parsed as JSON when it is a JSON object or array, and kept as text when not. `operation: list_tools` returns a server's catalogue — a way for an orchestration, or a reflector, to discover capabilities at run time.

## Adapters

An adapter carries one call out of the process:

```python
class Adapter(Protocol):
    name: str
    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> GenerateResult: ...
    def check(self) -> CheckResult: ...          # preflight
```

`GenerateResult` is `text`, `raw` (the provider's response, verbatim), and optional `tokens_sent` / `tokens_received`. That is the whole contract, and thirty-two adapters implement it: `ollama`, `openai`, `anthropic`, `gemini`, `mistral`, `cohere`, `groq`, `deepseek`, `xai`, `perplexity`, `together`, `fireworks`, `replicate`, `ai21`, `qwen-dashscope`, `nvidia-nim`, `huggingface-inference`, `watsonx`, `databricks`, `azure-openai`, `cloudflare-workers-ai`; the self-hosted servers `vllm`, `llamacpp`, `lmstudio`, `tgi`; the aggregators `openrouter`, `litellm`; the `cyberdiner` broker; the coding-agent CLIs `pi` and `claude_code`; `host_claude`; and `scripted`.

An orchestration written for one runs on another with no edits, because the document never named the adapter — [Configuration](04-configuration.md) did. Adapters declare an optional `list_models()` (what `cof list --models <adapter>` calls) and are registered in `ADAPTER_REGISTRY`; [Adapter Conformance](../adapter-conformance.md) is the checklist for writing one, and the `adapter=` parameter of the SDK is how to use one without registering it.

`scripted` is the odd one out: no network, ever, and no `model:` it actually reads. It answers every model call — a `prompt` effect, a `tool`/`use` effect's `expect: {mode: model}`, an `if`/`while` effect's own `mode: model` condition — from `runtime.adapters.scripted.replies_file` (resolved against the working directory, like any other relative config path; defaults to `scripted-replies.yaml` when unset) — a YAML or JSON mapping from the calling effect's own state path (`prime.review`, `prime.shots.iter_2.describe`) to an ordered list of replies, each either `{text, tokens_sent, tokens_received}` or `{error: {kind, status, message}}` with `kind` one of the named failures the runtime's own retry classification already knows (`timeout`, `connection`, `rate_limited`, `server_error`, `invalid_request`, `unauthorized`, `not_found`, `http`). Replies for one path are consumed in order, covering a retry or an `expect:` re-ask against the same effect; an unmatched call fails naming the path. `electricity/docs/spec/scripted-replies.md` has the full format — electricity's own scripted adapter reads the identical file, so one fixture configures the conformance suite's cases for both engines.

## Runtime plugins

A runtime plugin observes a run and changes nothing about its semantics:

```python
class MyPlugin:
    name = "my-plugin"
    def on_run_start(self, *, state, context): ...
    def on_run_success(self, *, state, context): ...
    def on_run_failure(self, *, state, context, error): ...
    def on_effect_start(self, *, state, context, effect_path, effect_node): ...      # optional
    def on_effect_complete(self, *, state, context, effect_path, effect_result): ... # optional
```

`context` carries `run_id`, `orchestration_path`, `dry_run`, `validate_only`, `runtime_config`, and `environment` (`CircuitryConfig.environment`, feeding the persistence plugins' `store_raw` default). The per-effect pair is balanced — an effect that fires one fires the other, failure included — and namespaced through `use` children. Plugins register in config (`"plugins": ["my_package.plugins:make_plugin"]`, a `module:attr` that may be a zero-argument factory); a plugin that raises is recorded under `runtime.plugins.events` and never fails the run. The contract version is reported at `runtime.plugins.contract_version`.

Thirty-one ship in-tree, behind the one protocol:

| Kind | Plugins |
| --- | --- |
| SQL persistence (one schema, seven dialects) | `sqlite`, `postgres`, `mysql`, `mssql`, `cockroachdb`, `duckdb`, `clickhouse` |
| Document / KV / object stores | `mongodb`, `couchdb`, `firestore`, `dynamodb`, `elasticsearch`, `opensearch`, `surrealdb`, `redis`, `memcached`, `s3`, `gcs`, `azure_blob`, `r2` |
| Append log | `jsonl_file` |
| Pub/sub | `kafka`, `nats`, `rabbitmq` |
| Observability exporters | `opentelemetry`, `sentry`, `datadog`, `honeycomb`, `prometheus`, `loki`, `cloudwatch` |

The exporters turn the per-effect pair into spans, events, or metrics; the stores turn the run record into rows. None of them touch control flow, which is the point of the protocol: you can add Datadog to a deployment without re-validating a single orchestration.

## Persistence: the durable side of the shadow state

`runtime.persistence` selects a backend that stores each run's final state and rehydrates it for the next:

```json
{
  "runtime": {
    "persistence": {
      "enabled": true,
      "backend": "sqlite",
      "db_path": ".circuitry/runs.db"
    }
  }
}
```

| `backend` | Required | Notes |
| --- | --- | --- |
| `jsonl-file` | `path` | one record per run, appended; stdlib; stays greppable |
| `sqlite` | `db_path` (or `path`) | stdlib; `table` defaults to `circuitry_runs` |
| `postgres` | `dsn` | `pip install psycopg[binary]`; `sslmode` should be `require` or stronger |
| `mongodb` | `uri` | `pip install circuitry-cof[mongodb]`; `database` / `collection` |

With a backend configured, a run loads the last persisted state for the orchestration before it starts (`runtime.persistence.loaded_from_persistence: true`) and saves its own at the end. A backend that cannot be reached fails the run with an actionable error and records `runtime.persistence.status`. Credential-bearing values — a DSN's password, a Mongo URI's `user:pass@` — are redacted everywhere state is serialized; only the driver sees them. A [profile](04-configuration.md) can select a backend per run, and `--out` remains what it always was: a one-shot dump of the final state to a file, orthogonal to and combinable with any backend.

[Postgres Persistence](../postgres-persistence.md) has the production notes.

### Resuming a run (`cof run --resume`)

Persistence seeds a run's *starting* state; it does not skip anything —
every effect reruns regardless of what a prior attempt already finished.
`cof run --resume last` (or `--resume <run-id>`, or `--state <file>
--resume <anything>`) is the opt-in that does skip: it continues an
interrupted or failed run of the same document, reusing every effect whose
node already finished without error (`meta.completed_at` set, no
`meta.error`) and rerunning only the first unfinished/failed one and
everything after it. A named loop in chain flow resumes at its first
unfinished pass, keeping the finished `iter_<N>` passes — see [Resuming a
Run](../orchestration-reference.md#resuming-a-run-cof-run---resume) in the
reference for the full skip/rerun rule, the three state sources (`--state`,
`--resume last`, `--resume <run-id>` via this section's persistence
backend), and the content-hash/inputs safety checks.

This is the owner's long film/upscale pipelines' main use: a 164-minute
video-upscale loop that crashes — or is killed outright (Ctrl-C/SIGTERM/
SIGHUP, exiting 130/143/129) — at frame 250 doesn't lose the 249 already-rendered
frames — `cof run upscale.yml --state run.json --resume x` picks up at
frame 250, and the adapter/tool calls for frames 0–249 never happen again.

## Anti-patterns

**A tool for text work.** Summarising is a prompt. A tool that "analyses" is a prompt wearing a `provider:`.

**Hard-coding tool parameters the model should choose.** Let a `json` prompt produce them; the tool reads `{{prime.<prompt>.value.<field>}}`.

**Widening the `shell` allowlist by habit.** Each `allowed_commands` entry is a sentence in your threat model.

**A plugin that steers.** Runtime plugins observe. A plugin that mutates `prime` mid-run has broken the one property everything else relies on.

**Credentials in a document.** Servers, tokens, DSNs — config, always. The run record redacts config; nothing redacts a template.

## See also

- [Plugin Extension Guide](../plugins.md) · [Adapter Conformance](../adapter-conformance.md) · [Postgres Persistence](../postgres-persistence.md).
- [`docs/plugins/ffmpeg.md`](../plugins/ffmpeg.md) · [`docs/plugins/comfyui.md`](../plugins/comfyui.md) · [`docs/plugins/surrealdb.md`](../plugins/surrealdb.md) · [`docs/plugins/agent.md`](../plugins/agent.md) · [Binary tool plugins](../plugins/binary-tools.md) · [Runtime Plugin Catalog](../runtime-plugins.md).
- [Threat Model](../threat-model.md).
