# Research: Rust ecosystem for electricity (a Circuitry runtime with output identical to Python `cof`)

Research date: 2026-10-06. Crate versions come from the crates.io API on that date unless noted otherwise.
All versions can go stale within weeks. Re-check them with `cargo search` / crates.io before pinning.

## Summary

Every area except one has a sound Rust crate. **The real risk is parity, not crate quality.** Three
Python components define behaviour that no Rust crate reproduces out of the box:

- **PyYAML** follows YAML 1.1 and has its own quirks.
- **chevron** deviates from the Mustache spec and renders values with Python's `str()`.
- **cel-python** is only partly spec-conformant, and its exception text is written into the state.

My recommendation is to put a **Python-semantics `Value` type** at the centre of electricity and build
three small, owned components around it:

- a PyYAML-compatible composer on top of `saphyr-parser`;
- a port of chevron's renderer (about 300 lines);
- a `json.dumps`-compatible serializer.

For CEL, use a crate (`cel` or `cel-core`), but run it behind a differential test corpus taken from
cel-python. Everything else (tokio, reqwest, rmcp, opentelemetry, sqlx, lettre, process-wrap, cargo-dist)
is mature and low risk. One exception: the `surrealdb` crate is licensed **BSL 1.1**, not OSI open
source, so talking to SurrealDB over its HTTP/WebSocket RPC directly is cleaner for an MIT binary.

## Findings

Labels used below: **Direct** means the source states it. **Interp.** means my reading of the source.
**Inference** means my own judgement.

### 0. Cross-cutting: what Circuitry does that makes parity hard (local evidence)

