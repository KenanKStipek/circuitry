# Configuration

Capability lives in config, deliberately outside orchestration documents. A document describes *what to do* — the topology, the prompts, the conditions. Config describes *what can do it* — which adapter, which model, which timeout, which persistence backend. Keep the two apart and an orchestration written against a local 8B model this morning runs against a hosted frontier model this afternoon with no edit, and the complexity router in Part III can choose per prompt.

The rule of thumb: an orchestration in the curation library never sets `adapter:` or `model:`. Both fields are legal at the top of a document and on every prompt, and there are moments to use them — a step that must run on a specific model whatever the run says — but each one is a small hard-wiring, and the router steps around it.

## Where config comes from

Resolution is layered, each layer deep-merged over the last:

1. **Sane defaults** — `ollama` at `http://localhost:11434`, `llama3.1:8b`.
2. **Global config** — `~/.config/circuitry/config.json`.
3. **Project config** — `circuitry.config.json` or `config.json` in the working directory, once you have trusted it with `cof trust` (see [Trusting a project config](#trusting-a-project-config)).
4. **An explicit file** — `--config <path>` or `CIRCUITRY_CONFIG`, which *replaces* 2 and 3 rather than layering over them.
5. **Environment variables** — `CIRCUITRY_ADAPTER`, `CIRCUITRY_MODEL`, `CIRCUITRY_ADAPTER_URL`, `CIRCUITRY_COMFYUI_URL`, the allowlists `CIRCUITRY_ENABLED_ADAPTERS` / `CIRCUITRY_ENABLED_TOOLS` / `CIRCUITRY_ENABLED_PLUGINS`, and `CIRCUITRY_TRUST_ORCHESTRATION_RUNTIME` (below).

Two exceptions to "the last layer wins", both about a project config found in the working directory rather than named explicitly. First, trust: it applies only once you have run `cof trust` on it (or set `CIRCUITRY_TRUST_PROJECT_CONFIG=1`) — see [Trusting a project config](#trusting-a-project-config). Second, once trusted, it may *narrow* an allowlist the global config set, never widen it: its list is intersected with the global one, and `"enabled_tools": null` in a cloned repo does not re-open what your global config locked down; a warning names what was ignored. A file you name yourself — `--config` or `CIRCUITRY_CONFIG` — skips both checks and is trusted as written.

`cof run` and `cof check` print the config files and environment variables they resolved from on their `Config:` line; `cof doctor` lists them as `Config sources`. A discovered project file appears there even when it was skipped for lack of trust — as `(project, not trusted — skipped)` rather than `(project, trusted)` — so the line explains a missing setting as well as a present one.

`cof setup` walks you through creating the global file — it detects local backends and writes a working config, and an API key you enter for the optional `.env` it offers to create. Both `config.json` and `.env` are written mode `0600` in a directory created `0700`, tightening the mode of either file if it already existed looser; `cof doctor` warns if either is still group- or world-readable (e.g. from before this). `cof init` writes a project config and a `hello.yml` beside it, and trusts the config it wrote. `--yes` (plus `--adapter`/`--adapter-url`/`--model` to override any default) skips every prompt, for a Dockerfile `RUN` or a CI onboarding script. The generated `hello.yml` needs no model, no network and no API key — it runs a bundled zero-dependency `regex` tool effect, not a prompt, so it works immediately regardless of what adapter `cof init` just configured; reach for `learn/hello` (an actual prompt effect) once a model is set up. `cof doctor` tells you what the resolved config can actually reach.

Two example files ship in the package itself (and on GitHub) if you would rather write `config.json`/`.env` by hand than run the wizard: [`config.example.json`](https://github.com/kenankstipek/circuitry/blob/main/src/circuitry/bundled/examples/config.example.json) shows the documented keys below with realistic placeholder values, and [`.env.example`](https://github.com/kenankstipek/circuitry/blob/main/src/circuitry/bundled/examples/.env.example) lists every credential/endpoint environment variable a hosted adapter or tool plugin reads, one per line with a comment; a storage or observability runtime plugin's own env vars (Postgres, Datadog, Sentry, …) are documented per plugin in the [Runtime Plugin Catalog](../runtime-plugins.md) instead, since there are far more of them and the set grows with every new plugin. `cof setup` prints the installed path of both after it writes `config.json`/`.env`.

A local-first project config:

```json
{
  "default_adapter": "ollama",
  "default_model": "llama3.1:8b",
  "runtime": {
    "adapters": {
      "ollama": {
        "base_url": "http://localhost:11434",
        "timeout_seconds": 600
      },
      "openai": {
        "base_url": "https://api.openai.com/v1",
        "default_model": "gpt-4o-mini"
      },
      "anthropic": {
        "base_url": "https://api.anthropic.com",
        "default_model": "claude-sonnet-4-20250514",
        "max_tokens": 4096
      }
    }
  }
}
```

`default_adapter` and `default_model` are the run defaults. `runtime.adapters.<name>` carries each adapter's own settings — base URL, socket timeout, a per-adapter default model, token limits. API keys are *not* in this file: hosted adapters read them from the environment (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, and so on — a `.env` file works), and `cof doctor` names the missing one. Nothing credential-shaped is ever written into serialized state; the redaction layer scrubs it from `runtime.effective_settings` before it lands.

The `.env` `cof setup` writes is loaded automatically, before config resolution, by every host entry point: the `cof` CLI, the TUI, the MCP server (`circuitry-mcp`), and the REST trigger service. Only that one private file — never a `.env` in the working directory or a document's directory. A variable already set in the real environment always wins. If the file is not owned by you, or is writable by group or others, it is refused rather than loaded, with one warning naming the file and `chmod 600 <path>`; a file merely *readable* by group or others is still loaded, with the same warning. `cof doctor` reports whether the file was loaded and the variable *names* it supplied or skipped because the environment already had them — never a value. The SDK (`circuitry.api`) does not load it automatically: an embedding program owns its own environment (see [API reference](../api-reference.md)).

Other top-level keys you will meet later: `runtime.complexity` ([Complexity](10-complexity.md)), `runtime.persistence` and `runtime.plugins.<name>` — per-plugin settings such as MCP servers and a tool's `binary` ([Tools and persistence](13-tools-and-persistence.md)), `runtime.library.sources` and `runtime.state.record_children` ([Composition](09-composition.md)), `runtime.tools.timeout_seconds` — the default budget for `tool` effects, independent of any adapter's (see [Timeouts](#timeouts) below), `plugins` (runtime plugin identifiers), and the three allowlists.

## What a document can set

A document may carry a `runtime:` block and a top-level `plugins:` list of its own. Two `runtime` settings belong to its author and always apply: `runtime.complexity` and `runtime.state`, each replacing the config block of the same name for that run. Everything else under `runtime` — `adapters`, `plugins`, `persistence`, `library`, `mcp` — is a host setting, and so is a `plugins:` entry config does not already list in `plugins` or `enabled_plugins`. Whether a document's host settings apply depends on how it reached `cof`.

**A file you run by path is trusted**, the way a script you run is. `cof run ./my.yml`, `cof check` and `cof score` on a path, a local file picked in the TUI Run view, the SDK's `run_orchestration(orchestration_path=...)` and scheduler jobs apply the document's whole `runtime:` block and `plugins:` list. Naming the file is the decision to run it — unless the resolved path (symlinks followed) lies inside a refreshable (e.g. `github`) library source's own cache directory: that is fetched content regardless of what string named it, so it stays limited like a bare library-name run instead, on every surface (#343). When a trusted document sets host settings, `cof run` prints one line on stderr naming them — key paths only, never values — and `cof check` shows the same line:

```
Warning: Applied host settings from my.yml: runtime.adapters.openai.base_url, plugins: acme.telemetry
```

A file that sets only `runtime.complexity` / `runtime.state` prints nothing extra. From Python, `run_orchestration(..., trust_document=False)` opts a path out, for a file you did not choose yourself — and the cache-directory case above opts it out for you regardless of what `trust_document` says, since the SDK's own default is `True`.

**A document that arrives any other way is limited** to `runtime.complexity` and `runtime.state`: `cof run <name>` from a library source (bundled, folder or github), `cof run-library` and `run_shared_orchestration`, the MCP server's `run_orchestration` and `validate_orchestration` tools, the REST trigger, and plans a model generates. The TUI follows the same rule: its Library view's "run this entry" stays limited whatever the source, and its Chat view's "run it now" stays limited too — the document is still what the model just generated, even once it is saved to disk. Save it, then run that same file with `cof run f.yml` or pick it from the Run view's own local-file list, and it trusts like any other local file. Every limited document's host settings are ignored, and `cof run` (on stderr) and `cof check` print one warning per key saying it must go in config.json; an unlisted `plugins:` entry is skipped.

That limit is what makes it reasonable to run a document you did not write. The document chooses what the run does; config alone chooses where each adapter sends your prompts and API keys, which binaries and environment a tool gets, which MCP servers start, and where state is written. A composed `use:` child is held tighter still: it runs on the parent's resolved runtime and its own `runtime:` and `plugins:` keys are never read, however the parent was run. Note that `cof fetch` followed by `cof run ./fetched.yml` is running the file by path: read a fetched file before you run it that way, or run it through `cof run-library` — unless the saved copy's own path happens to land inside a configured source's cache directory, which stays limited for the reason above.

```json
{
  "trust_orchestration_runtime": true
}
```

`trust_orchestration_runtime: true` (or `CIRCUITRY_TRUST_ORCHESTRATION_RUNTIME=1`) trusts every document, however it arrived: its whole `runtime:` block is merged over config and every module in its `plugins:` list is imported, with the same one-line notice. Only set it on a host that runs nothing but documents you wrote or have read. With it on, any document the host runs can point an adapter at its own server — which then receives your prompts and the adapter's credentials — change a tool's `binary` or `env`, add an MCP server command, redirect persistence, or import arbitrary Python, and `use: ref:` means that includes every document your library sources can reach.

## Trusting a project config

A project config is picked up from whatever directory you run in, and it carries the same authority as your own global file: it can repoint an adapter (and the credentials sent with every prompt), choose the binary and environment a tool plugin executes, start MCP server commands, send run state to a persistence backend, and import runtime plugins. A cloned or downloaded repository can ship one. So, like `direnv allow`, Circuitry applies a discovered project config only after you have said so:

```bash
cof trust                  # the circuitry.config.json / config.json in this directory
cof trust path/to/config.json --yes
cof trust --list           # every trusted file, and whether it still matches
cof untrust                # stop applying it
```

`cof trust` prints every setting the file makes, flags the host-sensitive ones — adapter settings, a tool's `binary` or `env`, MCP servers, `plugins`, persistence, library sources, `trust_orchestration_runtime` — and asks before it records anything (`--yes` skips the question). Trust is kept in `~/.config/circuitry/trusted.json`, beside the global config, as the file's resolved path and the SHA-256 of its contents; your `config.json` is never rewritten. Edit the file and it stops applying until you trust it again.

Until then the file is skipped as a whole, and the run says so once, on stderr:

```
Warning: Skipped project config /home/you/repo/circuitry.config.json: it is not trusted, so none of its settings apply. Review it, then run `cof trust /home/you/repo/circuitry.config.json` to apply it.
```

The run itself goes ahead on the other layers, and exit codes do not change. `cof check` prints the same warning; `cof doctor` and the TUI's Doctor and Settings views show the file and its trust state.

Two kinds of file are always trusted, because you named them: the global config, and a file passed with `--config` or `CIRCUITRY_CONFIG`. For CI jobs and containers where the checked-out repository is your own, `CIRCUITRY_TRUST_PROJECT_CONFIG=1` trusts every discovered project config without a trust entry. That is the whole protection switched off: set it only where every directory a run can start in holds a config you would have trusted anyway.

## Capability consent for a fetched or referenced document

`cof trust` and the project-config rule above decide whether a document can change *host settings*. They say nothing about what the document's own effects do — and a document you did not write yourself can still shell out, evaluate Python, write or delete a file, or reach the network, the moment one of its tool effects runs. #275 adds a second, narrower gate for exactly that: a `cof run-library`/`run_shared_orchestration` asset, a remote (refreshable, e.g. `github`) library source run by bare name or by the raw path of that source's own local cache (#337), or any `use: ref:` child reached from *any* document, trusted or not, needs an explicit yes before its `shell`, `python_eval`, filesystem-write, or network tool effects run, the first time. As the note above says, `cof fetch` followed by `cof run ./fetched.yml` is a run by path — never gated here, same as any other local file (#284); read a fetched file before running it that way, or run it through `cof run-library`. That's a different file (your own `-o` destination) from a `github`-type source's own cache directory, where naming the cached file directly is still naming fetched content, gated the same as the library name would be (#337).

Every bundled tool plugin is tagged with what it can do — `shell`, `python_eval`, `fs-write`, `network`, some with more than one — in `circuitry.plugins.capabilities.PLUGIN_CAPABILITIES`; the full table is in the [Orchestration Reference](../orchestration-reference.md#effect-types). Before such a document runs, Circuitry works out, statically, which of the four its tool effects (and its own `use` children) need, and asks:

```
'/path/to/fetched.yml' needs: network, shell
Allow it? (recorded for this document until it changes) [y/N]:
```

A yes is recorded by the document's own content digest, in the same store `cof trust` keeps project-config trust in (`~/.config/circuitry/trusted.json`, a different top-level key) — an edited document (a changed digest) asks again. From a script or CI, where nothing can answer a prompt, the run simply refuses, naming what to do about it:

```bash
cof trust path/to/fetched.yml          # review it and consent permanently
cof run-library asset --allow-capabilities shell,network   # approve just this run, not persisted
```

`cof trust` on a `.yml`/`.yaml` path shows the capabilities a document needs (the same walk the in-run prompt does) instead of config settings, and asks the same way `cof trust` on a project config does. A `use: ref:` child is gated independently of whatever document pulled it in — including one reached from a path-trusted document you wrote yourself, since the child's content still isn't yours; a `use: path:`/`inline:` child is not independently gated, only held to whatever capabilities the enclosing document already has consent for — the same reasoning that keeps it from setting its own host settings above. A reflector or decomposition plan generated inside a consented document is held to that same ceiling, never its own prompt.

A file you run by path, with no `use: ref:` in it, is never asked: naming it by path is the decision to run it, same as the host-settings rule above — unless the resolved path (symlinks followed) lies inside a refreshable source's own cache directory, in which case it is fetched content regardless of what string named it (#337). This gate applies the same way on every surface, not just `cof run` (#334): the TUI's Library view carries whether an entry came from a remote source through to its Run-view hand-off (and `cof run --last`'s replay, from the TUI's Runs view too — including a `cof run-library` run's own stash, #337); `circuitry.api.run_orchestration`/`run_shared_orchestration` take their own `allow_capabilities=` parameter — the embedding program's own consent for one call, since it is the host; and the MCP server resolves a bare orchestration name against the same registry `cof run` builds — or, when a caller names a path instead, the run itself still classifies it by the same cache-directory check (#337), so a cache path cannot stand in for the host's own by-path trust there either. MCP and the REST trigger have no field a caller can use to grant capabilities themselves — only `cof trust`'s own record of a previously-consented digest does that there — and the TUI refuses the same way rather than opening a dialog, a deliberate choice to keep one message and one path to approve a document rather than a second, TUI-only grant mechanism; all three simply refuse, naming `cof trust <document>` the same way a script or CI invocation of `cof run` does. See [Threat Model §9](../threat-model.md#9-capability-consent-for-a-document-that-is-not-the-users-own) for the full gate.

## Which model actually runs

Config sets the default, but it is one voice among several. The full ladder for a prompt's model, most authoritative first:

```
a profile's per-effect model override
  > the effect's own model:
    > --model on the command line
      > a profile's run-level model:
        > a profile's per-effect routing pin
          > the complexity router
            > the document's top-level model:
              > CIRCUITRY_MODEL
                > project config default_model
                  > global config default_model
                    > the built-in default
```

Everything above the router is a model a human named on purpose, and the router never overrules one. Everything below it is a default the router exists to replace. Two consequences worth stating: an effect's own `model:` beats `--model`, because `--model` sets the *run default* and a step that names its own model has opted out of the run default whoever supplied it; and a profile is how you retarget one step from outside the document without editing it.

Adapters resolve the same way minus the router: `--adapter` > profile > document `adapter:` > `CIRCUITRY_ADAPTER` > config.

The less usual part is that every one of these decisions is recorded. `runtime.effective_settings.sources` names the layer that supplied each setting — `cli`, `profile`, `orchestration`, `config`, `default`, or `router` — and each prompt's `meta.model_reason` says `explicit`, `default`, or `router`. You can reconstruct why a model ran from the state file alone, months later. `cof info <orchestration>` shows the resolved complexity settings and their provenance before you run anything.

## Models are opaque strings

The runtime does not interpret model names. For most adapters they are provider model identifiers — `llama3.1:8b`, `gpt-4o-mini`. For a broker they can be something else entirely: the [CyberDiner](https://github.com/KenanKStipek/CyberDiner) adapter is a job-queue LLM network, and there `model:` names a *capability tier* — `cheap`, `fast`, `good`, `good-fast`, `alpha` — that the network resolves to whatever is serving that tier. The adapter hides submit-and-poll behind the same synchronous `generate()` every other adapter implements, so the document neither knows nor cares. `cof list --models <adapter>` asks an adapter what it can serve.

Thirty-one adapters ship in-tree behind one `Adapter` protocol — hosted APIs, self-hosted servers (vllm, llama.cpp, LM Studio, TGI), aggregator routes (OpenRouter, LiteLLM), the CyberDiner broker, two coding-agent CLIs (`pi`, `claude_code`, below), and `host_claude`, the adapter that turns a Claude session into the model over MCP ([Surfaces](12-surfaces.md)). `cof list --extensions` prints the compiled-in set. [Tools and persistence](13-tools-and-persistence.md) covers writing one.

## A coding-agent CLI's own login

Every other adapter needs an API key or a local model server. `pi` and `claude_code` need neither: each runs a coding-agent CLI — pi or Claude Code — headlessly for one completion and uses whatever that CLI is logged in to, such as a Claude subscription. The CLI's own tools are off and no session is saved; the prompt is just a prompt.

```json
{
  "default_adapter": "claude_code",
  "default_model": "claude-sonnet-4-5",
  "runtime": {
    "adapters": {
      "claude_code": { "timeout_seconds": 600 },
      "pi": {
        "binary": "pi",
        "thinking": "low",
        "timeout_seconds": 600,
        "unset_env": ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"]
      }
    }
  }
}
```

The model is the CLI's own name for it: a Claude Code model for `claude_code`, pi's `provider/id` (`claude-bridge/claude-sonnet-4-5`) for `pi` — so a prompt can pick either with `provider: "pi:claude-bridge/claude-sonnet-4-5"`. The CLI runs with your environment minus two things: the variables that would make it think it is part of a parent pi or Claude Code session — Circuitry may well be running inside one — and `unset_env`, by default `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN`, so an exported key does not quietly replace the login you meant to use. Give it a generous `timeout_seconds`: a CLI retries a failing request on its own before it reports the error, and a timeout or a cancelled run stops the CLI and everything it started. `cof doctor` reports a CLI that is not installed as `binary:pi` or `binary:claude`; an auth failure or a CLI too old for the model fails the prompt with the CLI's own message. `prompt_type: json` works on both, and on `claude_code` the schema goes to the CLI itself. Tokens land in `meta` as usual, and the cost the CLI reports in `meta.cost_usd`. The [Orchestration Reference](../orchestration-reference.md#coding-agent-cli-adapters-pi-and-claude_code) has the exact commands and every key.

## Profiles

A profile is a YAML file that overlays one run — defaults, inputs, per-effect overrides, persistence — without touching the orchestration. This one keeps every model call on this machine: a small local model reads the issue, a larger local model writes the patches:

```yaml
# profiles/local.yml
adapter: ollama
out: runs/local.json
inputs:
  repo: "."                      # merged under input.*; -e still wins
effects:                         # keyed by effect path, as in state minus the prime. root
  error:
    model: llama3.2:3b
  patch:
    model: qwen2.5-coder:14b
    provider: ollama
  pre_review:
    enabled: false               # switch an effect off for this run
  plan:
    routing: heavy               # pin to a named routing band (chapter 10)
persistence:
  backend: jsonl-file
  path: runs.jsonl
```

```bash
cof run issue_to_pr.yml --profile local
```

The effect paths are those of the agent's whole document, which [Issue to pull request](14-issue-to-pull-request.md) builds.

Profiles are discovered at `<orchestration_dir>/profiles/<name>.yml` first, then `<cwd>/profiles/<name>.yml`, and validated against a schema — an unknown effect path fails with the list of valid ones. `enabled: false` is the flagship: it turns a reflector's agentic planning off for one run and leaves everything else intact, writing a skip node in its place so downstream paths still resolve (to `null`). A container disabled this way disables its whole subtree; a condition (`if:` on an `if`, `while:` on a loop) cannot be disabled on its own — disable the container.

The recorded profile is enough to reproduce the run without the file: `cof run issue_to_pr.yml --profile-from-state ./runs/local.json` reconstructs it from `runtime.effective_settings.profile`. Redacted secrets are the exception — reconstruction refuses to replay a `***REDACTED***` sentinel and tells you to bring the file.

[Named Profiles](../profiles.md) has the full precedence rules and the persistence table.

## Preflight and allowlists

Every adapter, tool plugin, and runtime plugin implements a `check()`. Before the first model call, `cof run` runs the checks for every extension the orchestration references — the adapter is reachable, the plugin's binary is installed, the API key is set — and refuses to start a run that would fail halfway. `cof doctor` runs every check and reports; `cof doctor --generate` also makes a live model call. `--skip-preflight` bypasses the gate when you know better.

An adapter that only optional steps use does not block the run. When every prompt on an adapter has `on_error: skip` or `continue`, a missing credential for it is a warning, not a failure, and those steps skip. [Errors](05-errors.md) has the exact rule.

The allowlists are the other gate: `enabled_adapters`, `enabled_tools`, and `enabled_plugins` in config (or their `CIRCUITRY_ENABLED_*` environment forms) restrict which extensions a run may touch. Unset means everything compiled in is available; set means an orchestration referencing anything else fails at validation, before any call. `cof check` follows the document's `use` children — `path:`, `ref:` and plain `inline:` — and applies the same lists to each. At run time every adapter and tool is checked again as it is built, which covers what no document text shows: a templated `inline:` child once it renders, reflector and decomposition plans, `--adapter`, the config's `default_adapter`, and a profile's `provider` overrides. `cof run-library --service-profile` keeps the lists too. In a deployment that should never shell out, `enabled_tools` is where you say so. [Threat Model](../threat-model.md) covers the reasoning. Capability consent (above) is a separate, narrower gate for the same class of risk: an allowlist is a host-wide policy an operator sets once; consent is a per-document, one-time yes for a document that did not come from the operator's own disk.

## Timeouts

Three clocks, two of them in config. `timeout_ms` on an effect bounds that effect, rounded up to the next whole second if you give it a sub-second value — a budget like `250` never floors to an instant 0-second timeout. `runtime.adapters.<name>.timeout_seconds` in config bounds that adapter's own socket, for `prompt` effects — a large local model can need cold-load headroom well beyond any single effect's budget, which is why the example above gives ollama ten minutes. It is read for whichever adapter an attempt actually dispatches to, not just the run default: a prompt with `provider: ollama` (or a `provider_fallbacks` entry naming it) uses `runtime.adapters.ollama.timeout_seconds` for that attempt even when `default_adapter` is something else entirely. `runtime.tools.timeout_seconds` (default 300s) is the same idea for `tool` effects, and deliberately separate from the adapter's: a tool run on the no-op adapter, or a slow `ffmpeg` pass alongside a fast model, each get their own number instead of inheriting whichever adapter the run happens to be using. All three compose: the effect's `timeout_ms` is the one you tune per step; the other two are the ones you set once per machine. Not every tool plugin can actually be bounded this way — see the `tool` effect's [Timeout](../orchestration-reference.md#tool) section for which ones do. (`litellm`'s own timeout key is `runtime.adapters.litellm.timeout_seconds`, the same spelling as every other adapter; the earlier `timeout` key still works but is deprecated and logs a warning.) [Errors](05-errors.md) is next.

## Concurrency limits

`max_concurrency` on a `loop`/`dynamic` effect (see [Loop](07-loop.md), [Dynamic](02-dynamic.md)) only ever bounds that one container's own fan-out. Nest a few of those — a tree loop inside a parallel dynamic inside another tree loop — and the actual number of effects dispatching at once is the *product* of every level's own pool size, not any one of them. `runtime.max_concurrency` and `runtime.concurrency_groups` are the run-wide answer: one cap (or a set of named caps) shared by every `tool`/`prompt` effect dispatched anywhere in the run, regardless of which loop, dynamic, or `use` child it sits inside.

```yaml
runtime:
  max_concurrency: 4
  concurrency_groups:
    gpu: 1
```

`max_concurrency` here is a single pool: at most 4 leaf effects run at once, full stop, no matter how many containers are fanned out in parallel above them. `concurrency_groups` adds named pools on top of it — a `tool`/`prompt` effect opts into one with its own `group: <name>` field, and then waits for a free slot in *both* that group and the run-wide pool (if both are set) before it dispatches. This is how you protect something physically singular: one GPU, one ComfyUI worker, one third-party API that only accepts one request at a time — give its tool calls `group: gpu`, set `concurrency_groups: {gpu: 1}`, and every parallel branch that reaches one waits its turn, however deep the nesting above it.

Only `tool` and `prompt` — the leaves — ever hold a slot. A container never dispatches itself, so `group:` on one is rejected by `cof check`, and a `group:` naming something `concurrency_groups` doesn't define is too. This is also why nesting can't deadlock: a container never holds a slot its own children are waiting on, so a blocked leaf is only ever waiting on another leaf's own unconditional, eventual release. A blocked effect's own node shows `meta.waiting_for` (`"global"` or the group name) for as long as it's waiting, visible in `--live-state` and the verbose CLI output, cleared once it gets a slot.

Like every other `runtime:` key besides `complexity`/`state`, both settings are host-level: set them in `config.json`, or in the top-level document's own `runtime:` block if it's trusted (run by path — see [What a document can set](#what-a-document-can-set)). A document reached through a `use ref:`, a library name, MCP, or the REST trigger cannot set either, by the same rule that keeps it from repointing an adapter — a shared, physically limited resource is a host decision, not something any document that happens to run should be able to loosen.

See the orchestration reference's [Concurrency Limits](../orchestration-reference.md#concurrency-limits) for the full field reference.

## Anti-patterns

**Hard-wiring the provider.** `adapter: openai` at the top of a document that is meant to be shared. Put it in config, or in a profile.

**Credentials in YAML.** Never. Environment or config; the run record redacts config, and nothing redacts a template.

**Reaching for `--model` to retarget one step.** `--model` sets the run default; a per-effect `model:` in the document beats it. Use a profile's `effects:` block to retarget a single step from outside.

**Trusting `PATH`.** The dev tooling is pinned in `requirements-dev.txt` so that green locally means green in CI. The same discipline applies to models: pin the model in config, not in memory.

## See also

- [Named Profiles](../profiles.md).
- [Library Sources](../library-sources.md) — `runtime.library.sources`.
- [CyberDiner demo runbook](../cyberdiner-demo-runbook.md) — a broker adapter end to end.
- [Concurrency Limits](../orchestration-reference.md#concurrency-limits) — `runtime.max_concurrency` and `runtime.concurrency_groups` field reference.
