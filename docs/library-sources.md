# Library Sources

`cof list`, `cof info`, `cof run`, and `cof eject` look up orchestrations
through a **library registry** — an ordered list of *sources*. The bundled
curation library is one source; a folder of your own `*.yml` files is another.

## Configuration reference — `runtime.library.sources`

```json
{
  "runtime": {
    "library": {
      "sources": [
        {"type": "curation"},
        {"type": "folder", "name": "local", "path": "./orchestrations"}
      ]
    }
  }
}
```

`sources` is a **list of objects, and order is precedence** — when a bare name
matches in more than one source, the earliest one wins.

When `runtime.library.sources` is absent, the registry defaults to
`[{"type": "curation"}]`. Zero-config behaviour is therefore exactly what it
was before sources existed: curation-only, with no source column and no
`source` key in `--json` output.

### Source types

| Field  | Applies to | Required | Description |
| ------ | ---------- | -------- | ----------- |
| `type` | all        | yes      | `curation` or `folder`. |
| `name` | all        | no       | Display name and qualifier prefix. Defaults to `curation` for curation sources and the directory's basename for folder sources. |
| `path` | `folder`   | yes      | Directory to scan. `~` is expanded; relative paths resolve against the process working directory. |

A malformed `sources` list (unknown `type`, missing `path`, empty list, a
non-object entry) is a hard error reported by the command, not silently ignored.

#### `curation`

Wraps the bundled curation library at `src/circuitry/curation/`, including its
`manifest.json` and the slash-delimited filesystem fallback. Resolution is
unchanged from the pre-registry CLI.

#### `folder`

Scans a directory recursively for `*.yml` and `*.yaml`. Entry names are the
path relative to the folder root, without the suffix — `nested/deep.yml`
becomes `nested/deep`, and its category is `nested`.

Metadata comes from an optional `manifest.json` at the folder root, which uses
the same shape as the curation manifest:

```json
{
  "entries": [
    {
      "name": "my_pipeline",
      "file": "my_pipeline.yml",
      "category": "local",
      "description": "Summarise a document.",
      "backends": ["llm"],
      "inputs": {"document": {"type": "string", "required": true}}
    }
  ]
}
```

Files not covered by the manifest — or every file, when there is no manifest —
get metadata derived from the YAML itself:

- **description** — the first non-empty line of the file's leading `#` comment
  block; failing that, `interface.description`; failing that, the first line of
  the first effect's `template`.
- **inputs** — `interface.inputs`, verbatim.
- **category** — the entry's directory relative to the folder root (empty for
  top-level files).
- **backends** — empty, unless the manifest supplies them.

## Referring to entries

Bare names search sources in precedence order:

```sh
cof run my_pipeline
```

If a bare name matches in more than one source, the first source wins and the
command prints a warning naming the other matches. **Source-qualified** names —
`"<source>:<name>"` — skip precedence entirely and never warn:

```sh
cof run local:my_pipeline
cof info local:my_pipeline
cof eject local:my_pipeline --out ./my_pipeline.yml
```

A colon only acts as a qualifier when the prefix names a configured source, so
values like `C:/tmp/orch.yml` are never mistaken for a qualified reference.
A literal path that exists on disk still wins over any library lookup.

## Listing

```sh
cof list                    # every source, in precedence order
cof list --source local     # one source only
cof list --json             # machine-readable
```

`cof list` grows a **Source** column — and `--json` a `source` key — only when
more than one source is configured, which is what keeps single-source output
byte-identical.

## Relationship to `runtime.library` shared-library retrieval

`runtime.library.sources` is independent of the `runtime.library.backend` /
`local_root` settings used by [shared library retrieval](./shared-library.md)
(`cof fetch` / `cof run-library`). The two features share the `runtime.library`
namespace but no keys; configuring one does not affect the other.
