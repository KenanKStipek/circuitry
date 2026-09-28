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

`cof setup` walks you through creating the global file — it detects local backends and writes a working config. `cof init` writes a project config and a `hello.yml` beside it, and trusts the config it wrote. `cof doctor` tells you what the resolved config can actually reach.

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

Other top-level keys you will meet later: `runtime.complexity` ([Complexity](10-complexity.md)), `runtime.persistence` and `runtime.plugins.<name>` — per-plugin settings such as MCP servers and a tool's `binary` ([Tools and persistence](13-tools-and-persistence.md)), `runtime.library.sources` and `runtime.state.record_children` ([Composition](09-composition.md)), `plugins` (runtime plugin identifiers), and the three allowlists.

## What a document can set

A document may carry a `runtime:` block and a top-level `plugins:` list of its own. Two `runtime` settings belong to its author and always apply: `runtime.complexity` and `runtime.state`, each replacing the config block of the same name for that run. Everything else under `runtime` — `adapters`, `plugins`, `persistence`, `library`, `mcp` — is a host setting, and so is a `plugins:` entry config does not already list in `plugins` or `enabled_plugins`. Whether a document's host settings apply depends on how it reached `cof`.

**A file you run by path is trusted**, the way a script you run is. `cof run ./my.yml`, `cof check` and `cof score` on a path, a local file picked in the TUI Run view, the SDK's `run_orchestration(orchestration_path=...)` and scheduler jobs apply the document's whole `runtime:` block and `plugins:` list. Naming the file is the decision to run it. When it sets host settings, `cof run` prints one line on stderr naming them — key paths only, never values — and `cof check` shows the same line:

```
Warning: Applied host settings from my.yml: runtime.adapters.openai.base_url, plugins: acme.telemetry
```

A file that sets only `runtime.complexity` / `runtime.state` prints nothing extra. From Python, `run_orchestration(..., trust_document=False)` opts a path out, for a file you did not choose yourself.

**A document that arrives any other way is limited** to `runtime.complexity` and `runtime.state`: `cof run <name>` from a library source (bundled, folder or github), `cof run-library` and `run_shared_orchestration`, the MCP server's `run_orchestration` and `validate_orchestration` tools, the REST trigger, and plans a model generates. The TUI follows the same rule: its Library view's "run this entry" stays limited whatever the source, and its Chat view's "run it now" stays limited too — the document is still what the model just generated, even once it is saved to disk. Save it, then run that same file with `cof run f.yml` or pick it from the Run view's own local-file list, and it trusts like any other local file. Every limited document's host settings are ignored, and `cof run` (on stderr) and `cof check` print one warning per key saying it must go in config.json; an unlisted `plugins:` entry is skipped.

That limit is what makes it reasonable to run a document you did not write. The document chooses what the run does; config alone chooses where each adapter sends your prompts and API keys, which binaries and environment a tool gets, which MCP servers start, and where state is written. A composed `use:` child is held tighter still: it runs on the parent's resolved runtime and its own `runtime:` and `plugins:` keys are never read, however the parent was run. Note that `cof fetch` followed by `cof run ./fetched.yml` is running the file by path: read a fetched file before you run it that way, or run it through `cof run-library`.

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

Twenty-nine adapters ship in-tree behind one `Adapter` protocol — hosted APIs, self-hosted servers (vllm, llama.cpp, LM Studio, TGI), aggregator routes (OpenRouter, LiteLLM), the CyberDiner broker, and `host_claude`, the adapter that turns a Claude session into the model over MCP ([Surfaces](12-surfaces.md)). `cof list --extensions` prints the compiled-in set. [Tools and persistence](13-tools-and-persistence.md) covers writing one.

## Profiles

A profile is a YAML file that overlays one run — defaults, inputs, per-effect overrides, persistence — without touching the orchestration:

A profile that keeps every model call on this machine — a small local model reads the issue, a larger local model writes the patches:

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

The allowlists are the other gate: `enabled_adapters`, `enabled_tools`, and `enabled_plugins` in config (or their `CIRCUITRY_ENABLED_*` environment forms) restrict which extensions a run may touch. Unset means everything compiled in is available; set means an orchestration referencing anything else fails at validation, before any call. `cof check` follows the document's `use` children — `path:`, `ref:` and plain `inline:` — and applies the same lists to each. At run time every adapter and tool is checked again as it is built, which covers what no document text shows: a templated `inline:` child once it renders, reflector and decomposition plans, `--adapter`, the config's `default_adapter`, and a profile's `provider` overrides. `cof run-library --service-profile` keeps the lists too. In a deployment that should never shell out, `enabled_tools` is where you say so. [Threat Model](../threat-model.md) covers the reasoning.

## Timeouts

Two clocks. `timeout_ms` on an effect bounds that effect. `runtime.adapters.<name>.timeout_seconds` in config bounds the adapter's socket — and a large local model can need cold-load headroom well beyond any single effect's budget, which is why the example above gives ollama ten minutes. The two compose: the effect's budget is the one you tune per step; the adapter's is the one you set once per machine. [Errors](05-errors.md) is next.

## Anti-patterns

**Hard-wiring the provider.** `adapter: openai` at the top of a document that is meant to be shared. Put it in config, or in a profile.

**Credentials in YAML.** Never. Environment or config; the run record redacts config, and nothing redacts a template.

**Reaching for `--model` to retarget one step.** `--model` sets the run default; a per-effect `model:` in the document beats it. Use a profile's `effects:` block to retarget a single step from outside.

**Trusting `PATH`.** The dev tooling is pinned in `requirements-dev.txt` so that green locally means green in CI. The same discipline applies to models: pin the model in config, not in memory.

## See also

- [Named Profiles](../profiles.md).
- [Library Sources](../library-sources.md) — `runtime.library.sources`.
- [CyberDiner demo runbook](../cyberdiner-demo-runbook.md) — a broker adapter end to end.
