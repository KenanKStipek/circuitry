# electricity

electricity is a Rust runner for Circuitry orchestrations: a CLI binary and an
embeddable library crate that compile a Circuitry document and `config.json`
to an internal bytecode and execute it, producing the same final state as
`cof run`. `cof` (the Python package in this repository) stays the authoring
tool — schema checking, linting, the wizard, document generation; electricity
is the production runner.

**This is a preview.** `electricity <config.json> <orchestration.yml>` runs
documents built from `tool`, `dynamic`, `if`, `loop`, `use`, `yield`,
`prompt` and `reflector` effects and `finally:` blocks, with the `json`,
`shell`, `awk`, `imagemagick`, `ffmpeg`, `fs`, `env_vars`, `hash`, `uuid`,
`clock`, `regex`, `http` and `validate_yaml` tool providers, aiming at the
same final state, `--live-state` and `--events` output as `cof run`. A
top-level `prompts:` map and `{{> name}}` composition run too, in every
field that reaches them (a tool's `prompt`/`params`/`params_json`, a
`use`'s `inputs`/`inline`, a `yield`'s `template`, a `prompt`'s own
`template`/`messages`). A `prompt` effect runs for real against a
document's own top-level `adapter:`/a prompt's own `provider:` (native
Ollama, the OpenAI-compatible family, `openai`, `anthropic`, `replicate`,
`watsonx`, `cyberdiner`, `claude_code`, `pi`, and the `scripted` adapter,
or the config's own `default_adapter` when a prompt names none) with its
preflight check run for real too -- `litellm` is the only Circuitry-known
adapter name with no electricity builder, and stays refused permanently
(it embeds the Python `litellm` package, which electricity neither embeds
nor reimplements), as does a `reflector`'s own generated plan naming it.
Model-mode `if`/`while` conditions and `expect:` run too. `--profile
<path>` loads a profile (adapter/model/inputs/persistence overrides,
routing pins and provider denials) from an explicit path -- no name
discovery. `runtime.complexity` scoring, model routing and decomposition
(splitting an over-threshold prompt into chunks, planned and executed as
its own isolated child run) all run too, writing
`meta.complexity`/`meta.decomposition` the same way `cof run` does. [What
electricity can't run yet](#what-electricity-cant-run-yet), below, still
refuses with a preview message pointing at `cof run`. A document and its
literal `use` children are checked before the run starts; an inline or
templated `use` child is checked when it is loaded. `electricity --help`
lists the options, and `--dump-ir` prints the compiled IR as unstable
debugging JSON. Progress is tracked in issue #358. electricity shares the
Python package's version and release tags; from this repository's next
release on, its binaries and a container image are attached to the GitHub
release alongside the Python distributions, marked as a preview.

## What electricity can't run yet

- **Tool providers.** electricity builds 13 of Circuitry's 74 tool
  plugins (the list above). A document, or a `use: {path:}` child, naming
  any of the other 61 is refused before the run starts; a plan compiled
  mid-run that names one (an inline `use`, a reflector plan or a
  decomposition plan) stops the run there instead, and `on_error` never
  absorbs it. A name that isn't one of Circuitry's plugins at all fails
  the way `cof run` does, with its `Unknown plugin` error. `mcp`,
  `surrealdb` and `email_smtp` are not built yet; the external plugin
  bridge, which brings `python_eval` and the rest, isn't either.
- **Text encodings.** In `fs` and `hash`, any encoding other than UTF-8,
  ASCII or Latin-1 fails the effect with the preview message.
- **Adapters.** `litellm` is refused for good (above). `host_claude`
  can't be built from config on either engine.
- **Persistence and resume.** `runtime.persistence` is refused. There is
  no `--resume` or `--state` yet.
- **Runtime plugins** are refused: the `plugins:` list (the SQL,
  SurrealDB and JSONL observability plugins, and OpenTelemetry).
- **CLI.** The config is a required argument: no config discovery, no
  `cof trust`. `--profile` takes a path, not a name. There is no
  `--dry-run`, `--validate-only`, `--verbose` or `--skip-preflight`.
  `.toon` documents and `use: {ref: ...}` are refused. Windows isn't
  supported.
- **Known gaps, tracked:** schema error wording and order (#528); rare
  JSON Schema shapes (#507); a reflector plan containing a date (#512);
  lint advisories (#428).

## Build and test

From the repository root:

```bash
cd electricity
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
```

`cargo build --release -p electricity-cli` produces the `electricity` binary
at `target/release/electricity`.

Every folder under `crates/` is a workspace member (`members = ["crates/*"]`): a new
crate needs no edit to the member list, and a stray folder there without a
`Cargo.toml` breaks every cargo command.

Generators in `scripts/generate_*.py` write checked-in files from Circuitry's
own Python code: copies of its schemas, and expected outputs from its real
loaders, renderers and evaluators. Each one supports `--check`. CI runs all of
them (`pip install -e . -c electricity/scripts/generator-constraints.txt` from
the repository root first, with Python 3.11, then
`python3 scripts/generate_<name>.py --check` from `electricity/`) in
`.github/workflows/electricity-generated.yml`, for any change to Circuitry's
Python package as well as to `electricity/`, because a change on either side
can make a committed file stale. When one is stale, run that generator
without `--check` and commit the result; the Rust tests then replay it.

## Read more

- [`DESIGN.md`](DESIGN.md) — the full design: architecture, the Value model,
  the bytecode and VM, tools and adapters, the conformance suite, milestones.
- [`docs/spec/runtime-semantics.md`](docs/spec/runtime-semantics.md) — the
  behavioural specification electricity implements.
- [`docs/spec/tools-adapters-plugins.md`](docs/spec/tools-adapters-plugins.md)
  — the tool, adapter, and plugin protocol specification.
- [`docs/research/rust-ecosystem.md`](docs/research/rust-ecosystem.md) — the
  crate survey behind the dependency choices in `DESIGN.md`.
