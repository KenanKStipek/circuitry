//! Lane A: `Region`, `Condition`, `ExpectCondition` and `LoopSpec` —
//! control-flow containers (issue #408's Scope section; DESIGN.md §5.1).

use crate::op::Op;
use crate::template::TemplateText;
use serde::Serialize;

/// A `mode: cel`/`mode: model` condition, shared by a `conditional`'s
/// `if:` and a `loop`'s `while:` (`ConditionDef`/`LoopWhileDef` — both
/// Python dataclasses have the identical `mode`/`template`/`expr`/
/// `strict` shape, so one IR type carries both).
#[derive(Debug, Clone, Serialize)]
pub enum Condition {
    Cel { expr: String, strict: bool },
    Model { template: TemplateText },
}

impl Condition {
    /// `ConditionDef.mode`/`LoopWhileDef.mode`'s own string spelling --
    /// an `if` node's `meta.mode` (`core/conditional.py`'s own `meta`
    /// dict, issue #408's finding 5; the M0-H VM only ever executes the
    /// `Cel` variant, the compiler's `mode: model` default notwithstanding
    /// -- [`crate::refusal`] refuses a `Model` condition before the run
    /// starts).
    pub fn mode(&self) -> &'static str {
        match self {
            Condition::Cel { .. } => "cel",
            Condition::Model { .. } => "model",
        }
    }
}

/// A tool/prompt/`use`'s `expect:` condition (`ExpectDef`).
///
/// Not [`Condition`]: `ExpectDef` has no `strict` field, so its `Cel`
/// variant can't borrow `Condition::Cel`'s shape without inventing a
/// value Python never has.
#[derive(Debug, Clone, Serialize)]
pub enum ExpectCondition {
    Cel { expr: String },
    Model { template: TemplateText },
}

/// A `loop`'s `each:`/`while:` — exactly one of the two is present
/// (`LoopDefinition.while_def`/`each_def`, checked at compile time).
#[derive(Debug, Clone, Serialize)]
pub enum LoopSpec {
    Each {
        in_path: String,
        /// Defaults to `"item"` (`LoopEachDef.as_name`).
        as_name: String,
        truncate: bool,
    },
    While(Condition),
}

/// `loop`'s and `dynamic`'s `flow:` (`chain` or `tree`), kept as its own
/// field on [`Region::Loop`] rather than folded into [`LoopSpec`] — a
/// `while` loop is always sequential (runtime-semantics §5.5; no tree-
/// mode `while` exists), but the field still has to exist for an `each`
/// loop's chain/tree choice, so both loop kinds carry it uniformly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum LoopFlow {
    Chain,
    Tree,
}

impl LoopFlow {
    /// `"chain"`/`"tree"` -- the exact `flow:` spelling Circuitry's own
    /// `meta.flow` (dynamic) and `DynamicDefinition.flow`/
    /// `LoopDefinition.flow` (compiler dump) both use.
    pub fn as_str(self) -> &'static str {
        match self {
            LoopFlow::Chain => "chain",
            LoopFlow::Tree => "tree",
        }
    }
}

/// Nested, WASM-style control flow (DESIGN.md §5.1) — the compiled shape
/// of a `dynamic`, `if`, `loop`, or a `finally:` wrapper.
#[derive(Debug, Clone, Serialize)]
pub enum Region {
    /// A `dynamic`'s (or the document root's) `flow: chain` effect list,
    /// a `loop` body, or an `if` branch.
    ///
    /// `overlay` is `false` for the document root and for a `dynamic`
    /// (runtime-semantics §2.4's "top-level root is not a scope-overlay
    /// container"), and `true` for a loop body and an `if` branch.
    ///
    /// Known divergence (crate docs): a chain-flow `dynamic`'s own
    /// `max_concurrency`/`stop_on_error` have no field here — Circuitry
    /// documents both as meaningful only under `flow: tree`.
    Block { ops: Vec<Op>, overlay: bool },
    /// A `dynamic`'s `flow: tree` effect list — a *statically known*
    /// branch list only; a tree-mode `each` loop's runtime-sized pass
    /// set is represented by `Region::Loop` with `flow: Tree`, never by
    /// this variant (DESIGN.md §5.1).
    Parallel {
        branches: Vec<Op>,
        max_concurrency: Option<u32>,
        stop_on_error: bool,
    },
    /// A `conditional` (`if:`/`then:`/`else:`). `threshold:` is carried
    /// through to `meta` but never consulted by the interpreter
    /// (runtime-semantics §5.6); defaults to `0.5`
    /// (`ConditionalDefinition.threshold`).
    If {
        cond: Condition,
        then_: Box<Region>,
        else_: Option<Box<Region>>,
        threshold: f64,
    },
    /// A `loop` (`each`/`while`, chain or tree).
    Loop {
        spec: LoopSpec,
        body: Box<Region>,
        flow: LoopFlow,
        max_concurrency: Option<u32>,
        max_iterations: Option<u32>,
        min_iterations: u32,
        /// Needs a *named* loop (checked at compile time) —
        /// `LoopDefinition.collect`.
        collect: Option<String>,
    },
    /// A `dynamic`'s (or the document root's) `finally:` — wraps the
    /// `Block`/`Parallel` region built from its own effects, sharing one
    /// `seen_names` set with it at compile time (runtime-semantics
    /// §6.4), rather than compiling `finally:`'s effects as a sibling
    /// list.
    TryFinally {
        body: Box<Region>,
        finally: Box<Region>,
    },
}

