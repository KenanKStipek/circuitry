//! Lane A: a refusal walker over a compiled [`Program`] -- the first
//! effect path the VM cannot yet run, so the CLI can refuse it up
//! front, before any state is written, with the preview marker
//! (issue #431's "Refused before the run starts" list) instead of
//! discovering mid-run that nothing implements a `prompt`/`loop`/`use`/
//! `reflector`/`yield` effect, a model-mode condition or `expect`, a
//! `tool` effect naming an unsupported `provider:`, or an unexpanded
//! `{{> name}}` partial reference (the run-time half of #406, out of
//! scope through M1-P), or a document naming a top-level `adapter:` at
//! all, since `cof run` runs preflight against it whenever a config is
//! given and M0-H ports none of that check yet -- replaced once lane R
//! of #448 (M1's run wiring v2) ports preflight (#451).
//!
//! **Capability-driven (issue #449's gate lane, item 9).** [`Supported`]
//! is one flag per capability, plus the tool-provider allow-list, so
//! each later M1 lane flips exactly one field of its own in one line
//! (and, for a lane whose effect type can itself nest other effects --
//! `reflector`'s own `effects:`, an `if`'s own branches -- the walker
//! still recurses into it once that lane's flag is `true`, so an
//! unsupported effect *inside* an otherwise-supported container is
//! still found). The walker's own body does not need to change again
//! after this lane: [`Supported::m0`] is the exact value M0 ran with
//! (every flag `false`), pinned by this module's own golden tests
//! below so no later lane can silently widen what the walker accepts
//! without a reviewed, intentional flip. [`Supported::preflight`] is
//! one such flag, added by #451: a document naming a top-level
//! `adapter:` is refused while it's `false`, the same as every other
//! capability here, even though it stands in for a safety check this
//! walker runs instead of real preflight rather than a VM capability a
//! later M1 lane implements -- flipped only once lane R of #448 ports
//! preflight for real. [`RefusalReason::PartialReference`] is the one
//! exception, with no flag of its own: M1-P's own landing removes that
//! check from the walker entirely rather than ever gating it.
//!
//! Walks the whole compiled tree in document order and stops at the
//! first node [`RefusalReason`] names -- not a list of every offending
//! node, matching the CLI's own "the first" framing (one refusal message
//! per run, same as every other check-failure surface in this
//! workspace).

use crate::op::{LeafKind, NodeKind, Op};
use crate::param::ParamNode;
use crate::path::EffectPath;
use crate::program::Program;
use crate::region::{Condition, ExpectCondition, Region};
use crate::template::TemplateText;

/// Why [`first_unsupported`] refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefusalReason {
    /// A `prompt` effect (M1-G).
    Prompt,
    /// A `loop` effect, `each` or `while` (M1-H).
    Loop,
    /// A `use` effect (M1-I).
    Use,
    /// A `reflector` effect (M1-L1).
    Reflector,
    /// A `yield` effect (M1-P, the run-time half of #406).
    Yield,
    /// An `if`/`while` condition in `mode: model` -- the compiler's own
    /// default when a condition has no `mode:` at all (M1-G2).
    ModelCondition,
    /// A tool/prompt/`use` `expect:` in `mode: model` (M1-G2).
    ModelExpect,
    /// The document declares a top-level `prompts:` map (M1-P).
    DeclaredPrompts,
    /// A `tool` effect's own `provider:` (normalized the same way
    /// `plugins/factory.py::build_plugin` does, `.strip().lower()`)
    /// isn't in [`Supported::providers`] -- the normalized name is
    /// carried here (not the raw, as-written text) so a caller that
    /// reports it in the refusal message shows the same spelling the
    /// lookup itself judged.
    UnsupportedToolProvider(String),
    /// An unexpanded `{{> name}}` partial reference in a field the VM
    /// would otherwise render (a supported tool's `prompt`/`params`/
    /// `params_json`) -- the run-time half of #406 never ran to splice
    /// it in. Unconditional (not one of [`Supported`]'s own flags):
    /// M1-P's own landing removes this check from the walker entirely,
    /// rather than ever gating it behind a flag of its own.
    PartialReference,
    /// The document's own top-level `adapter:` is a non-blank string,
    /// and [`Supported::preflight`] is `false` -- `cli/allowlist.py::
    /// walk_orchestration_refs`'s own `include_document_adapter` branch
    /// folds it into the set `preflight()` checks liveness for, and
    /// `cof run` runs preflight whenever a config is given
    /// (`cli/runtime_shim.py`, `req.config is not None`); M0-H ports
    /// none of that yet, so a document naming one is refused here
    /// instead of silently running without the check Python would have
    /// failed it on, until lane R of #448 (M1's run wiring v2) ports
    /// preflight and flips [`Supported::preflight`] for the lanes that
    /// no longer need this stand-in. The adapter name is carried
    /// already `.strip()`'d, the same text `walk_orchestration_refs`
    /// itself adds to that set ([`electricity_value::pycompat::py_strip`]
    /// -- Python's exact `str.strip()` whitespace set, not
    /// `electricity-compiler::pipeline::is_python_strip_whitespace`'s
    /// own ASCII-only approximation; issue #449's gate lane added the
    /// shared, non-approximating version this now reuses instead of
    /// keeping its own copy).
    DocumentAdapter(String),
}

