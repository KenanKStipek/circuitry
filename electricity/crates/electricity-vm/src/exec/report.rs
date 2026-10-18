//! `report` effect execution (#605) -- a close port of
//! `core/report.py::ReportRuntime.execute`: renders *report_op*'s own
//! `message` (with `{{> name}}` partial expansion, unescaped -- it is
//! prompt text, like a `yield`'s) and resolves `data`'s own [`ParamNode`]
//! tree, then fires [`RunObserver::report`] -- never
//! [`RunObserver::effect_start`]/[`RunObserver::effect_complete`], so a
//! report never counts toward `runtime.last_run.totals.effects_run` and
//! never appears as a start/end event. A named report also writes its
//! own node (`value = {message, level, data}`, with `meta.created_at`/
//! `completed_at`); an unnamed one writes nothing.
//!
//! A report never fails the run: it has no retries, no `on_error`, no
//! capability check. A `data` leaf's `{from: <path>}` that doesn't
//! resolve (missing or an explicit `null`) becomes `null`, with one
//! warning naming the path -- never an error. If rendering raises
//! anyway (a genuine template failure), it is logged as one warning and
//! the report still publishes, with an empty message and no data.

use crate::exec::CtxChain;
use crate::exec::live_scope::LiveRoot;
use crate::params::ParamsCtx;
use crate::{Frame, NodeRef, RunContext, RunObserver, Store};
use electricity_bytecode::{Op, ParamNode, ReportOp};
use electricity_template::PlainCtx;
use electricity_value::{Dict, Value};

fn now_iso() -> Value {
    use chrono::SecondsFormat;
    Value::Str(chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Micros, false))
}

fn set_leaf(store: &Store, node: &NodeRef, key: &str, value: Value) {
    store.set_leaf(node, Value::Str(key.to_string()), value);
}

/// Resolves a report's own `data:` tree against *ctx* -- never fails:
/// a `{from: <path>}` leaf that doesn't resolve (missing or an explicit
/// `null`) logs one warning naming *effect_path*/*field* -- always
/// `"report at '<path>'"`, regardless of whether this report is named
/// (`core/report.py::ReportRuntime._resolve_data`'s own wording) -- and
/// becomes `Value::None`; a string leaf renders like the report's own
/// `message` (unescaped, partials allowed), propagating a genuine
/// render failure up to the caller, which folds it into the same
/// "never fails the run" fallback the message's own render failure
/// gets; anything else is kept as written.
#[allow(clippy::too_many_arguments)]
fn resolve_data<R: ParamsCtx>(
    node: &ParamNode,
    ctx: &R,
    declared_prompts: &indexmap::IndexMap<String, String>,
    effect_names: &std::collections::BTreeSet<String>,
    effect_path: &str,
    field: &str,
) -> Result<Value, String> {
    match node {
        ParamNode::From { path, .. } => match ctx.resolve_from(path) {
            Some(value) if !matches!(value, Value::None) => Ok(value),
            _ => {
                log::warn!(
                    "report at '{effect_path}' data '{field}': '{{from: {path}}}' did not \
                     resolve to a value; using null."
                );
                Ok(Value::None)
            }
        },
        ParamNode::Template(text) => {
            let label = format!("report data '{field}'");
            crate::compose::render_with_composition(
                &text.source,
                ctx,
                declared_prompts,
                effect_names,
                &label,
                &PlainCtx,
                false,
            )
            .map(Value::Str)
        }
        ParamNode::Map(entries) => {
            let mut map = Dict::new();
            for (key, item) in entries {
                let key_str = match key {
                    Value::Str(s) => s.clone(),
                    other => other.py_str(),
                };
                let child = resolve_data(
                    item,
                    ctx,
                    declared_prompts,
                    effect_names,
                    effect_path,
                    &format!("{field}.{key_str}"),
                )?;
                map.insert(key.clone(), child);
            }
            Ok(Value::Dict(map))
        }
        ParamNode::List(items) => {
            let mut list = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                list.push(resolve_data(
                    item,
                    ctx,
                    declared_prompts,
                    effect_names,
                    effect_path,
                    &format!("{field}[{index}]"),
                )?);
            }
            Ok(Value::List(list))
        }
        ParamNode::Literal(value) => Ok(value.clone()),
    }
}

/// Runs a `report` effect: renders *report_op*'s own `message` and
/// `data` against *ctx_chain*, resolved lazily through [`LiveRoot`],
/// publishes through [`RunObserver::report`], and (when named) writes
/// its own node -- see the module docstring.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_report(
    op: &Op,
    report_op: &ReportOp,
    store: &Store,
    parent: &NodeRef,
    ctx_chain: &CtxChain,
    frame: &Frame,
    run_ctx: &RunContext<'_>,
    observer: &dyn RunObserver,
) {
    let live_root = LiveRoot::new(ctx_chain, store);
    let effect_path = frame.effect_path(&op.path);
    let effect_path_str = effect_path.to_string();
    // The message-render label is name-conditional (`core/report.py::
    // ReportRuntime.execute`'s own `label=`); every other warning this
    // effect can log always says `"report at '<path>'"`, named or not.
    let message_label = match &op.name {
        Some(name) => format!("report '{name}' message"),
        None => format!("report at '{effect_path_str}' message"),
    };

    let render_result: Result<(String, Option<Value>), String> = (|| {
        let message = crate::compose::render_with_composition(
            &report_op.message.source,
            &live_root,
            run_ctx.declared_prompts,
            run_ctx.effect_names,
            &message_label,
            &PlainCtx,
            false,
        )?;
        let data = match &report_op.data {
            Some(node) => Some(resolve_data(
                node,
                &live_root,
                run_ctx.declared_prompts,
                run_ctx.effect_names,
                &effect_path_str,
                "data",
            )?),
            None => None,
        };
        Ok((message, data))
    })();

    let (message, data) = match render_result {
        Ok(pair) => pair,
        Err(err) => {
            log::warn!(
                "report at '{effect_path_str}' failed to render; publishing it with no \
                 message or data: {err}"
            );
            (String::new(), None)
        }
    };

    if let Some(name) = &op.name {
        let name_key = Value::Str(name.to_string());
        let effect_node = match store.ensure_dict(parent, name_key) {
            Ok(node) => node,
            Err(_) => return,
        };
        let mut value_dict = Dict::new();
        value_dict.insert(
            Value::Str("message".to_string()),
            Value::Str(message.clone()),
        );
        value_dict.insert(
            Value::Str("level".to_string()),
            Value::Str(
                match report_op.level {
                    electricity_bytecode::ReportLevel::Info => "info",
                    electricity_bytecode::ReportLevel::Warning => "warning",
                }
                .to_string(),
            ),
        );
        value_dict.insert(
            Value::Str("data".to_string()),
            data.clone().unwrap_or(Value::None),
        );
        set_leaf(store, &effect_node, "value", Value::Dict(value_dict));
        let meta_node = match store.ensure_dict(&effect_node, Value::Str("meta".to_string())) {
            Ok(node) => node,
            Err(_) => return,
        };
        let now = now_iso();
        set_leaf(store, &meta_node, "created_at", now.clone());
        set_leaf(store, &meta_node, "completed_at", now);
        set_leaf(store, &meta_node, "dry_run", Value::Bool(run_ctx.dry_run));
    }

    observer.report(&effect_path, report_op.level, &message, data.as_ref());
    observer.write();
}
