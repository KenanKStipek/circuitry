# Editor Highlighting (MVP)

## Scope

MVP supports syntax highlighting for Circuitry orchestration YAML through a lightweight VS Code TextMate grammar.

- Packaging target: local VS Code extension scaffold at `editor/vscode-circuitry`
- Grammar file: `editor/vscode-circuitry/syntaxes/circuitry.tmLanguage.json`
- File extension target: `.circuitry.yml`, `.circuitry.yaml`

## Installation (Local VS Code)

1. Open VS Code in this repository.
2. Run extension host from the extension folder:
   - `cd editor/vscode-circuitry`
   - `code .`
   - press `F5` to launch Extension Development Host
3. In the extension host, open a `.circuitry.yml` file and confirm syntax highlighting.

Alternative for existing `.yml` files:
- Add file association in VS Code settings:
  - `"files.associations": { "*.circuitry.yml": "circuitry" }`

## Token Coverage

Current grammar highlights:
- `type` values: `prompt`, `dynamic`, `if`, `conditional`, `loop`, `reflector`
- Common orchestration keys: `effects`, `flow`, `template`, `prompt_type`, `schema`, `each`, `then`, `else`, etc.
- `flow` values: `chain`, `tree`, aliases (`cot`, `tot`, etc.)
- `prompt_type` values: `text`, `json`, `boolean`, `number`, `array`, `object`, `tool`

Representative fixtures:
- `orchestrations/hello.yml`
- `orchestrations/typed_prompt_example.yml`
- `orchestrations/multi_primitive_story.yml`

## Known Limitations

- This is syntax highlighting only; no schema validation or autocomplete.
- Existing `.yml` files require file association or rename to `.circuitry.yml`.
- Grammar is intentionally minimal for MVP and does not include semantic validation.

## Contribution Workflow

1. Update grammar in `editor/vscode-circuitry/syntaxes/circuitry.tmLanguage.json`.
2. Add/update fixture coverage using `orchestrations/` files.
3. Run verification:
   - `pytest -q tests/docs/test_editor_highlighting.py`
4. Update this document if token classes or install flow changes.