/// The first effect path (and why) [`first_unsupported`] found the
/// VM cannot run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub path: EffectPath,
    pub reason: RefusalReason,
}

/// One flag per capability [`first_unsupported`] can refuse, plus the
/// tool-provider allow-list -- issue #449's gate lane, item 9. Each
/// field names the M1 lane that ever flips it from `false` to `true`;
/// the walker's own body reads every field exactly once and never
/// needs another lane-specific branch added to it.
#[derive(Debug, Clone, Copy)]
pub struct Supported<'a> {
    /// M1-G.
    pub prompt: bool,
    /// M1-H.
    pub loop_: bool,
    /// M1-I.
    pub use_: bool,
    /// M1-P.
    pub yield_: bool,
    /// M1-L1.
    pub reflector: bool,
    /// M1-G2.
    pub model_condition: bool,
    /// M1-G2.
    pub model_expect: bool,
    /// M1-P (the document-level `prompts:` map `{{> name}}` expands).
    pub declared_prompts: bool,
    /// Lane R of #448 (M1's run wiring v2): until then, a document
    /// naming a top-level `adapter:` is refused regardless of every
    /// other flag here, standing in for the preflight check M0-H never
    /// ran (#451).
    pub preflight: bool,
    /// A `tool` effect's own normalized `provider:` is accepted only
    /// when it is one of these (already-normalized, lower-case)
    /// names -- the caller's own [`electricity_tools::ToolRegistry`]/
    /// `registry::build_plugin` allow-list, e.g. `&["json"]` for M0,
    /// widened by lanes C/D1/D2/E/F2 as each native tool lands.
    pub providers: &'a [&'a str],
}

impl<'a> Supported<'a> {
    /// The exact capability set the M0-H VM ran with: every M1 effect
    /// type refused, *providers* naming exactly the tool providers the
    /// caller's own [`electricity_tools::ToolRegistry`]/`build_plugin`
    /// combination actually dispatches (`"json"`, plus `"sleep"`/
    /// `"fail"` under the `test-tools` cargo feature) -- this module's
    /// own golden tests below pin that this walker refuses *exactly*
    /// what M0 refused, for this value.
    pub fn m0(providers: &'a [&'a str]) -> Self {
        Supported {
            prompt: false,
            loop_: false,
            use_: false,
            yield_: false,
            reflector: false,
            model_condition: false,
            model_expect: false,
            declared_prompts: false,
            preflight: false,
            providers,
        }
    }
}

/// Walks *program* in document order, returning the first [`Refusal`],
/// or `None` once every effect path is one the VM can actually run
/// under *supported*.
pub fn first_unsupported(program: &Program, supported: &Supported<'_>) -> Option<Refusal> {
    if !supported.declared_prompts && !program.prompts.is_empty() {
        return Some(Refusal {
            path: EffectPath::root(),
            reason: RefusalReason::DeclaredPrompts,
        });
    }
    if !supported.preflight {
        if let Some(adapter) = &program.adapter {
            let stripped = electricity_value::pycompat::py_strip(adapter);
            if !stripped.is_empty() {
                return Some(Refusal {
                    path: EffectPath::root(),
                    reason: RefusalReason::DocumentAdapter(stripped.to_string()),
                });
            }
        }
    }
    walk_op(&program.root, supported)
}

fn walk_op(op: &Op, supported: &Supported<'_>) -> Option<Refusal> {
    match &op.kind {
        NodeKind::Leaf(leaf) => walk_leaf(&op.path, leaf, supported),
        NodeKind::Control(region) => walk_region(&op.path, region, supported),
    }
}

