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
