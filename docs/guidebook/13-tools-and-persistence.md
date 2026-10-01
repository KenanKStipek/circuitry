# Tools, adapters, and persistence

Three kinds of extension carry a run beyond the process, and all three are behind one small protocol each. **Tool plugins** reach outward from inside a document — a computation whose result is not a token stream. **Adapters** carry the conversation to a model provider. **Runtime plugins** are pure observers, and the durable ones — persistence backends — are the reason a run can be resumed, queried, and audited after the process is gone.

## Tool effects

```
Tool ::= { type: 'tool', name: NAME, provider: PLUGIN_NAME,
           prompt?: TEMPLATE, model?: STRING,
           params?: MAP,                                 — string values Mustache-rendered; wins over prompt/model
           params_json?: TEMPLATE,                       — rendered, parsed as JSON, deep-merged over params
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

`shell`'s own allowlist is a document setting — a per-effect `allowed_commands` replaces the tiny default, and the author who writes it down is trusted to have meant it. What a document *cannot* do is widen that past a host pin: `runtime.plugins.shell.allowed_commands` in config, when set, intersects with whatever the effect lists (or the default), so an effect can only narrow it further. The pin is config-only — a document's own `runtime:` block cannot set it, for the same reason it cannot set any other `runtime.plugins.*` key (see [Host settings versus orchestration documents](../threat-model.md#6-host-settings-versus-orchestration-documents)) — and it is honoured only from an effect's literal, unrendered `params.allowed_commands`: a value that arrived via `params_json`, or a literal list with a still-unrendered Mustache tag in it, is a hard error rather than silently accepted, so a prior step's (or a model's) output can never be the source of what commands a later step is allowed to run.

```json
{
  "runtime": {
    "plugins": {
      "shell": {"allowed_commands": ["ls", "cat", "ffmpeg"]}
    }
  }
}
```

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

`GenerateResult` is `text`, `raw` (the provider's response, verbatim), and optional `tokens_sent` / `tokens_received`. That is the whole contract, and twenty-nine adapters implement it: `ollama`, `openai`, `anthropic`, `gemini`, `mistral`, `cohere`, `groq`, `deepseek`, `xai`, `perplexity`, `together`, `fireworks`, `replicate`, `ai21`, `qwen-dashscope`, `nvidia-nim`, `huggingface-inference`, `watsonx`, `databricks`, `azure-openai`, `cloudflare-workers-ai`; the self-hosted servers `vllm`, `llamacpp`, `lmstudio`, `tgi`; the aggregators `openrouter`, `litellm`; the `cyberdiner` broker; and `host_claude`.

An orchestration written for one runs on another with no edits, because the document never named the adapter — [Configuration](04-configuration.md) did. Adapters declare an optional `list_models()` (what `cof list --models <adapter>` calls) and are registered in `ADAPTER_REGISTRY`; [Adapter Conformance](../adapter-conformance.md) is the checklist for writing one, and the `adapter=` parameter of the SDK is how to use one without registering it.

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

`context` carries `run_id`, `orchestration_path`, `dry_run`, `validate_only`, and `runtime_config`. The per-effect pair is balanced — an effect that fires one fires the other, failure included — and namespaced through `use` children. Plugins register in config (`"plugins": ["my_package.plugins:make_plugin"]`, a `module:attr` that may be a zero-argument factory); a plugin that raises is recorded under `runtime.plugins.events` and never fails the run. The contract version is reported at `runtime.plugins.contract_version`.

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

## Anti-patterns

**A tool for text work.** Summarising is a prompt. A tool that "analyses" is a prompt wearing a `provider:`.

**Hard-coding tool parameters the model should choose.** Let a `json` prompt produce them; the tool reads `{{prime.<prompt>.value.<field>}}`.

**Widening the `shell` allowlist by habit.** Each `allowed_commands` entry is a sentence in your threat model.

**A plugin that steers.** Runtime plugins observe. A plugin that mutates `prime` mid-run has broken the one property everything else relies on.

**Credentials in a document.** Servers, tokens, DSNs — config, always. The run record redacts config; nothing redacts a template.

## See also

- [Plugin Extension Guide](../plugins.md) · [Adapter Conformance](../adapter-conformance.md) · [Postgres Persistence](../postgres-persistence.md).
- [`docs/plugins/ffmpeg.md`](../plugins/ffmpeg.md) · [`docs/plugins/comfyui.md`](../plugins/comfyui.md) · [`docs/plugins/surrealdb.md`](../plugins/surrealdb.md) · [Binary tool plugins](../plugins/binary-tools.md) · [Runtime Plugin Catalog](../runtime-plugins.md).
- [Threat Model](../threat-model.md).
