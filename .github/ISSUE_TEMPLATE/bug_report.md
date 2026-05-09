---
name: Bug report
about: Report something Circuitry is doing wrong
title: "[bug] "
labels: ["bug"]
assignees: []
---

## Description

A clear, concise description of what's broken. What did you expect, and what actually happened?

## Reproduction

The smallest orchestration YAML and command that reproduces the issue:

```yaml
# orchestration.yml
effects:
  - type: prompt
    name: example
    template: "..."
```

```bash
cof run orchestration.yml -e key=value
```

Stack trace or error output (please redact any secrets):

```
<paste here>
```

## Expected vs. actual

- **Expected:** ...
- **Actual:** ...

## Environment

- `cof version`: <run `cof version`>
- Output of `cof doctor`:

  ```
  <paste here>
  ```
- OS / kernel: <e.g. macOS 14.5 / Ubuntu 22.04>
- Python version: <e.g. 3.11.8>
- Adapter / model in use: <e.g. ollama at http://localhost:11434, model llama3.1:8b>

## Additional context

Logs, configuration snippets (with secrets redacted), screenshots, links to related issues, etc.