fn refusal(path: &EffectPath, reason: RefusalReason) -> Option<Refusal> {
    Some(Refusal {
        path: path.clone(),
        reason,
    })
}

fn walk_leaf(path: &EffectPath, leaf: &LeafKind, supported: &Supported<'_>) -> Option<Refusal> {
    match leaf {
        LeafKind::Prompt(_) => {
            if supported.prompt {
                None
            } else {
                refusal(path, RefusalReason::Prompt)
            }
        }
        LeafKind::Use(_) => {
            if supported.use_ {
                None
            } else {
                refusal(path, RefusalReason::Use)
            }
        }
        LeafKind::Reflector(reflector) => {
            if supported.reflector {
                walk_region(path, &reflector.inner, supported)
            } else {
                refusal(path, RefusalReason::Reflector)
            }
        }
        LeafKind::Yield(_) => {
            if supported.yield_ {
                None
            } else {
                refusal(path, RefusalReason::Yield)
            }
        }
        LeafKind::Tool(tool) => {
            if !supported.model_expect
                && matches!(&tool.expect, Some(ExpectCondition::Model { .. }))
            {
                return refusal(path, RefusalReason::ModelExpect);
            }
            // `plugins/factory.py::build_plugin`'s own normalization,
            // applied here too so a provider written ` JSON ` is
            // accepted the same way `cof run` would accept it, and so
            // the refusal carries the same name a caller's own registry
            // lookup would miss on.
            let normalized_provider = tool.provider.trim().to_lowercase();
            if !supported.providers.contains(&normalized_provider.as_str()) {
                return refusal(
                    path,
                    RefusalReason::UnsupportedToolProvider(normalized_provider),
                );
            }
            if tool.prompt.as_ref().is_some_and(has_partial)
                || tool.params_json.as_ref().is_some_and(has_partial)
                || param_node_has_partial(&tool.params)
            {
                return refusal(path, RefusalReason::PartialReference);
            }
            None
        }
    }
}

fn walk_region(path: &EffectPath, region: &Region, supported: &Supported<'_>) -> Option<Refusal> {
    match region {
        Region::Block { ops, .. } => ops.iter().find_map(|op| walk_op(op, supported)),
        Region::Parallel { branches, .. } => branches.iter().find_map(|op| walk_op(op, supported)),
        Region::TryFinally { body, finally } => {
            walk_region(path, body, supported).or_else(|| walk_region(path, finally, supported))
        }
        Region::If {
            cond, then_, else_, ..
        } => {
            if !supported.model_condition && matches!(cond, Condition::Model { .. }) {
                return refusal(path, RefusalReason::ModelCondition);
            }
            walk_region(path, then_, supported).or_else(|| {
                else_
                    .as_ref()
                    .and_then(|region| walk_region(path, region, supported))
            })
        }
        Region::Loop { body, .. } => {
            if supported.loop_ {
                walk_region(path, body, supported)
            } else {
                // A `loop` is refused outright regardless of its own
                // body or `while:` condition's mode -- M1-H's own
                // milestone, not a narrower per-field gap this walker's
                // `{{>`/model-mode checks would otherwise also catch
                // inside it.
                refusal(path, RefusalReason::Loop)
            }
        }
    }
}

fn has_partial(text: &TemplateText) -> bool {
    text.source.contains("{{>")
}

