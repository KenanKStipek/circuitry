# Security Policy

## Supported Versions

Circuitry is in early alpha. Only the latest `0.1.x` release line receives security fixes.

| Version | Supported |
| ------- | --------- |
| 0.1.x   | Yes       |
| < 0.1   | No        |

## Reporting a Vulnerability

Please report suspected vulnerabilities **privately** to:

**kenan@stipek.org**

Use a clear subject line such as `[security] <short description>`. Include:

- A description of the issue and the impact you believe it could have
- Reproduction steps (orchestration YAML, CLI invocation, environment)
- Affected version (`cof version`)
- Any relevant logs or stack traces (with secrets redacted)

Please **do not** open a public GitHub issue for vulnerabilities until a fix is released.

### Expected response

- Acknowledgment within **5 business days**.
- If confirmed, we will work with you on a remediation timeline and credit you in the release notes (unless you prefer to remain anonymous).
- For non-vulnerability bug reports, please use the public issue tracker instead.

## Scope

In scope:

- The `circuitry` Python package and the `cof` CLI
- Bundled tool plugins under `src/circuitry/plugins/` (currently `ffmpeg`, `comfyui`)
- Bundled orchestrations under `src/circuitry/bundled/orchestrations/`
- The orchestration JSON Schema at `src/circuitry/schema/orchestration.schema.json`

Out of scope:

- Vulnerabilities in user-authored orchestrations or in user-supplied tool plugins
- Vulnerabilities in upstream dependencies (`ollama`, `openai`, `anthropic`, `litellm`, `ffmpeg`, `ComfyUI`) — please report those to their maintainers
- Issues in third-party adapters or integrations not maintained in this repository
- Prompt-injection attacks against models invoked through Circuitry (these are a property of the model and the user's prompt design, not the framework)

## Threat model

For the framework's documented attack surfaces, mitigations, and known limitations, see [`docs/threat-model.md`](docs/threat-model.md).
