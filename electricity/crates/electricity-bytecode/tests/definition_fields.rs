//! Every field of Circuitry's effect `*Definition` dataclasses (and
//! their nested field types) has a documented IR counterpart (issue
//! #408's Scope section). `golden/python_definition_fields.json`
//! (`electricity/scripts/generate_compiler_definition_fields.py
//! --check`) is the ground truth, generated from the real dataclasses;
//! this test's `MAPPING` is the hand-maintained side, naming the IR
//! field/type that carries each Python field, or the documented reason
//! it's intentionally absent. A new Python field appears in the golden
//! file with nothing in `MAPPING` and fails here until both are
//! updated.

use serde_json::Value as JsonValue;
use std::collections::BTreeMap;

const GOLDEN: &str = include_str!("golden/python_definition_fields.json");

/// `(dataclass, python_field) -> where it lives in electricity-bytecode`.
///
/// Every entry documents a real IR field or type; see the referenced
/// type/field's own doc comment in `src/` for the full correspondence.
/// A few are prose notes instead of a field path, for a Python field
/// that's deliberately not IR-carried (a documented divergence) or
/// that's hoisted onto the shared `Op` rather than kept per-effect-type.
fn mapping() -> BTreeMap<(&'static str, &'static str), &'static str> {
    let mut m = BTreeMap::new();
    let mut add = |class: &'static str, field: &'static str, ir: &'static str| {
        m.insert((class, field), ir);
    };

    // PromptDefinition -> effects::PromptOp (name/on_error/enabled -> Op)
    add("PromptDefinition", "name", "op::Op::name");
    add(
        "PromptDefinition",
        "template",
        "effects::PromptContent::Template",
    );
    add(
        "PromptDefinition",
        "messages",
        "effects::PromptContent::Messages",
    );
    add(
        "PromptDefinition",
        "prompt_type",
        "effects::PromptOp::prompt_type",
    );
    add("PromptDefinition", "schema", "effects::PromptOp::schema");
    add("PromptDefinition", "model", "effects::PromptOp::model");
    add(
        "PromptDefinition",
        "provider",
        "effects::PromptOp::provider",
    );
    add(
        "PromptDefinition",
        "provider_fallbacks",
        "effects::PromptOp::provider_fallbacks",
    );
    add(
        "PromptDefinition",
        "routing_override",
        "effects::PromptOp::routing_override",
    );
    add(
        "PromptDefinition",
        "params",
        "effects::PromptOp::model_params",
    );
    add(
        "PromptDefinition",
        "timeout_ms",
        "effects::PromptOp::timeout_ms",
    );
    add(
        "PromptDefinition",
        "deterministic",
        "effects::PromptOp::deterministic",
    );
    add("PromptDefinition", "inputs", "effects::PromptOp::inputs");
    add("PromptDefinition", "assets", "effects::PromptOp::assets");
    add("PromptDefinition", "retries", "effects::PromptOp::retries");
    add("PromptDefinition", "on_error", "op::Op::on_error");
    add(
        "PromptDefinition",
        "description",
        "effects::PromptOp::description",
    );
    add("PromptDefinition", "enabled", "op::Op::enabled");
    add("PromptDefinition", "group", "effects::PromptOp::group");

    // MessageDef -> effects::Message
    add(
        "MessageDef",
        "role",
        "effects::Message::role (effects::Role)",
    );
    add("MessageDef", "content", "effects::Message::content");

    // AssetRefDef -> effects::AssetRef (ref renamed: Rust keyword)
    add("AssetRefDef", "kind", "effects::AssetRef::kind");
    add("AssetRefDef", "ref", "effects::AssetRef::reference");

    // RetryPolicyDef -> effects::RetryPolicy
    add(
        "RetryPolicyDef",
        "max_attempts",
        "effects::RetryPolicy::max_attempts",
    );
    add(
        "RetryPolicyDef",
        "backoff_ms",
        "effects::RetryPolicy::backoff_ms",
    );

    // ToolDefinition -> effects::ToolOp
    add("ToolDefinition", "name", "op::Op::name");
    add("ToolDefinition", "provider", "effects::ToolOp::provider");
    add("ToolDefinition", "params", "effects::ToolOp::params");
    add(
        "ToolDefinition",
        "params_json",
        "effects::ToolOp::params_json",
    );
    add("ToolDefinition", "prompt", "effects::ToolOp::prompt");
    add("ToolDefinition", "model", "effects::ToolOp::model");
    add(
        "ToolDefinition",
        "timeout_ms",
        "effects::ToolOp::timeout_ms",
    );
    add("ToolDefinition", "on_error", "op::Op::on_error");
    add(
        "ToolDefinition",
        "description",
        "effects::ToolOp::description",
    );
    add("ToolDefinition", "retries", "effects::ToolOp::retries");
    add("ToolDefinition", "expect", "effects::ToolOp::expect");
    add("ToolDefinition", "enabled", "op::Op::enabled");
    add("ToolDefinition", "group", "effects::ToolOp::group");

    // ExpectDef -> region::ExpectCondition
    add("ExpectDef", "mode", "region::ExpectCondition (variant tag)");
    add("ExpectDef", "expr", "region::ExpectCondition::Cel::expr");
    add(
        "ExpectDef",
        "template",
        "region::ExpectCondition::Model::template",
    );

    // UseDefinition -> effects::UseOp
    add("UseDefinition", "name", "op::Op::name");
    add(
        "UseDefinition",
        "ref",
        "documented divergence: no IR field -- ref: is rejected at compile \
         time (no library-name/remote-library resolution, DESIGN.md §4)",
    );
    add("UseDefinition", "path", "effects::UseSource::Path");
    add(
        "UseDefinition",
        "orchestration",
        "effects::UseSource::Orchestration",
    );
    add("UseDefinition", "inline", "effects::UseSource::Inline");
    add("UseDefinition", "inputs", "effects::UseOp::inputs");
    add("UseDefinition", "outputs", "effects::UseOp::outputs");
    add("UseDefinition", "validate", "effects::UseOp::validate");
    add("UseDefinition", "on_error", "op::Op::on_error");
    add(
        "UseDefinition",
        "description",
        "effects::UseOp::description",
    );
    add("UseDefinition", "retries", "effects::UseOp::retries");
    add("UseDefinition", "expect", "effects::UseOp::expect");
    add("UseDefinition", "enabled", "op::Op::enabled");

    // YieldDefinition -> effects::YieldOp
    add("YieldDefinition", "name", "op::Op::name");
    add("YieldDefinition", "template", "effects::YieldOp::template");
    add("YieldDefinition", "inputs", "effects::YieldOp::inputs");
    add("YieldDefinition", "on_error", "op::Op::on_error");
    add(
        "YieldDefinition",
        "description",
        "effects::YieldOp::description",
    );
    add("YieldDefinition", "enabled", "op::Op::enabled");

    // ReflectorDefinition -> effects::ReflectorOp
    add("ReflectorDefinition", "name", "op::Op::name");
    add(
        "ReflectorDefinition",
        "inner",
        "effects::ReflectorOp::inner",
    );
    add(
        "ReflectorDefinition",
        "plan_from_step",
        "effects::ReflectorOp::plan_from_step",
    );
    add(
        "ReflectorDefinition",
        "max_iterations",
        "effects::ReflectorOp::max_iterations",
    );
    add(
        "ReflectorDefinition",
        "generated_key",
        "effects::ReflectorOp::generated_key",
    );
    add(
        "ReflectorDefinition",
        "stop_on_done",
        "effects::ReflectorOp::stop_on_done",
    );
    add(
        "ReflectorDefinition",
        "prime_template",
        "effects::ReflectorOp::prime_template",
    );
    add(
        "ReflectorDefinition",
        "max_effects",
        "effects::ReflectorOp::max_effects",
    );
    // ReflectorDefinition has no on_error field of its own in Python.
    add("ReflectorDefinition", "enabled", "op::Op::enabled");

    // DynamicDefinition -> region::Region::{Block,Parallel,TryFinally}
    add("DynamicDefinition", "name", "op::Op::name");
    add(
        "DynamicDefinition",
        "effects",
        "region::Region::Block::ops / Parallel::branches",
    );
    add(
        "DynamicDefinition",
        "flow",
        "region::Region variant choice (Block = chain, Parallel = tree)",
    );
    add(
        "DynamicDefinition",
        "finally_effects",
        "region::Region::TryFinally::finally",
    );
    add(
        "DynamicDefinition",
        "max_concurrency",
        "region::Region::Parallel::max_concurrency",
    );
    add(
        "DynamicDefinition",
        "stop_on_error",
        "region::Region::Parallel::stop_on_error",
    );
    add("DynamicDefinition", "on_error", "op::Op::on_error");
    add("DynamicDefinition", "labels", "op::Op::labels");
    add("DynamicDefinition", "enabled", "op::Op::enabled");
    add(
        "DynamicDefinition",
        "prompts",
        "program::Program::prompts (root dynamic only; non-root is always empty)",
    );
    add(
        "DynamicDefinition",
        "effect_names",
        "program::Program::effect_names (root dynamic only; non-root is always empty)",
    );

    // ConditionalDefinition -> region::Region::If
    add(
        "ConditionalDefinition",
        "name",
        "op::Op::name (None for an unnamed/transparent if)",
    );
    add(
        "ConditionalDefinition",
        "condition",
        "region::Region::If::cond",
    );
    add(
        "ConditionalDefinition",
        "then_effects",
        "region::Region::If::then_",
    );
    add(
        "ConditionalDefinition",
        "else_effects",
        "region::Region::If::else_",
    );
    add(
        "ConditionalDefinition",
        "threshold",
        "region::Region::If::threshold",
    );
    add("ConditionalDefinition", "on_error", "op::Op::on_error");
    add("ConditionalDefinition", "enabled", "op::Op::enabled");
    add("ConditionalDefinition", "labels", "op::Op::labels");

    // ConditionDef (if:/while: shared shape) -> region::Condition
    add("ConditionDef", "mode", "region::Condition (variant tag)");
    add(
        "ConditionDef",
        "template",
        "region::Condition::Model::template",
    );
    add("ConditionDef", "expr", "region::Condition::Cel::expr");
    add("ConditionDef", "strict", "region::Condition::Cel::strict");

    // LoopDefinition -> region::Region::Loop
    add(
        "LoopDefinition",
        "name",
        "op::Op::name (None for an unnamed/transparent loop)",
    );
    add("LoopDefinition", "body", "region::Region::Loop::body");
    add("LoopDefinition", "while_def", "region::LoopSpec::While");
    add("LoopDefinition", "each_def", "region::LoopSpec::Each");
    add(
        "LoopDefinition",
        "max_iterations",
        "region::Region::Loop::max_iterations",
    );
    add(
        "LoopDefinition",
        "min_iterations",
        "region::Region::Loop::min_iterations",
    );
    add("LoopDefinition", "on_error", "op::Op::on_error");
    add("LoopDefinition", "collect", "region::Region::Loop::collect");
    add(
        "LoopDefinition",
        "flow",
        "region::Region::Loop::flow (region::LoopFlow)",
    );
    add(
        "LoopDefinition",
        "max_concurrency",
        "region::Region::Loop::max_concurrency",
    );
    add("LoopDefinition", "enabled", "op::Op::enabled");
    add("LoopDefinition", "labels", "op::Op::labels");

    // LoopWhileDef (same shape as ConditionDef) -> region::Condition
    add("LoopWhileDef", "mode", "region::Condition (variant tag)");
    add(
        "LoopWhileDef",
        "template",
        "region::Condition::Model::template",
    );
    add("LoopWhileDef", "expr", "region::Condition::Cel::expr");
    add("LoopWhileDef", "strict", "region::Condition::Cel::strict");

    // LoopEachDef -> region::LoopSpec::Each
    add("LoopEachDef", "in_path", "region::LoopSpec::Each::in_path");
    add("LoopEachDef", "as_name", "region::LoopSpec::Each::as_name");
    add(
        "LoopEachDef",
        "truncate",
        "region::LoopSpec::Each::truncate",
    );

    m
}

