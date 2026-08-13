# Shared Library Retrieval

Circuitry can retrieve orchestration assets from a shared library source.

## Scope Boundary

Publishing is intentionally out of scope for this repository.

- New shared assets are added via pull request to a separate library repository.
- This repository consumes/retrieves shared assets for execution.
- Any local filesystem library here is a development/testing mirror, not the source-of-truth publishing workflow.

**The consumption path for that publish-by-PR workflow is the `github` library
source** — see [library sources → `github`](./library-sources.md#github). Point
it at the library repository, run `cof library refresh`, and the merged
orchestrations become listable and runnable by name from a SHA-pinned local
cache (no network on the run path). `cof fetch` / `cof run-library`, documented
below, are a separate per-asset retrieval mechanism.

> **Not to be confused with [library sources](./library-sources.md).**
> `runtime.library.sources` configures where `cof list/info/run/eject` look up
> orchestrations by name (including the `github` source above); the `backend` /
> `local_root` settings below configure shared-asset retrieval (`cof fetch`,
> `cof run-library`). They share the `runtime.library` namespace but no keys.

## Configuration

Configure a library backend under `runtime.library`.

Filesystem backend (current implementation):

```json
{
  "runtime": {
    "library": {
      "backend": "filesystem",
      "local_root": "./library",
      "auth_token": "optional-token"
    }
  }
}
```

Service profiles can be defined for per-service runtime overrides without editing shared assets:

```json
{
  "runtime": {
    "library": {
      "backend": "filesystem",
      "local_root": "./library",
      "service_profiles": {
        "svc-a": {
          "default_adapter": "openai",
          "default_model": "gpt-4o-mini",
          "runtime": {
            "adapters": {
              "openai": {"timeout_seconds": 15}
            }
          }
        }
      }
    }
  }
}
```

Asset layout:
- `<local_root>/<asset_id>/<version>.yml` or `.yaml`
- optional metadata sidecar `<version>.json`

## Fetch Asset

```bash
python -m circuitry.cli.app fetch welcome --version 1.0.0 --out ./welcome.yml
```

By default, latest version is selected when `--version` is omitted.

## Run Asset Directly

```bash
python -m circuitry.cli.app run-library welcome --version 1.0.0 --dry-run --out out.json
```

Apply per-service profile:

```bash
python -m circuitry.cli.app run-library welcome --service-profile svc-a --dry-run
```

This uses the same runtime execution pipeline as local-path runs.

## Runtime Metadata

When running shared assets, run state includes:
- `runtime.shared_library.asset_id`
- `runtime.shared_library.version`
- `runtime.shared_library.source`
- `runtime.shared_library.path`
- `runtime.shared_library.retrieved_at`
- `runtime.shared_library.metadata`
- `runtime.shared_library.service_profile` (when provided)

## Auth

If `runtime.library.auth_token` is configured, pass token via:
- `--auth-token`
- or `CIRCUITRY_LIBRARY_TOKEN`