impl Region {
    /// `meta.flow` (`"chain"`/`"tree"`) for a `dynamic`/document-root
    /// node's own [`Region`] -- `None` for an `If`/`Loop` region, which
    /// have no `flow:` of their own (a `Loop`'s chain/tree choice is its
    /// own [`LoopFlow`] field, read directly, never through this
    /// method). Looks through [`Region::TryFinally`] to its `body`: a
    /// `dynamic`'s `finally:` wraps the very `Block`/`Parallel` region
    /// built from its own effects (this crate's own docs on
    /// `TryFinally`), so the wrapper itself carries no flow of its own
    /// to report -- the body's is the node's.
    pub fn dynamic_flow(&self) -> Option<LoopFlow> {
        match self {
            Region::Block { .. } => Some(LoopFlow::Chain),
            Region::Parallel { .. } => Some(LoopFlow::Tree),
            Region::TryFinally { body, .. } => body.dynamic_flow(),
            Region::If { .. } | Region::Loop { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::Escape;

    fn block() -> Region {
        Region::Block {
            ops: Vec::new(),
            overlay: false,
        }
    }

    fn parallel() -> Region {
        Region::Parallel {
            branches: Vec::new(),
            max_concurrency: None,
            stop_on_error: false,
        }
    }

    #[test]
    fn dynamic_flow_reads_block_and_parallel() {
        assert_eq!(block().dynamic_flow(), Some(LoopFlow::Chain));
        assert_eq!(parallel().dynamic_flow(), Some(LoopFlow::Tree));
    }

    #[test]
    fn dynamic_flow_looks_through_try_finally_to_the_body() {
        let wrapped = Region::TryFinally {
            body: Box::new(parallel()),
            finally: Box::new(block()),
        };
        assert_eq!(wrapped.dynamic_flow(), Some(LoopFlow::Tree));
    }

    #[test]
    fn dynamic_flow_is_none_for_if_and_loop() {
        let cond = Region::If {
            cond: Condition::Cel {
                expr: "true".to_string(),
                strict: false,
            },
            then_: Box::new(block()),
            else_: None,
            threshold: 0.5,
        };
        assert_eq!(cond.dynamic_flow(), None);

        let loop_region = Region::Loop {
            spec: LoopSpec::Each {
                in_path: "input.items".to_string(),
                as_name: "item".to_string(),
                truncate: false,
            },
            body: Box::new(block()),
            flow: LoopFlow::Chain,
            max_concurrency: None,
            max_iterations: None,
            min_iterations: 0,
            collect: None,
        };
        assert_eq!(loop_region.dynamic_flow(), None);
    }

    #[test]
    fn condition_mode_matches_the_variant() {
        assert_eq!(
            Condition::Cel {
                expr: "true".to_string(),
                strict: false
            }
            .mode(),
            "cel"
        );
        assert_eq!(
            Condition::Model {
                template: TemplateText::new("?", false, Escape::None)
            }
            .mode(),
            "model"
        );
    }

    #[test]
    fn loop_flow_as_str_matches_circuitrys_spelling() {
        assert_eq!(LoopFlow::Chain.as_str(), "chain");
        assert_eq!(LoopFlow::Tree.as_str(), "tree");
    }
}