#[test]
fn every_python_definition_field_has_an_ir_counterpart() {
    let golden: BTreeMap<String, Vec<String>> =
        serde_json::from_str(GOLDEN).expect("golden/python_definition_fields.json is valid JSON");
    let mapping = mapping();

    let mut missing = Vec::new();
    for (class, fields) in &golden {
        for field in fields {
            if !mapping.contains_key(&(class.as_str(), field.as_str())) {
                missing.push(format!("{class}.{field}"));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "fields with no IR mapping in tests/definition_fields.rs -- add an \
         entry to `mapping()` (and the corresponding IR field, unless the \
         field is a documented divergence): {missing:?}"
    );
}

#[test]
fn mapping_has_no_entries_for_fields_the_golden_file_no_longer_has() {
    // Catches the mapping going stale the other direction: an entry for
    // a field a dataclass no longer has (renamed/removed) would
    // otherwise sit here silently, unnoticed, forever.
    let golden: BTreeMap<String, Vec<String>> =
        serde_json::from_str(GOLDEN).expect("golden/python_definition_fields.json is valid JSON");
    let mapping = mapping();

    let mut stale = Vec::new();
    for (class, field) in mapping.keys() {
        let still_present = golden
            .get(*class)
            .map(|fields| fields.iter().any(|f| f == field))
            .unwrap_or(false);
        if !still_present {
            stale.push(format!("{class}.{field}"));
        }
    }

    assert!(
        stale.is_empty(),
        "tests/definition_fields.rs's mapping() has entries for fields no \
         longer in golden/python_definition_fields.json: {stale:?}"
    );
}

#[test]
fn golden_file_is_valid_json_object_of_string_arrays() {
    let value: JsonValue = serde_json::from_str(GOLDEN).unwrap();
    let object = value.as_object().expect("golden file is a JSON object");
    for (class, fields) in object {
        let array = fields
            .as_array()
            .unwrap_or_else(|| panic!("{class}'s value is not an array"));
        for field in array {
            assert!(
                field.is_string(),
                "{class} has a non-string field name: {field:?}"
            );
        }
    }
}