1. **Claim:** Circuitry calls `chevron.render(template, ctx)` with default arguments. That means
   `partials_path='.'` and `partials_ext='mustache'`, so `{{> name}}` reads `./name.mustache` from the
   **process working directory**. Rendering errors become `TemplateError`.
   **Sources:** `src/circuitry/core/templates.py`;
   [chevron renderer.py](https://raw.githubusercontent.com/noahmorrison/chevron/main/chevron/renderer.py).
   **Support:** Direct. **Confidence:** high.
2. **Claim:** A failed condition is recorded as `meta["error"] = str(e)`. Exception *text* from CEL (and
   probably from other libraries) therefore ends up in the final state JSON.
   **Sources:** `src/circuitry/core/conditional.py` (around line 207).
   **Support:** Direct. **Confidence:** high.
   **Inference:** the conformance suite must either require exact error strings or normalize
   `meta.error`. This is an owner decision, and it changes how much work items 2, 5 and 6 need.
3. **Claim:** Circuitry adds its own CEL semantics on top of cel-python. In non-strict mode an unset
   `state.` path reads as false. `strict: true` raises instead.
   **Sources:** `conditional.py` `ConditionDef.strict`. **Support:** Direct. **Confidence:** high.
   **Inference:** electricity must reproduce this layer whichever CEL crate it uses.
4. **Claim:** Circuitry depends on `pyyaml>=6.0`, `jsonschema>=4.0`, `chevron>=0.14`, `cel-python>=0.3`
   and `mcp>=2.0,<3`. OpenTelemetry uses the HTTP OTLP exporter (`opentelemetry-exporter-otlp-proto-http`).
   **Sources:** Circuitry's `pyproject.toml`. **Support:** Direct. **Confidence:** high.
   The cel-python floor is 0.3, but the current release is 0.5.0 (2026-01-31), and behaviour differs by
   version (0.5 moved `matches()` to RE2 only). Parity must pin a specific cel-python version.
5. **Inference (design):** do not use `serde_json::Value` as the runtime value type. Python state can
   hold things JSON cannot:
   - int and bool dict keys (`yes:` → `True` → JSON key `"true"`);
   - big ints;
   - `datetime.date` values from YAML timestamps;
   - `NaN`.

   It also needs Python `str()`/`repr()` behaviour for chevron. Define an electricity `Value` with
   these variants: `None`, `Bool`, `Int` (i64 + BigInt fallback), `Float`, `Str`, `List`, `Dict`
   (IndexMap keyed by a hashable `Value`), `Date` and `DateTime`. Give it `py_str`, `py_repr` and
   `to_json_python()`.

### 1. YAML: match PyYAML `safe_load`

6. **Claim:** All maintained pure-Rust parsers target YAML **1.2**: `saphyr` 0.1.0 / `saphyr-parser`
   0.1.0 (MIT OR Apache-2.0, released 2026-09-19) and `yaml-rust2` 0.13.0 (2026-09-11). Both pass the
   yaml-test-suite. **Sources:** [saphyr repo](https://github.com/saphyr-rs/saphyr),
   [crates.io saphyr](https://crates.io/crates/saphyr),
   [yaml-rust2 docs](https://docs.rs/crate/yaml-rust2/latest). **Support:** Direct. **Confidence:** high.
7. **Claim:** `saphyr-parser` produces an event stream. Each `Scalar` event carries its `ScalarStyle`
   (plain or quoted), its anchor id and an optional `Tag`, and the parser has span/`Marker` support.
   That is everything needed to run **our own YAML 1.1 resolver and composer**.
   **Sources:** `parser/src/parser.rs`, `parser/tests/span.rs` in the saphyr repo.
   **Support:** Direct (API). **Confidence:** high.
8. **Claim:** PyYAML's implicit resolvers are a short list of regexes. They have quirks that YAML 1.2
   parsers do not share:
   - Booleans are `yes/no/on/off/true/false` in three casings each. `y`/`n` stay strings.
   - A float **must contain a `.`**, and an exponent **must have a sign**. So `1e3` and `1.0e3` load
     as **strings**, while `1.0e+3` loads as a float.
   - Ints include `0b…`, legacy octal `0[0-7_]+`, `0x…`, sexagesimal `1:30`, and underscores.
     `09` is a string.
   - Floats include sexagesimal forms and `.inf`/`.nan`.
   - Nulls are `~`, `null`/`Null`/`NULL` and the empty scalar.
   - Timestamps become `date`/`datetime`.
   - `<<` is a merge key.
   - `=` resolves to `tag:yaml.org,2002:value`, which SafeLoader has **no constructor** for, so it is
     a load error.

   **Sources:** [PyYAML resolver.py](https://raw.githubusercontent.com/yaml/pyyaml/main/lib/yaml/resolver.py).
   **Support:** Direct (the regexes). The `=` load-error consequence is Interp. **Confidence:** high.
9. **Claim:** `serde-saphyr` 1.3.0 has YAML 1.1-style options (`legacy_octal_numbers`,
   `strict_booleans`, `duplicate_keys`, `merge_keys`). It is serde- and target-type-driven, though, and
   does not reproduce PyYAML's exact regexes.
   **Sources:** [serde-saphyr Options](https://docs.rs/serde-saphyr/latest/serde_saphyr/options/struct.Options.html).
   **Support:** Direct (options); "not PyYAML-exact" is Interp. **Confidence:** medium.
10. **Claim:** Avoid the serde_yaml family:
    - `serde_yaml` is deprecated/archived.
    - `serde_yml` is flagged unsound and unmaintained (RUSTSEC-2025-0068).
    - `serde_yaml_ng` still uses the unmaintained `unsafe-libyaml`.
    - `serde_norway` is a maintained fork built on `unsafe-libyaml-norway`.

    libyaml is a YAML 1.1 *parser*, but the serde layer resolves types its own way, so libyaml does not
    buy PyYAML type-resolution parity. **Sources:** [RUSTSEC-2025-0068](https://rustsec.org/advisories/RUSTSEC-2025-0068).
    **Support:** Direct for status; the rest is Interp. **Confidence:** medium-high.
11. **Inference (recommendation):** use `saphyr-parser` plus an owned composer (about 500–800 lines)
    that:
    - (a) applies PyYAML's resolver regexes to plain scalars only;
    - (b) implements SafeConstructor: merge keys (including lists of maps), timestamps → Date/DateTime,
      the `!!binary` and `!!set`/`!!omap` decisions, and an error for `=`;
    - (c) reports duplicate keys with line/column. Note that stock PyYAML `safe_load` silently keeps the
      **last** duplicate, so confirm whether Circuitry adds its own duplicate check before copying any
      behaviour.

    The remaining risk is **syntax**: documents that one parser accepts and the other rejects (tabs,
    the `%YAML 1.1` directive, `\/` escapes, NEL line breaks, some flow-context plain scalars). Error
    *messages* will never match PyYAML's.

### 2. CEL

12. **Claim:** `cel-interpreter` was renamed. The current crate is **`cel` 0.15.0** (MIT), published
    2026-10-06. Its repo has a conformance harness, but `conformance/src/bin/ignored.txt` lists about
    **820 skipped cel-spec cases**. Most involve protobuf, wrappers or extensions (`block_ext`,
    `bindings_ext`, `eq_wrapper`), plus some JSON-null equality cases.
    **Sources:** [cel-rust repo](https://github.com/cel-rust/cel-rust), crates.io API.
    **Support:** Direct (file contents); "mostly proto/ext" is Interp from the visible lines.
    **Confidence:** medium-high.
13. **Claim:** **`cel-core` 0.5.1** (MIT OR Apache-2.0, last release 2026-02-16) claims **100% of the
    cel-spec conformance suite** across parse, check and eval. Adoption is tiny: about 500 downloads,
    1 star, a single maintainer.
    **Sources:** [ponix-dev/cel-core](https://github.com/ponix-dev/cel-core),
    [crates.io](https://crates.io/crates/cel-core).
    **Support:** Direct (the claim is the project's own and unverified). **Confidence:** medium.
14. **Claim:** **cel-python is not spec-exact itself.** It implements `has()` as "evaluating the child
    raised no `CELEvalError`" (issue #73). Its runtime errors are Python exceptions whose `str()` lands
    in state. Version 0.5 removed the RE2→`re` fallback, so `matches()` is RE2 only.
    **Sources:** [cel-python #73](https://github.com/cloud-custodian/cel-python/issues/73),
    [changelog](https://data.safetycli.com/packages/pypi/cel-python/changelog),
    [README](https://github.com/cloud-custodian/cel-python/blob/main/README.rst).
    **Support:** Direct. **Confidence:** medium-high.
15. **Inference:** "spec-conformant" and "same as cel-python" are different targets. Spec conformance
    could even be *worse* for parity: heterogeneous numeric equality (`1 == 1.0` is true per the current
    spec) and error-vs-false on missing fields may differ from cel-python 0.5.

    Recommendation: start with `cel` (larger user base, active, Value conversion via serde). Wrap
    evaluation in Circuitry's own non-strict/strict layer. Build a **differential corpus**: extract every
    `mode: cel` expression from production orchestration documents, evaluate each in cel-python
    against recorded states, and diff. Keep `cel-core` as the fallback if `cel` gaps show up.

    Areas to probe:
    - int/uint/double mixing, and JSON numbers arriving as double vs int (Python int stays int);
    - `size()` on strings (code points);
    - `has()` on maps vs missing keys;
    - `null` comparisons;
    - `in` on lists and maps;
    - string functions (`contains`, `startsWith`, `matches`);
    - overflow errors;
    - timestamps.

### 3. Mustache

16. **Claim:** chevron (unmaintained; latest 0.14.0) deviates from the spec and from other renderers
    in several ways:
    - **Escaping:** HTML-escapes only `& " < >` and **not `'`**.
    - **Value rendering:** values go through Python `str()`, so `True` → `True`, a dict renders as
      `{'a': 1}`, a float renders as `repr`.
    - **Falsy values:** render as `''`, except `0` and `False`, which are kept and render `0` and
      `False`. Because `0.0 == 0`, `0.0` renders as `0.0`.
    - **Dotted names:** a name that fails part-way through **falls back to outer scopes** (the spec
      says it should not), and `items.0` indexes lists.
    - **Missing keys:** render `''`.
    - **Sections:** a string is pushed as a scope, not iterated. Inverted sections push `not value`.
    - **Partials:** loaded from `./*.mustache`, with indentation padding.
    - **Lambdas:** callables are supported.

    **Sources:** [chevron renderer.py](https://raw.githubusercontent.com/noahmorrison/chevron/main/chevron/renderer.py).
    **Support:** Direct (code). **Confidence:** high.
17. **Claim:** The Rust Mustache crates fall short:
    - `mustache` 0.9.0 (last release 2018) is abandoned.
    - `ramhorns` 1.0.1 (2024) is a derive-macro-oriented "Mustache-like" engine.
    - `mustache2` 1.0.7 (2026, 189 downloads, Codeberg) claims full spec compliance.

    None of them targets chevron's quirks, and spec compliance is the wrong target here.
    **Sources:** crates.io API; [mustache2 docs](https://docs.rs/crate/mustache2/latest);
    [ramhorns](https://github.com/maciejhirsz/ramhorns).
    **Support:** Direct for status; "wrong target" is Inference. **Confidence:** high.
18. **Inference (recommendation):** **write our own renderer as a line-by-line port of chevron's
    `tokenizer.py` + `renderer.py`** (together under 600 lines of Python) over the electricity `Value`.
    Test it against the mustache/spec YAML suite *as chevron passes it*, plus a chevron differential
    corpus. Decide with the owner whether file partials from the CWD are kept, which is a parity vs
    security trade-off. This is clearly the safer route.

### 4. JSON: byte-identical to `json.dumps`

19. **Claim:** `serde_json` 1.0.151 needs a custom `ser::Formatter` and other changes for
    `json.dumps` parity:
    - **Separators:** Python uses `", "`/`": "` when `indent=None`, and `","` + newline when indented.
    - **`ensure_ascii=True` by default:** every non-ASCII character becomes lowercase `\uXXXX`, and
      non-BMP characters become surrogate pairs. serde_json writes raw UTF-8.
    - **Non-finite floats:** Python writes `NaN`/`Infinity`/`-Infinity`; serde_json writes `null`.
    - **Floats:** serde_json formats with ryu, which differs from Python `repr` (see 20).
    - **Big ints:** Python ints are unbounded; serde_json needs `arbitrary_precision`.

    Community `PythonFormatter` crates exist (`serde-json-python-formatter`, `serde_json_pythonic`), but
    they are small and unvetted. **Sources:** [Formatter trait](https://docs.rs/serde_json/latest/serde_json/ser/trait.Formatter.html),
    [serde-json-python-formatter](https://docs.rs/serde-json-python-formatter/latest/serde_json_python_formatter/struct.PythonFormatter.html),
    [laya-core json_compat notes](https://docs.rs/crate/laya-core/latest/source/src/json_compat.rs).
    **Support:** Direct. **Confidence:** high.
20. **Claim:** Floats differ between ryu and Python `repr`. Python switches to scientific notation
    when the decimal exponent is < -4 or ≥ 16, and always writes a sign plus at least two exponent
    digits: `1e-05`, `1e+16`. ryu writes `1e-5`, and writes `1e16` as `10000000000000000.0`. Both give
    `1.0` for whole floats.
    **Support:** Interp/researcher knowledge of CPython's `float_repr_style='short'`; the `1e-5` vs
    `1e-05` difference is confirmed by the search result above. **Confidence:** medium-high.
    Verify against CPython in the conformance tests. **Recommendation:** take the shortest round-trip
    digits from Rust's `{:e}` (or `ryu`) and lay them out with Python's rule.
21. **Claim:** With the `preserve_order` feature, `serde_json::Map::remove` is **`swap_remove`**, which
    reorders keys. Python `dict` deletion keeps the order of the remaining keys, so use `shift_remove`.
    **Sources:** [Map docs](https://docs.rs/serde_json/latest/serde_json/map/struct.Map.html),
    [serde-rs/json#807](https://github.com/serde-rs/json/issues/807).
    **Support:** Direct. **Confidence:** high.
22. **Inference:** write a dedicated `py_json::dumps(value, indent, ensure_ascii, sort_keys)` over the
    electricity `Value`. It must convert non-string keys the way Python does (`True` → `"true"`,
    `None` → `"null"`, `1` → `"1"`, float keys via repr). Golden-test it against CPython. Still confirm
    the exact `json.dumps` arguments that `cof run --out` uses; I did not locate that call.

### 5. JSON Schema

23. **Claim:** `jsonschema` 0.58.6 (MIT; very active, released 2026-10-06; about 98M downloads)
    supports Drafts 4, 6, 7, 2019-09 and 2020-12. It is tracked on Bowtie and has structured output.
    By default it **resolves remote `$ref`s over HTTP and from files**; disable this with
    `default-features=false` or `.offline()`. `pattern` uses `fancy-regex` by default.
    Format validation is draft-dependent. Python `jsonschema` checks formats only when a
    `format_checker` is passed.
    **Sources:** [docs.rs jsonschema](https://docs.rs/jsonschema/latest/jsonschema/index.html),
    [crates.io](https://crates.io/crates/jsonschema),
    [python-jsonschema validate](https://python-jsonschema.readthedocs.io/en/stable/validate/).
    **Support:** Direct. **Confidence:** high.
24. **Claim:** The error messages differ. Python writes `'name' is a required property` (Python repr,
    single quotes); Rust writes `"name" is a required property` (JSON, double quotes). Other keywords
    differ similarly, and so does error iteration order.
    **Sources:** [python _keywords.py](https://github.com/python-jsonschema/jsonschema/blob/main/jsonschema/_keywords.py),
    [rust error.rs](https://docs.rs/jsonschema/latest/src/jsonschema/error.rs.html).
    **Support:** Direct. **Confidence:** high.
    **Inference:** validity verdicts will match. If messages reach state, map them from
    `ValidationErrorKind` into Python-format text for the subset of keywords the documents use. Also pin
    the default draft: Python `validate()` with no `$schema` uses the latest draft (2020-12) — please
    re-verify this.

### 6. Regex

25. **Claim:** The candidates:
    - `regex` 1.13.1: RE2-like, linear time, no lookaround or backreferences.
    - `fancy-regex` 0.19.2: lookaround, backreferences, Python-style `(?P<name>)`/`(?P=name)`, atomic
      groups. It **also allows some variable-length lookbehind**, which Python rejects.
    - `pcre2` 0.2.11: a binding to the C library, with PCRE syntax rather than Python's.

    None is identical to Python `re`. **Sources:** [fancy-regex](https://docs.rs/fancy-regex/latest/fancy_regex/),
    [pcre2](https://docs.rs/pcre2/latest/pcre2/), [Python re](https://docs.python.org/3/library/re.html).
    **Support:** Direct. **Confidence:** high.
26. **Inference:** use **two engines**:
    - CEL `matches()` → the `regex` crate. This is a good parity fit, because cel-python 0.5 uses RE2.
    - The `regex` tool and JSON Schema `pattern` → `fancy-regex`, behind a validator that rejects
      constructs Python would reject.

    Known Python-vs-Rust gaps to test:
    - `$` matches before a trailing `\n` in Python;
    - `\Z` (Python) vs `\z` (Rust);
    - Unicode `\w`/`\d`/`\b` definitions;
    - `re.match` (anchored) vs `re.search`;
    - case-folding edge cases;
    - `re.sub` replacement syntax (`\1`, `\g<name>`) vs `$1`.

    Python `re.ASCII` and `re.VERBOSE` need flag mapping. Check the owner's documents for actual pattern
    use (likely simple).

### 7. MCP client

27. **Claim:** The official **`rmcp` is 3.5.1** (Apache-2.0, published 2026-10-05). The 3.x line
    targets **MCP 2026-07-28**, which is stateless: no initialize handshake, no `Mcp-Session-Id`, no GET
    stream or resumption. It keeps a `legacy_session_mode` for older servers, and its transport features
    cover child-process stdio and the streamable-HTTP client (reqwest). The Python `mcp` 2.x that
    Circuitry pins also targets 2026-07-28 and earlier revisions, so both runtimes negotiate the same
    protocol generation.
    **Sources:** [rmcp 3.0 migration](https://github.com/modelcontextprotocol/rust-sdk/discussions/969),
    [rmcp v3.0.0 notes](https://newreleases.io/project/github/modelcontextprotocol/rust-sdk/release/rmcp-v3.0.0),
    [MCP 2026-07-28 blog](https://blog.modelcontextprotocol.io/posts/2026-07-28/),
    [Streamable HTTP spec](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http),
    [python-sdk v2.0.0](https://github.com/modelcontextprotocol/python-sdk/releases/tag/v2.0.0).
    **Support:** Direct. **Confidence:** high.
    **Parity risk:** version negotiation and fallback against older servers (2025-11-25 / 2025-03-26),
    and legacy HTTP+SSE (2024-11-05) support, which I did not verify in rmcp 3.x. Tool results also need
    the same content-block → state mapping as Circuitry. Alternative: `rust-mcp-sdk` (community).

### 8. Persistence

28. **Claim:** `surrealdb` 3.3.0 (SurrealDB 3.x; 3.0 released 2026-02-17) supports both embedded
    (`kv-mem`, `kv-rocksdb`, …) and remote (`protocol-ws`, `protocol-http`) connections. **The crate's
    licence is "non-standard": BSL 1.1**, with an Additional Use Grant forbidding use "as a Database
    Service", a Change Date of 2030-01-01 and Apache-2.0 after that. SurrealDB's FAQ says SDKs are
    Apache/MIT, but the published Rust crate ships the BSL text.
    **Sources:** crates.io API (license field);
    [docs.rs LICENSE](https://docs.rs/crate/surrealdb/latest/source/LICENSE);
    [SurrealDB licence FAQ](https://surrealdb.com/license);
    [3.0 release](https://surrealdb.com/releases/3.0).
    **Support:** Direct. **Confidence:** high.
    **Inference:** for an MIT binary, prefer a thin **remote-only client** speaking SurrealDB's
    HTTP `/sql` / RPC API over reqwest or tokio-tungstenite. Circuitry uses SurrealDB as a remote server
    anyway. This also keeps the binary small. Otherwise get the owner's sign-off on BSL. Parity risk:
    SurrealQL value encoding (record IDs, datetimes, decimals) in the stored runs/effects rows.
29. **Claim:** `sqlx` 0.9.0 (MIT/Apache-2.0, 2026-05) handles async SQLite, Postgres and MySQL.
    SQLite can be bundled for static builds. Circuitry's SQL plugins also cover cockroach, duckdb, mssql
    and clickhouse, which sqlx does not.
    **Sources:** crates.io API; Circuitry's `pyproject.toml`. **Support:** Direct. **Confidence:** high.
    **Parity risk:** the runs/effects schema, type affinity, and timestamp/JSON column text formats.

### 9. Observability

30. **Claim:** `opentelemetry` / `opentelemetry_sdk` / `opentelemetry-otlp` 0.33.0 (2026-09-18) and
    `tracing-opentelemetry` 0.34.0 are current. In 0.33, the Logs and Metrics API/SDK are **stable**,
    the OTLP Logs/Metrics exporters are RC (stable expected in 0.33.1), and **Distributed Tracing is
    still pre-stable (beta)**.
    **Sources:** [0.33.0 release notes](https://newreleases.io/project/github/open-telemetry/opentelemetry-rust/release/opentelemetry-0.33.0),
    [release_0.32.md](https://github.com/open-telemetry/opentelemetry-rust/blob/main/docs/release_0.32.md).
    **Support:** Direct. **Confidence:** high.
    **Inference:** expect minor breaking changes in the trace API, so pin exact versions. Use OTLP
    http/protobuf to match Circuitry's exporter. Parity concerns span names and attributes, not bytes.

### 10. Runtime plumbing

31. **Claim:** The crates:
    - `tokio` 1.53.2: `tokio::signal` for SIGINT/SIGTERM; `CancellationToken` is in `tokio-util`.
    - `process-wrap` 10.0.1 (watchexec; successor to `command-group`): composable tokio wrappers for
      `ProcessGroup::leader()`, `ProcessSession` and `KillOnDrop`, reaping the whole group. Implement
      timeouts as `tokio::time::timeout` → SIGTERM to the group → grace period → SIGKILL.
    - `reqwest` 0.13.5: **rustls by default** with **aws-lc-rs** as the crypto provider, and
      `rustls-platform-verifier` for roots.
    - `lettre` 0.11.23 for SMTP.

    **Sources:** [process-wrap](https://docs.rs/process_wrap/latest/process_wrap/),
    [reqwest 0.13 notes](https://github.com/seanmonstar/reqwest/releases/tag/v0.13.0),
    [reqwest blog](https://seanmonstar.com/blog/reqwest-v013-rustls-default/), crates.io API.
    **Support:** Direct. **Confidence:** high.
    **Inference/risks:**
    - aws-lc-rs needs cmake and a C toolchain when cross-building musl. Switch to `ring` with
      `rustls-no-provider` if that is painful.
    - The platform verifier on a scratch/distroless image needs a CA bundle, so ship
      `webpki-roots` or `ca-certificates`.
    - Exit codes 130/143 must come from the signal handler, not from default termination.

### 11. Distribution

32. **Claim:** `cargo-dist` ("dist") is maintained. 0.33.0 was released 2026-09-10 on GitHub (crates.io
    shows 0.32.0). It generates GitHub Actions release workflows, archives, installers and checksums.
    musl static builds work with `cargo-zigbuild` or musl cross images. **musl's default allocator is
    slow under multithreaded load**: swap in `mimalloc` for musl targets.
    **Sources:** [cargo-dist releases](https://github.com/axodotdev/cargo-dist/releases),
    [nickb.dev on the musl allocator](https://nickb.dev/blog/default-musl-allocator-considered-harmful-to-performance/),
    [tweag mimalloc](https://www.tweag.io/blog/2023-08-10-rust-static-link-with-mimalloc/),
    [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild/blob/main/README.md).
    **Support:** Direct. **Confidence:** medium-high.
    **Inference:** macOS arm64 cannot be fully static (libSystem), so it is a normal dynamic binary.
    Build the container image `FROM scratch` or distroless/static with CA certificates, using a multi-arch
    `docker buildx` step. dist does not build container images. Publish to crates.io from CI with
    trusted publishing.

### 12. Plugin protocol

33. **Claim:** The precedents:
    - **LSP** frames JSON-RPC with `Content-Length` headers (`lsp-server`, the rust-analyzer crate).
    - **MCP stdio** uses **newline-delimited JSON-RPC**, and mismatched framing is a real interop bug
      source.
    - **WASI 0.3** (2026-06-11) made async native to the Component Model. wasmtime is the reference
      runtime, and `componentize-py` can build Python components (last push 2026-03).

    **Sources:** [LSP 3.17 spec](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/),
    [python-sdk#2546 framing bug](https://github.com/modelcontextprotocol/python-sdk/issues/2546),
    [WASI 0.3](https://bytecodealliance.org/articles/WASI-0.3),
    [componentize-py](https://github.com/bytecodealliance/componentize-py).
    **Support:** Direct. **Confidence:** high.
34. **Inference (recommendation):** use **NDJSON JSON-RPC 2.0 over stdio**, MCP-style:
    - **Protocol:** one message per line; stdout reserved for protocol and stderr for logs. Messages are
      a `describe` handshake (plugin name, version, tool schemas), then `invoke`, an optional
      `progress` notification, and `cancel`.
    - **Implementation:** about 200 lines over tokio lines codecs, rather than jsonrpsee (which is
      HTTP/WS oriented).
    - **Python plugin safety:**
      - spawn `python3 -m circuitry_plugin_host <plugin>` with an explicit argv (no shell);
      - give it a scrubbed env (allowlisted variables only, no API keys unless declared);
      - set the working directory explicitly;
      - put it in its own process group, killed on cancel or timeout;
      - set rlimits through `pre_exec` (CPU, address space, number of files);
      - cap message size;
      - run one process per plugin and reuse it across calls.

    python_eval relies on RestrictedPython, which is *not* a sandbox. The OS boundary is the real
    boundary. WASM components are the stronger sandbox, but CPython-in-WASM is heavy, misses many C
    extensions and adds wasmtime (large) to the binary. Keep it as a v2 option, not v1.

### 13. VM design for resumable orchestration

35. **Claim:** The durable-execution engines teach the same lesson:
    - **Temporal** resumes by **replaying** deterministic workflow code against a durable Event History
      and requires that replay produce the same command sequence.
    - **Restate** journals every side effect (`ctx.run`) and replays the journal after failures.

    Neither snapshots a VM stack. **Sources:** [Temporal workflow execution](https://docs.temporal.io/workflow-execution),
    [Temporal event history](https://docs.temporal.io/encyclopedia/event-history),
    [Restate request lifecycle](https://docs.restate.dev/guides/request-lifecycle),
    [Restate invocation protocol](https://github.com/restatedev/sdk-shared-core/blob/main/docs/service-invocation-protocol.md).
    **Support:** Direct. **Confidence:** high.
36. **Claim (local):** Circuitry's `--resume` is **state-based**. Nodes carry
    `meta.created_at/completed_at/error`, and a resume treats a node with no completion or with a stale
    error as unfinished (#270 F4). That is replay-by-state, not a snapshot.
    **Sources:** `conditional.py` lines 140–160. **Support:** Direct. **Confidence:** high.
37. **Inference (recommendation):** effects cost milliseconds to minutes (LLM calls, ffmpeg), so
    dispatch speed is irrelevant. Optimise for parity, cancellation and resume. Use a **structured IR
    ("block bytecode")**:
    - **Control flow:** compile each effect to an op with a **stable effect path ID** (document path),
      and model control flow as WASM-style nested regions (`block`, `loop`, `if`, `try/finally`,
      `parallel`) instead of arbitrary jumps.
    - **Execution:** interpret with an explicit frame stack, so that `parallel` spawns child fibers
      (tokio tasks) each with their own frame.
    - **Cancellation:** a `CancellationToken` tree that unwinds through `finally` regions, in the spirit
      of CPython 3.11's exception tables and Lua's protected calls.
    - **Resume:** re-run from the top and skip effects whose state says completed, exactly as cof does.
      This matches cof semantics and avoids serialising VM frames.

    Treat a flat register/stack bytecode as unnecessary. A pure tree-walker over the typed IR would be
    equally valid, and the 2026 JOT study shows the AST-vs-bytecode trade-offs are small. "Bytecode" can
    be the serialized form of this IR. Dagger is a content-addressed DAG cache, which is less relevant
    now that caching was removed.
    **Sources (context):** [AST, Bytecode, and the Space In Between (JOT 2026)](https://www.jot.fm/issues/issue_2026_01/a15.pdf),
    [Marr: AST vs Bytecode](https://kar.kent.ac.uk/102817/7/AST%20vs%20Bytecode_Marr_PUB.pdf).
    **Confidence:** medium (design judgement).

## Contradictions

- **SurrealDB licence:** the SurrealDB FAQ and the license repo say "SDKs are Apache 2.0 or MIT", but
  the `surrealdb` 3.3.0 crate's licence field is "non-standard" and its LICENSE file is BSL 1.1. Treat
  the crate as BSL until SurrealDB says otherwise.
- **CEL conformance claims:** a search summary called `cel` "actively maintained with a conformance
  harness, no pass-rate published". The repo's ignored list (about 820 cases) shows the gaps are real.
  `cel-core` claims 100% but has almost no adoption. Neither number speaks to *cel-python* parity.
- **mustache2 version:** the docs.rs page shows 1.0.7 while its README example uses `"1.4.3"`;
  crates.io says 1.0.7.
- **rmcp in search summaries:** one summary cited `rmcp = "0.5"` from a fork; the crates.io API says
  3.5.1. Use crates.io.

## Missing evidence

- The exact `json.dumps` arguments `cof run --out` uses (indent, ensure_ascii, sort_keys, default=)
  and whether `datetime`/`date` from YAML timestamps can reach state. I could not list directories in
  the reference checkout.
- Whether Circuitry adds duplicate-key detection on top of PyYAML (stock `safe_load` does not), and
  where it does.
- cel-python 0.5 behaviour on heterogeneous numeric equality, `has()` on missing map keys, and `null`
  handling. This needs the differential corpus; it is not documented.
- Whether rmcp 3.x still speaks legacy HTTP+SSE (2024-11-05) as a client.
- The Python float-repr layout rule (item 20) is stated from CPython knowledge, not from a fetched
  source. Confirm it with golden tests.
- Python `jsonschema`'s default draft when `$schema` is absent (believed to be 2020-12; not re-fetched).
- An owner decision on error-string parity in `meta.error` (item 2). This drives effort in CEL, schema
  and YAML errors.

## Sources

- Kept: PyYAML resolver.py (https://raw.githubusercontent.com/yaml/pyyaml/main/lib/yaml/resolver.py) — the exact YAML 1.1 rules to port.
- Kept: chevron renderer.py (https://raw.githubusercontent.com/noahmorrison/chevron/main/chevron/renderer.py) — the behaviour to port.
- Kept: saphyr repo and parser source (https://github.com/saphyr-rs/saphyr) — event API with style, tag and spans.
- Kept: cel-rust repo (https://github.com/cel-rust/cel-rust) and its ignored.txt — real conformance gaps.
- Kept: cel-core (https://github.com/ponix-dev/cel-core) — fallback CEL crate with a 100% claim.
- Kept: cel-python issue #73 and README (https://github.com/cloud-custodian/cel-python) — the reference's own deviations.
- Kept: serde_json Map and Formatter docs (https://docs.rs/serde_json/latest/serde_json/) — swap_remove and formatter hook.
- Kept: jsonschema docs.rs (https://docs.rs/jsonschema/latest/jsonschema/) — drafts, default ref resolution, regex engine.
- Kept: rmcp 3.0 migration and MCP 2026-07-28 spec/blog — protocol alignment with Python mcp 2.x.
- Kept: surrealdb crate LICENSE on docs.rs — licence risk.
- Kept: opentelemetry-rust 0.33 notes; reqwest 0.13 notes; process-wrap docs; cargo-dist releases; musl allocator posts.
- Kept: Temporal and Restate docs — replay vs snapshot.
- Kept (Circuitry source): `pyproject.toml`, `core/templates.py`, `core/conditional.py`.
- Rejected/deprioritized: reddit threads (unverifiable); `serde_yml` (unsound); search-engine summaries quoting old crate versions; SkillFed/checklist.day aggregators; arXiv hits (irrelevant).

## Next steps

(Written before the design. `DESIGN.md` §1 records the decisions these steps led to.)

1. Pull every CEL expression, Mustache template and regex out of production orchestration documents.
   Generate differential corpora from cel-python, chevron and Python `re`. This decides the CEL crate and
   the regex scope.
2. Read cof's `--out` writer and its YAML loader in Circuitry's source to pin the exact `json.dumps` and
   duplicate-key behaviour.
3. Ask the owner two things: (a) must `meta.error` strings be byte-identical; (b) is a BSL-licensed
   `surrealdb` dependency acceptable, or should electricity use a remote-only HTTP/WS client.

## Summary table

| Area | Recommended crate(s) (version, licence) | Confidence | Top parity risk |
|---|---|---|---|
| YAML | `saphyr-parser` 0.1.0 (MIT/Apache) + own PyYAML 1.1 resolver/constructor | high | 1.1 scalar resolution (`yes`, `1e3` is a string, octal, sexagesimal, timestamps, `<<`, `=`) and parser accept/reject differences |
| CEL | `cel` 0.15.0 (MIT); fallback `cel-core` 0.5.1 (MIT/Apache) | medium | cel-python's non-spec `has()`, numeric equality, error-vs-false, and exception text in `meta.error` |
| Mustache | Own port of chevron (no crate) | high | chevron quirks: `str()` rendering (`True`, `{'a': 1}`), escaping without `'`, dotted-name fallback, CWD partials |
| JSON | `serde_json` 1.0.151 (`preserve_order`, `arbitrary_precision`) + own Python-compatible writer | high | float repr (`1e-05`, `1e+16`), `ensure_ascii` surrogates, separators, `NaN`, non-string keys, swap_remove |
| JSON Schema | `jsonschema` 0.58.6 (MIT), offline | high (validity) / low (messages) | error message wording and order; format-check defaults; remote `$ref` fetching on by default |
| Regex | `regex` 1.13.1 (for CEL) + `fancy-regex` 0.19.2 (for tools and schemas) | medium | Python `re` semantics (`$` before `\n`, `\Z`, Unicode classes, `re.sub` templates, variable lookbehind accepted) |
| MCP client | `rmcp` 3.5.1 (Apache-2.0) | high | negotiation with older servers or legacy SSE; tool-result → state mapping |
| Persistence | `sqlx` 0.9.0 (MIT/Apache); SurrealDB via own HTTP/WS client (the `surrealdb` 3.3.0 crate is BSL 1.1) | medium-high | row schema and value encoding vs the Python plugins; BSL licence |
| Observability | `opentelemetry`/`_sdk`/`-otlp` 0.33.0, `tracing-opentelemetry` 0.34.0 (Apache-2.0) | high | trace API still beta, so pin versions; span name and attribute parity |
| Runtime | `tokio` 1.53.2, `tokio-util`, `process-wrap` 10.0.1, `reqwest` 0.13.5 (rustls/aws-lc), `lettre` 0.11.23 | high | group kill/timeout semantics and exit codes 130/143 must match cof |
| Distribution | `cargo-dist` 0.33 + `cargo-zigbuild`/musl + `mimalloc`; scratch/distroless image | medium-high | aws-lc-rs musl cross builds; CA roots in the container; macOS cannot be fully static |
| Plugin protocol | Own NDJSON JSON-RPC 2.0 over stdio (MCP-style); WASM component model later | medium-high | framing and stdout pollution by Python plugins; same results as in-process cof plugins |
| VM | Own structured-IR interpreter (WASM-like regions, stable effect IDs, state-based resume) | medium | resume must skip/re-run exactly what cof does (state-based, #270 F4) |
