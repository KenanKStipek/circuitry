# Configuration

Capability lives in config, deliberately outside orchestration documents. A document describes *what to do* — the topology, the prompts, the conditions. Config describes *what can do it* — which adapter, which model, which timeout, which persistence backend. Keep the two apart and an orchestration written against a local 8B model this morning runs against a hosted frontier model this afternoon with no edit, and the complexity router in Part III can choose per prompt.

The rule of thumb: an orchestration in the curation library never sets `adapter:` or `model:`. Both fields are legal at the top of a document and on every prompt, and there are moments to use them — a step that must run on a specific model whatever the run says — but each one is a small hard-wiring, and the router steps around it.

## Where config comes from

Resolution is layered, each layer deep-merged over the last:

1. **Sane defaults** — `ollama` at `http://localhost:11434`, `llama3.1:8b`.
2. **Global config** — `~/.config/circuitry/config.json`.
3. **Project config** — `circuitry.config.json` or `config.json` in the working directory.
4. **An explicit file** — `--config <path>` or `CIRCUITRY_CONFIG`, which *replaces* 2 and 3 rather than layering over them.
5. **Environment variables** — `CIRCUITRY_ADAPTER`, `CIRCUITRY_MODEL`, `CIRCUITRY_ADAPTER_URL`, `CIRCUITRY_COMFYUI_URL`, and the allowlists `CIRCUITRY_ENABLED_ADAPTERS` / `CIRCUITRY_ENABLED_TOOLS` / `CIRCUITRY_ENABLED_PLUGINS`.

`cof setup` walks you through creating the global file — it detects local backends and writes a working config. `cof init` writes a project config and a `hello.yml` beside it. `cof doctor` tells you what the resolved config can actually reach.

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

Other top-level keys you will meet later: `runtime.complexity` ([Complexity](10-complexity.md)), `runtime.persistence` ([Tools and persistence](13-tools-and-persistence.md)), `runtime.library.sources` ([Composition](09-composition.md)), `plugins` (runtime plugin identifiers), and the three allowlists.

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

```yaml
# profiles/fast.yml
adapter: ollama
model: llama3.2
out: runs/fast.json
inputs:
  occasion: "weeknight"          # merged under input.*; -e still wins
effects:                         # keyed by effect path, as in state minus the prime. root
  plan_courses:
    model: cheap
    provider: cyberdiner
  season:
    enabled: false               # switch an effect off for this run
  replan_service:
    routing: premium             # pin to a named routing band (chapter 10)
persistence:
  backend: jsonl-file
  path: runs.jsonl
```

```bash
cof run dinner.yml --profile fast
```

Profiles are discovered at `<orchestration_dir>/profiles/<name>.yml` first, then `<cwd>/profiles/<name>.yml`, and validated against a schema — an unknown effect path fails with the list of valid ones. `enabled: false` is the flagship: it turns a reflector's agentic planning off for one run and leaves everything else intact, writing a skip node in its place so downstream paths still resolve (to `null`). A container disabled this way disables its whole subtree; a condition (`if:` on an `if`, `while:` on a loop) cannot be disabled on its own — disable the container.

The recorded profile is enough to reproduce the run without the file: `cof run dinner.yml --profile-from-state ./runs/fast.json` reconstructs it from `runtime.effective_settings.profile`. Redacted secrets are the exception — reconstruction refuses to replay a `***REDACTED***` sentinel and tells you to bring the file.

[Named Profiles](../profiles.md) has the full precedence rules and the persistence table.

## Preflight and allowlists

Every adapter, tool plugin, and runtime plugin implements a `check()`. Before the first model call, `cof run` runs the checks for every extension the orchestration references — the adapter is reachable, the plugin's binary is installed, the API key is set — and refuses to start a run that would fail halfway. `cof doctor` runs every check and reports; `cof doctor --generate` also makes a live model call. `--skip-preflight` bypasses the gate when you know better.

The allowlists are the other gate: `enabled_adapters`, `enabled_tools`, and `enabled_plugins` in config (or their `CIRCUITRY_ENABLED_*` environment forms) restrict which extensions a run may touch. Unset means everything compiled in is available; set means an orchestration referencing anything else fails at validation, before any call. In a deployment that should never shell out, `enabled_tools` is where you say so. [Threat Model](../threat-model.md) covers the reasoning.

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
