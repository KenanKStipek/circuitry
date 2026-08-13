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
        {"type": "folder", "name": "local", "path": "./orchestrations"},
        {
          "type": "github",
          "name": "hub",
          "repo": "owner/name",
          "ref": "main",
          "path": "library/",
          "token_env": "GITHUB_TOKEN"
        }
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

| Field       | Applies to          | Required | Description |
| ----------- | ------------------- | -------- | ----------- |
| `type`      | all                 | yes      | `curation`, `folder`, or `github`. |
| `name`      | all                 | no       | Display name and qualifier prefix. Defaults to `curation` for curation sources, the directory's basename for folder sources, and the repository name for GitHub sources. |
| `path`      | `folder`            | yes      | Directory to scan. `~` is expanded; relative paths resolve against the process working directory. |
| `repo`      | `github`            | yes      | `owner/name`. |
| `ref`       | `github`            | no       | Branch, tag, or commit SHA. Defaults to `main`. |
| `path`      | `github`            | no       | Subtree to fetch. Defaults to the repository root. |
| `token_env` | `github`            | no       | **Name** of the environment variable holding a GitHub token. The token value itself is never read from — nor written to — config or state. |
| `cache_dir` | `github`            | no       | Override the cache root (default `~/.cache/circuitry/library`). |
| `api_base`  | `github`            | no       | Override the API base URL (GitHub Enterprise). Defaults to `https://api.github.com`. |

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

#### `github`

A subtree of a GitHub repository, served from a **SHA-pinned local cache**.
This is the consumption half of the publish-by-PR workflow described in
[shared library contributions](./shared-library-contributions.md): people add
orchestrations to a library repository by pull request, and consumers point a
`github` source at it.

```json
{
  "type": "github",
  "repo": "owner/name",
  "ref": "main",
  "path": "library/",
  "name": "hub",
  "token_env": "GITHUB_TOKEN"
}
```

**Only `cof library refresh` touches the network.** `cof list`, `cof info`,
`cof run`, and `cof eject` read the cache and nothing else — there is no
implicit fetch, so a run can never stall on a network call or silently pick up
a different version of an orchestration than the one you listed.

Refresh resolves `ref` → commit SHA and downloads the subtree's `*.yml` /
`*.yaml` files (plus an optional `manifest.json`) through the REST contents API
into:

```
~/.cache/circuitry/library/<source>/<sha>/…
~/.cache/circuitry/library/<source>/index.json   # {"sha": …, "fetched_at": …}
```

`XDG_CACHE_HOME` is honoured, and `CIRCUITRY_CACHE_DIR` (or the source's
`cache_dir`) overrides the root outright. The cached subtree is read exactly
like a [folder source](#folder) — same manifest handling, same metadata
derivation, same name matching — so `library/nested/deep.yml` is the entry
`nested/deep`.

Refresh re-downloads **only when the SHA moved**; when it matches the cached
one the command reports `unchanged` and makes no content requests. A successful
fetch is staged beside the live tree and swapped in when the index is rewritten,
and superseded SHA directories are pruned.

Until the first refresh the source is simply empty, and commands print:

```
Warning: Library source 'hub' (owner/name@main) has not been fetched yet — run `cof library refresh hub`.
```

##### Authentication

Public repositories work with no token at all. For private repositories — or to
raise the API rate limit — set `token_env` to the **name** of an environment
variable and export the token there:

```sh
export GITHUB_TOKEN=ghp_…
cof library refresh hub
```

When that variable is set (and non-empty), requests carry
`Authorization: Bearer $GITHUB_TOKEN`. The config file, the cache index, and
every serialised state file record only the variable *name*, never the secret.

Failures are reported with the fix attached: a 401 names the variable that was
rejected, a 404 reminds you that a private repository looks identical to a
missing one when unauthenticated, and an exhausted rate limit reports the reset
time plus how to authenticate.

## Refreshing

```sh
cof library refresh          # every configured source
cof library refresh hub      # one source
cof library refresh --json   # machine-readable results
```

Each source reports `updated` (with the new SHA), `unchanged`, or `skipped`
(local sources have nothing to fetch). The command exits non-zero if any source
failed, after attempting all of them.

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