fn param_node_has_partial(node: &ParamNode) -> bool {
    match node {
        ParamNode::Template(text) => has_partial(text),
        ParamNode::Map(entries) => entries.values().any(param_node_has_partial),
        ParamNode::List(items) => items.iter().any(param_node_has_partial),
        ParamNode::Literal(_) | ParamNode::From { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{PromptContent, PromptOp, PromptType, RetryPolicy, ToolOp};
    use crate::op::OnError;
    use crate::template::Escape;
    use indexmap::IndexMap;
    use std::collections::BTreeSet;

    /// M0's own allow-list -- every golden test below runs against
    /// this, pinning that [`first_unsupported`] refuses *exactly* what
    /// the M0-H VM refused.
    fn m0() -> Supported<'static> {
        Supported::m0(&["json"])
    }

    fn tool_op(params: ParamNode) -> ToolOp {
        ToolOp {
            provider: "json".to_string(),
            params,
            params_json: None,
            prompt: None,
            model: None,
            timeout_ms: None,
            retries: RetryPolicy::default(),
            expect: None,
            description: None,
            group: None,
        }
    }

    fn leaf_op(path: EffectPath, kind: LeafKind) -> Op {
        Op {
            path,
            name: Some("n".to_string()),
            kind: NodeKind::Leaf(Box::new(kind)),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        }
    }

    fn root_block(ops: Vec<Op>) -> Op {
        Op {
            path: EffectPath::root(),
            name: Some("prime".to_string()),
            kind: NodeKind::Control(Region::Block {
                ops,
                overlay: false,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        }
    }

    /// A `dynamic` (chain-flow, like [`root_block`], but *not* the
    /// document root) nested as a sibling in some other region's own
    /// `ops`/`body` -- a non-overlay `Block` named *name*, same as the
    /// root but reachable only by [`walk_region`] actually recursing
    /// into a child `Op`'s own `Control` region.
    fn nested_dynamic(name: &str, path: EffectPath, ops: Vec<Op>) -> Op {
        Op {
            path,
            name: Some(name.to_string()),
            kind: NodeKind::Control(Region::Block {
                ops,
                overlay: false,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        }
    }

    /// An unsupported `use` leaf at *path* -- the shortest effect
    /// [`first_unsupported`] refuses, used throughout this module's own
    /// tests as a plain "something the walker must still find here"
    /// marker.
    fn use_leaf(path: EffectPath) -> Op {
        leaf_op(
            path,
            LeafKind::Use(crate::effects::UseOp {
                source: crate::effects::UseSource::Path("child.yml".to_string()),
                inputs: None,
                outputs: None,
                validate: true,
                retries: RetryPolicy::default(),
                expect: None,
                description: None,
            }),
        )
    }

    fn program_with_root(root: Op) -> Program {
        Program {
            root,
            prompts: IndexMap::new(),
            effect_names: BTreeSet::new(),
            document: None,
            runtime_block: None,
            interface: None,
            adapter: None,
            model: None,
        }
    }

    #[test]
    fn a_plain_tool_chain_is_fully_supported() {
        let tool_path = EffectPath::root().push_name("fetch");
        let tool = leaf_op(
            tool_path,
            LeafKind::Tool(tool_op(ParamNode::Map(IndexMap::new()))),
        );
        let program = program_with_root(root_block(vec![tool]));
        assert!(first_unsupported(&program, &m0()).is_none());
    }

    fn tool_op_with_provider(provider: &str, params: ParamNode) -> ToolOp {
        ToolOp {
            provider: provider.to_string(),
            ..tool_op(params)
        }
    }

    #[test]
    fn a_shell_provider_is_refused() {
        let tool_path = EffectPath::root().push_name("run");
        let tool = leaf_op(
            tool_path.clone(),
            LeafKind::Tool(tool_op_with_provider(
                "shell",
                ParamNode::Map(IndexMap::new()),
            )),
        );
        let program = program_with_root(root_block(vec![tool]));
        let refusal = first_unsupported(&program, &m0()).unwrap();
        assert_eq!(
            refusal.reason,
            RefusalReason::UnsupportedToolProvider("shell".to_string())
        );
        assert_eq!(refusal.path, tool_path);
    }

    #[test]
    fn a_provider_written_with_whitespace_and_mixed_case_is_still_accepted() {
        let tool_path = EffectPath::root().push_name("fetch");
        let tool = leaf_op(
            tool_path,
            LeafKind::Tool(tool_op_with_provider(
                " JSON ",
                ParamNode::Map(IndexMap::new()),
            )),
        );
        let program = program_with_root(root_block(vec![tool]));
        assert!(first_unsupported(&program, &m0()).is_none());
    }

    #[test]
    fn an_extra_allowed_provider_is_accepted_only_when_listed() {
        let tool_path = EffectPath::root().push_name("wait");
        let tool = leaf_op(
            tool_path.clone(),
            LeafKind::Tool(tool_op_with_provider(
                "sleep",
                ParamNode::Map(IndexMap::new()),
            )),
        );
        let program = program_with_root(root_block(vec![tool]));
        assert_eq!(first_unsupported(&program, &m0()).unwrap().path, tool_path);
        assert!(first_unsupported(&program, &Supported::m0(&["json", "sleep"])).is_none());
    }

    #[test]
    fn declared_prompts_refuses_at_the_document_level() {
        let mut program = program_with_root(root_block(vec![]));
        program
            .prompts
            .insert("greeting".to_string(), "hi".to_string());
        let refusal = first_unsupported(&program, &m0()).unwrap();
        assert_eq!(refusal.reason, RefusalReason::DeclaredPrompts);
    }

    #[test]
    fn declared_prompts_is_accepted_once_its_own_flag_is_set() {
        let mut program = program_with_root(root_block(vec![]));
        program
            .prompts
            .insert("greeting".to_string(), "hi".to_string());
        let supported = Supported {
            declared_prompts: true,
            ..m0()
        };
        assert!(first_unsupported(&program, &supported).is_none());
    }

    #[test]
    fn a_document_level_adapter_refuses_even_with_an_otherwise_supported_tree() {
        let tool_path = EffectPath::root().push_name("parse");
        let tool = leaf_op(
            tool_path,
            LeafKind::Tool(tool_op(ParamNode::Map(IndexMap::new()))),
        );
        let mut program = program_with_root(root_block(vec![tool]));
        program.adapter = Some("openai".to_string());
        let refusal = first_unsupported(&program, &m0()).unwrap();
        assert_eq!(
            refusal.reason,
            RefusalReason::DocumentAdapter("openai".to_string())
        );
        assert_eq!(refusal.path, EffectPath::root());
    }

    #[test]
    fn a_document_level_adapter_is_accepted_once_preflight_is_set() {
        let mut program = program_with_root(root_block(vec![]));
        program.adapter = Some("openai".to_string());
        let supported = Supported {
            preflight: true,
            ..m0()
        };
        assert!(first_unsupported(&program, &supported).is_none());
    }

    #[test]
    fn a_document_level_adapter_is_stripped_the_same_way_cof_run_strips_it() {
        let mut program = program_with_root(root_block(vec![]));
        program.adapter = Some("  openai \t".to_string());
        let refusal = first_unsupported(&program, &m0()).unwrap();
        assert_eq!(
            refusal.reason,
            RefusalReason::DocumentAdapter("openai".to_string())
        );
    }

    #[test]
    fn a_document_level_adapter_that_is_blank_after_stripping_is_not_refused() {
        let mut program = program_with_root(root_block(vec![]));
        program.adapter = Some("   ".to_string());
        assert!(first_unsupported(&program, &m0()).is_none());
    }

    /// U+2003 (EM SPACE) is Unicode `White_Space`, so `char::is_whitespace()`
    /// alone already strips it the same way Python's `str.isspace()` does --
    /// unlike the \x1c-\x1f case below, this one needs no special-casing at
    /// all, only confirms `is_whitespace()` isn't itself ASCII-only.
    #[test]
    fn a_document_level_adapter_of_only_an_em_space_is_not_refused() {
        let mut program = program_with_root(root_block(vec![]));
        program.adapter = Some("\u{2003}".to_string());
        assert!(first_unsupported(&program, &m0()).is_none());
    }

    /// \x1c (FILE SEPARATOR) is a C0 control character Python's
    /// `str.isspace()` counts as whitespace but Rust's `char::is_whitespace()`
    /// does not -- the one gap [`electricity_value::pycompat::is_py_whitespace`]'s
    /// own `\x1c`-`\x1f` range closes explicitly.
    #[test]
    fn a_document_level_adapter_of_only_a_file_separator_control_is_not_refused() {
        let mut program = program_with_root(root_block(vec![]));
        program.adapter = Some("\x1c".to_string());
        assert!(first_unsupported(&program, &m0()).is_none());
    }

    /// U+00A0 (NO-BREAK SPACE) and U+2003 (EM SPACE) bracket "openai" --
    /// both outside ASCII, confirming the stripped name survives with
    /// neither left in it, not just that the whole string isn't blank.
    #[test]
    fn a_document_level_adapter_strips_unicode_whitespace_around_the_name() {
        let mut program = program_with_root(root_block(vec![]));
        program.adapter = Some("\u{a0}openai\u{2003}".to_string());
        let refusal = first_unsupported(&program, &m0()).unwrap();
        assert_eq!(
            refusal.reason,
            RefusalReason::DocumentAdapter("openai".to_string())
        );
    }

    #[test]
    fn a_nested_use_effect_is_found_by_path() {
        let use_path = EffectPath::root().push_name("sub");
        let use_op = leaf_op(
            use_path.clone(),
            LeafKind::Use(crate::effects::UseOp {
                source: crate::effects::UseSource::Path("child.yml".to_string()),
                inputs: None,
                outputs: None,
                validate: true,
                retries: RetryPolicy::default(),
                expect: None,
                description: None,
            }),
        );
        let program = program_with_root(root_block(vec![use_op]));
        let refusal = first_unsupported(&program, &m0()).unwrap();
        assert_eq!(refusal.reason, RefusalReason::Use);
        assert_eq!(refusal.path, use_path);
    }

    #[test]
    fn model_mode_if_is_refused() {
        let if_op = Op {
            path: EffectPath::root().push_name("gate"),
            name: Some("gate".to_string()),
            kind: NodeKind::Control(Region::If {
                cond: Condition::Model {
                    template: TemplateText::new("ok?", false, Escape::None),
                },
                then_: Box::new(Region::Block {
                    ops: Vec::new(),
                    overlay: true,
                }),
                else_: None,
                threshold: 0.5,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let program = program_with_root(root_block(vec![if_op]));
        let refusal = first_unsupported(&program, &m0()).unwrap();
        assert_eq!(refusal.reason, RefusalReason::ModelCondition);
    }

    #[test]
    fn model_mode_if_is_accepted_once_its_own_flag_is_set_and_its_branches_still_walked() {
        let use_path = EffectPath::root().push_name("gate").push_name("sub");
        let if_op = Op {
            path: EffectPath::root().push_name("gate"),
            name: Some("gate".to_string()),
            kind: NodeKind::Control(Region::If {
                cond: Condition::Model {
                    template: TemplateText::new("ok?", false, Escape::None),
                },
                then_: Box::new(Region::Block {
                    ops: vec![use_leaf(use_path.clone())],
                    overlay: true,
                }),
                else_: None,
                threshold: 0.5,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let program = program_with_root(root_block(vec![if_op]));
        let supported = Supported {
            model_condition: true,
            ..m0()
        };
        let refusal = first_unsupported(&program, &supported).unwrap();
        assert_eq!(refusal.reason, RefusalReason::Use);
        assert_eq!(refusal.path, use_path);
    }

    #[test]
    fn cel_if_with_nested_use_is_found_inside_the_branch() {
        let use_path = EffectPath::root().push_name("gate").push_name("sub");
        let use_op = leaf_op(
            use_path.clone(),
            LeafKind::Use(crate::effects::UseOp {
                source: crate::effects::UseSource::Path("child.yml".to_string()),
                inputs: None,
                outputs: None,
                validate: true,
                retries: RetryPolicy::default(),
                expect: None,
                description: None,
            }),
        );
        let if_op = Op {
            path: EffectPath::root().push_name("gate"),
            name: Some("gate".to_string()),
            kind: NodeKind::Control(Region::If {
                cond: Condition::Cel {
                    expr: "true".to_string(),
                    strict: false,
                },
                then_: Box::new(Region::Block {
                    ops: vec![use_op],
                    overlay: true,
                }),
                else_: None,
                threshold: 0.5,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let program = program_with_root(root_block(vec![if_op]));
        let refusal = first_unsupported(&program, &m0()).unwrap();
        assert_eq!(refusal.reason, RefusalReason::Use);
        assert_eq!(refusal.path, use_path);
    }

    #[test]
    fn an_unsupported_effect_in_an_else_branch_is_found() {
        let use_path = EffectPath::root().push_name("gate").push_name("sub");
        let if_op = Op {
            path: EffectPath::root().push_name("gate"),
            name: Some("gate".to_string()),
            kind: NodeKind::Control(Region::If {
                cond: Condition::Cel {
                    expr: "true".to_string(),
                    strict: false,
                },
                then_: Box::new(Region::Block {
                    ops: Vec::new(),
                    overlay: true,
                }),
                else_: Some(Box::new(Region::Block {
                    ops: vec![use_leaf(use_path.clone())],
                    overlay: true,
                })),
                threshold: 0.5,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let program = program_with_root(root_block(vec![if_op]));
        let refusal = first_unsupported(&program, &m0()).unwrap();
        assert_eq!(refusal.reason, RefusalReason::Use);
        assert_eq!(refusal.path, use_path);
    }

    #[test]
    fn an_unsupported_effect_inside_a_nested_chain_dynamic_is_found() {
        // The root is itself a chain `dynamic`; one of its own children
        // is a *second*, nested `dynamic` (also chain-flow) with the
        // unsupported effect inside *that* one, not the root's own
        // `ops` directly -- proves `walk_region`'s `Block` arm actually
        // recurses into a child `Op`'s own `Control` region rather than
        // only ever walking one level of `ops`.
        let use_path = EffectPath::root().push_name("inner").push_name("sub");
        let inner = nested_dynamic(
            "inner",
            EffectPath::root().push_name("inner"),
            vec![use_leaf(use_path.clone())],
        );
        let program = program_with_root(root_block(vec![inner]));
        let refusal = first_unsupported(&program, &m0()).unwrap();
        assert_eq!(refusal.reason, RefusalReason::Use);
        assert_eq!(refusal.path, use_path);
    }

    #[test]
    fn an_unsupported_effect_in_a_nested_dynamics_own_finally_is_found() {
        // A nested `dynamic` (not the root) with a `finally:` whose own
        // unsupported effect is reachable only through `walk_region`'s
        // `TryFinally` arm -- `body` fully supported, `finally` isn't.
        let use_path = EffectPath::root().push_name("inner").push_name("cleanup");
        let inner = Op {
            path: EffectPath::root().push_name("inner"),
            name: Some("inner".to_string()),
            kind: NodeKind::Control(Region::TryFinally {
                body: Box::new(Region::Block {
                    ops: Vec::new(),
                    overlay: false,
                }),
                finally: Box::new(Region::Block {
                    ops: vec![use_leaf(use_path.clone())],
                    overlay: false,
                }),
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let program = program_with_root(root_block(vec![inner]));
        let refusal = first_unsupported(&program, &m0()).unwrap();
        assert_eq!(refusal.reason, RefusalReason::Use);
        assert_eq!(refusal.path, use_path);
    }

    #[test]
    fn a_loop_is_refused_even_with_an_otherwise_supported_body() {
        let loop_op = Op {
            path: EffectPath::root().push_name("each"),
            name: Some("each".to_string()),
            kind: NodeKind::Control(Region::Loop {
                spec: crate::region::LoopSpec::Each {
                    in_path: "input.items".to_string(),
                    as_name: "item".to_string(),
                    truncate: false,
                },
                body: Box::new(Region::Block {
                    ops: Vec::new(),
                    overlay: true,
                }),
                flow: crate::region::LoopFlow::Chain,
                max_concurrency: None,
                max_iterations: None,
                min_iterations: 0,
                collect: None,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let program = program_with_root(root_block(vec![loop_op]));
        let refusal = first_unsupported(&program, &m0()).unwrap();
        assert_eq!(refusal.reason, RefusalReason::Loop);
    }

    #[test]
    fn a_loop_once_supported_still_recurses_into_its_own_body() {
        let use_path = EffectPath::root().push_name("each").push_name("sub");
        let loop_op = Op {
            path: EffectPath::root().push_name("each"),
            name: Some("each".to_string()),
            kind: NodeKind::Control(Region::Loop {
                spec: crate::region::LoopSpec::Each {
                    in_path: "input.items".to_string(),
                    as_name: "item".to_string(),
                    truncate: false,
                },
                body: Box::new(Region::Block {
                    ops: vec![use_leaf(use_path.clone())],
                    overlay: true,
                }),
                flow: crate::region::LoopFlow::Chain,
                max_concurrency: None,
                max_iterations: None,
                min_iterations: 0,
                collect: None,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let program = program_with_root(root_block(vec![loop_op]));
        let supported = Supported {
            loop_: true,
            ..m0()
        };
        let refusal = first_unsupported(&program, &supported).unwrap();
        assert_eq!(refusal.reason, RefusalReason::Use);
        assert_eq!(refusal.path, use_path);
    }

    #[test]
    fn a_tool_expect_in_model_mode_is_refused() {
        let mut op = tool_op(ParamNode::Map(IndexMap::new()));
        op.expect = Some(ExpectCondition::Model {
            template: TemplateText::new("ok?", false, Escape::None),
        });
        let tool = leaf_op(EffectPath::root().push_name("fetch"), LeafKind::Tool(op));
        let program = program_with_root(root_block(vec![tool]));
        let refusal = first_unsupported(&program, &m0()).unwrap();
        assert_eq!(refusal.reason, RefusalReason::ModelExpect);
    }

    #[test]
    fn a_tool_expect_in_model_mode_is_accepted_once_its_own_flag_is_set() {
        let mut op = tool_op(ParamNode::Map(IndexMap::new()));
        op.expect = Some(ExpectCondition::Model {
            template: TemplateText::new("ok?", false, Escape::None),
        });
        let tool = leaf_op(EffectPath::root().push_name("fetch"), LeafKind::Tool(op));
        let program = program_with_root(root_block(vec![tool]));
        let supported = Supported {
            model_expect: true,
            ..m0()
        };
        assert!(first_unsupported(&program, &supported).is_none());
    }

    #[test]
    fn an_unexpanded_partial_in_tool_params_is_refused() {
        let mut params = IndexMap::new();
        params.insert(
            electricity_value::Value::Str("cmd".to_string()),
            ParamNode::Template(TemplateText::new("{{> greeting}}", true, Escape::Html)),
        );
        let tool = leaf_op(
            EffectPath::root().push_name("fetch"),
            LeafKind::Tool(tool_op(ParamNode::Map(params))),
        );
        let program = program_with_root(root_block(vec![tool]));
        let refusal = first_unsupported(&program, &m0()).unwrap();
        assert_eq!(refusal.reason, RefusalReason::PartialReference);
    }

    #[test]
    fn a_prompt_effect_is_refused_before_a_later_use_effect() {
        let prompt = leaf_op(
            EffectPath::root().push_name("ask"),
            LeafKind::Prompt(PromptOp {
                content: PromptContent::Template(TemplateText::new("hi", false, Escape::None)),
                prompt_type: PromptType::Text,
                schema: None,
                model: None,
                provider: None,
                provider_fallbacks: Vec::new(),
                routing_override: None,
                model_params: None,
                timeout_ms: None,
                deterministic: false,
                inputs: None,
                assets: Vec::new(),
                retries: RetryPolicy::default(),
                description: None,
                group: None,
            }),
        );
        let use_op = leaf_op(
            EffectPath::root().push_name("sub"),
            LeafKind::Use(crate::effects::UseOp {
                source: crate::effects::UseSource::Path("child.yml".to_string()),
                inputs: None,
                outputs: None,
                validate: true,
                retries: RetryPolicy::default(),
                expect: None,
                description: None,
            }),
        );
        let program = program_with_root(root_block(vec![prompt, use_op]));
        let refusal = first_unsupported(&program, &m0()).unwrap();
        assert_eq!(refusal.reason, RefusalReason::Prompt);
    }

    #[test]
    fn a_prompt_effect_is_accepted_once_its_own_flag_is_set() {
        let prompt = leaf_op(
            EffectPath::root().push_name("ask"),
            LeafKind::Prompt(PromptOp {
                content: PromptContent::Template(TemplateText::new("hi", false, Escape::None)),
                prompt_type: PromptType::Text,
                schema: None,
                model: None,
                provider: None,
                provider_fallbacks: Vec::new(),
                routing_override: None,
                model_params: None,
                timeout_ms: None,
                deterministic: false,
                inputs: None,
                assets: Vec::new(),
                retries: RetryPolicy::default(),
                description: None,
                group: None,
            }),
        );
        let program = program_with_root(root_block(vec![prompt]));
        let supported = Supported {
            prompt: true,
            ..m0()
        };
        assert!(first_unsupported(&program, &supported).is_none());
    }

    #[test]
    fn a_yield_effect_is_refused_and_then_accepted_once_its_own_flag_is_set() {
        let yield_op = leaf_op(
            EffectPath::root().push_name("say"),
            LeafKind::Yield(crate::effects::YieldOp {
                template: TemplateText::new("hi", false, Escape::None),
                inputs: None,
                description: None,
            }),
        );
        let program = program_with_root(root_block(vec![yield_op]));
        let refusal = first_unsupported(&program, &m0()).unwrap();
        assert_eq!(refusal.reason, RefusalReason::Yield);

        let supported = Supported {
            yield_: true,
            ..m0()
        };
        assert!(first_unsupported(&program, &supported).is_none());
    }

    #[test]
    fn a_reflector_effect_is_refused_and_then_recurses_into_its_own_inner_effects() {
        let use_path = EffectPath::root().push_name("think").push_name("sub");
        let reflector_op = leaf_op(
            EffectPath::root().push_name("think"),
            LeafKind::Reflector(crate::effects::ReflectorOp {
                inner: Box::new(Region::Block {
                    ops: vec![use_leaf(use_path.clone())],
                    overlay: true,
                }),
                plan_from_step: "propose_steps".to_string(),
                max_iterations: 1,
                generated_key: "generated".to_string(),
                stop_on_done: true,
                prime_template: TemplateText::new("", false, Escape::None),
                max_effects: 8,
            }),
        );
        let program = program_with_root(root_block(vec![reflector_op]));
        let refusal = first_unsupported(&program, &m0()).unwrap();
        assert_eq!(refusal.reason, RefusalReason::Reflector);

        let supported = Supported {
            reflector: true,
            ..m0()
        };
        let refusal = first_unsupported(&program, &supported).unwrap();
        assert_eq!(refusal.reason, RefusalReason::Use);
        assert_eq!(refusal.path, use_path);
    }
}
