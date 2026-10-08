# electricity

electricity is a Rust runner for Circuitry orchestrations: a CLI binary and an
embeddable library crate that compile a Circuitry document and `config.json`
to an internal bytecode and execute it, producing the same final state as
`cof run`. `cof` (the Python package in this repository) stays the authoring
tool — schema checking, linting, the wizard, document generation; electricity
is the production runner.

**This is a preview.** The crates here are a workspace skeleton only: the
`electricity` binary accepts `--version`/`--help` and otherwise reports that
it cannot run orchestrations yet and exits non-zero. There is no compiler,
VM, tool, or adapter implementation in this release. electricity shares the
Python package's version and release tags; from this repository's next
release on, its binaries and a container image are attached to the GitHub
release alongside the Python distributions, marked as a preview.

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
