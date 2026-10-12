# electricity

electricity is a Rust runner for Circuitry orchestrations: a CLI binary and an
embeddable library crate that compile a Circuitry document and `config.json`
to an internal bytecode and execute it, producing the same final state as
`cof run`. `cof` (the Python package in this repository) stays the authoring
tool — schema checking, linting, the wizard, document generation; electricity
is the production runner.

**This is a preview.** `electricity <config.json> <orchestration.yml>` runs
documents built from `tool`, `dynamic`, `if`, `loop`, `use` and `yield`
effects and `finally:` blocks, with the `json`, `shell`, `awk`,
`imagemagick`, `ffmpeg`, `fs`, `env_vars`, `hash`, `uuid` and `clock` tool
providers, aiming at the same final state, `--live-state` and `--events`
output as `cof run`. A top-level `prompts:` map and `{{> name}}` composition
run too, in every field that reaches them (a tool's `prompt`/`params`/
`params_json`, a `use`'s `inputs`/`inline`, a `yield`'s `template`).
A document whose own top-level `adapter:`/a prompt's own `provider:` names a
real provider (`ollama`, `openai`, `anthropic`, the OpenAI-compatible family,
`replicate`, `watsonx`) now has its preflight check run for real against that
provider -- `claude_code`, `cyberdiner`, `litellm` and `pi` are the only
Circuitry-known adapter names with no electricity builder yet. Prompt
effects themselves (dispatching a real model call), reflectors, the other
tool providers, persistence, runtime plugins and complexity routing are
still refused with a preview message pointing at `cof run`. A document and its literal
`use` children are checked before the run starts; an inline or templated
`use` child is checked when it is loaded. `electricity --help` lists the
options, and `--dump-ir` prints the compiled IR as unstable debugging JSON.
Progress is tracked in issue #358. electricity shares the Python package's
version and release tags; from this repository's next release on, its
binaries and a container image are attached to the GitHub release alongside
the Python distributions, marked as a preview.

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
