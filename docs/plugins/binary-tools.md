# Binary tool plugins: `binary` and `env`

Every tool plugin that wraps a command-line binary — the 18
`GenericSubprocessTool`-based plugins (`awk`, `docker`, `exiftool`, `gh`,
`git`, `imagemagick`, `kubectl`, `linter`, `mediainfo`, `ocr`, `pandoc`,
`ping`, `pytest`, `ripgrep`, `sed`, `7z`, `traceroute`, `yt_dlp`) plus
`ffmpeg` — reads two optional settings from
`runtime.plugins.<name>` in your config file:

- `binary` — the executable to run, an absolute path (`~` is expanded).
  When set, it replaces the `PATH` search entirely.
- `env` — environment variables for the tool's process, merged over the
  inherited environment (it adds to and overrides, it never replaces).

Both belong in config: they are machine-specific (a locally built binary,
a thread-limit tuned for one box) and orchestration YAML stays portable
across machines. Neither can be set via `params` on the tool effect.

```json
{
  "runtime": {
    "plugins": {
      "imagemagick": {
        "binary": "~/opt/imagemagick-omp/bin/magick",
        "env": {"MAGICK_THREAD_LIMIT": "4", "OMP_WAIT_POLICY": "passive"}
      }
    }
  }
}
```

## A secret in `env`

An `env` value can be a secret instead of a literal, `{secret: <name>}`
(optionally with `prefix:`, literal text put before it), resolved from
`runtime.secrets.<name>` when the tool runs, never earlier:

```json
{
  "runtime": {
    "secrets": {"gh": {"command": ["gh", "auth", "token"]}},
    "plugins": {
      "git": {"env": {"GH_TOKEN": {"secret": "gh"}}}
    }
  }
}
```

The process gets the value; anything it prints that contains it, on
`stdout`, `stderr` or in an error, is saved with the value replaced by
`[secret:gh]`, and the value never reaches saved state, events or logs.
See [Secrets](../orchestration-reference.md#secrets-in-http-headers-and-a-subprocess-tools-env)
for the three kinds of secret and the trust rule.

## Unset — behaviour is unchanged

Leaving `binary` unset means the same `PATH` search as always: the first
of a plugin's binary candidates found on `PATH` runs (`imagemagick` tries
`magick` then falls back to `convert`; `sed` tries `sed` then `gsed`;
`ffmpeg` looks for `ffmpeg`). Leaving `env` unset means the child process
inherits circuitry's own environment exactly as before — no new
variables are introduced.

## A configured `binary` that doesn't work fails clearly

If `binary` is a relative path, or points at a path that doesn't exist or
isn't executable, the tool effect fails with a message naming both the
setting and the path, e.g.:

```
imagemagick: configured runtime.plugins.imagemagick.binary='/opt/imagemagick-omp/bin/magick'
does not exist or is not executable (resolved: /opt/imagemagick-omp/bin/magick).
```

`cof check` / `cof doctor` (preflight) report the same condition ahead of
a run, the same way they report a missing `PATH` binary today.

## Run record

Each tool node's `meta.binary` records the resolved absolute path of the
executable that actually ran — the configured `binary` (after `~`
expansion) when set, otherwise whichever `PATH` candidate resolved. This
is what makes a run record self-documenting: which build of a binary
produced this result.

## Example: a faster local ImageMagick build

Homebrew's ImageMagick ships without OpenMP, so every operation runs on
one core. A locally built OpenMP-enabled binary of the same version
produces pixel-identical output, several times faster on large images,
provided its thread count is bounded (an unbounded thread pool spins many
idle OpenMP threads per process and can push a batch of concurrent runs
past their timeouts):

```json
{
  "runtime": {
    "plugins": {
      "imagemagick": {
        "binary": "~/opt/imagemagick-omp/bin/magick",
        "env": {"MAGICK_THREAD_LIMIT": "4", "OMP_WAIT_POLICY": "passive"}
      }
    }
  }
}
```

```yaml
- type: tool
  name: resize
  provider: imagemagick
  params:
    args: ["convert", "in.png", "-resize", "50%", "out.png"]
```

## Parsing JSON output: `parse: json`

`shell` and every `GenericSubprocessTool`-based plugin accept an optional
`parse: json` param. `value` is then the parsed JSON of stdout (an object,
array or scalar) instead of the text, so later effects read real types
(`state.prime.report.value.ok` is a boolean) without a following `json`
effect:

```yaml
- type: tool
  name: report
  provider: shell
  params: {command: mytool, args: [summary, --json], parse: json}
```

- The raw stdout is still recorded, on `meta.stdout`.
- Empty (or whitespace-only) stdout, and stdout that is not valid JSON,
  fail the effect. The message names the exit code and quotes the first
  200 characters of stdout.
- With `allow_nonzero: true`, a non-zero exit's stdout is parsed the same
  way and `meta.exit_code` keeps the code; if that stdout is not JSON the
  effect fails as above.
- A `parse` value other than `json` fails before the process runs.

## Not covered

- **`shell`** cannot call a binary by absolute path (its allowlist works
  on bare command names, deliberately, for the security properties that
  gives); it does not read `binary` / `env`. It does take `parse: json`.
- **`gpg`**, **`diff_patch`**, **`pdf_render`**, **`web_search`**,
  **`weather`** are not `GenericSubprocessTool`-based and are out of
  scope for this page.
