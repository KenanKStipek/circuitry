//! The plan side: compiling a document into the tree `osp` displays
//! (DESIGN.md §5, `PlanTree`). O-1 fills in the tree itself — the path
//! pattern matching against state and event paths, `use` grafting, and
//! the fallback for a document that fails to compile.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use electricity_bytecode::{
    EffectPath, LeafKind, NodeKind, OnError, Op, PathSegment, Program, Region, UseSource,
};
use electricity_compiler::{CheckOptions, RunCheckError, check_for_run};

/// Compiles `path` with the same options `cof`'s `runtime_shim.run`
/// passes (DESIGN.md §5): calling code has already decided to run the
/// document, so preflight and trust checks are skipped here, not
/// reimplemented.
///
/// `CheckOptions` is built from `Default` plus field assignment, not a
/// struct literal: its field list is still growing as electricity's
/// compiler lanes land, and a literal would need an update — silently
/// defaulting the rest is correct here — every time one does.
#[allow(clippy::field_reassign_with_default)]
pub fn compile(path: &Path) -> Result<Program, RunCheckError> {
    let mut options = CheckOptions::default();
    options.skip_preflight = true;
    options.trust_document = true;
    check_for_run(path, &options)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Chain,
    Tree,
}

/// What kind of plan node matched a concrete path — enough for the
/// status/skip-reason rules (DESIGN.md §2) and the details-pane summary
/// to tell containers from leaves without re-matching the IR.
#[derive(Debug, Clone)]
pub enum PlanEntryKind {
    Leaf,
    Use,
    Loop {
        max_concurrency: Option<u32>,
        flow: Flow,
        named: bool,
    },
    If {
        named: bool,
    },
    Dynamic {
        flow: Flow,
    },
    TryFinally,
}

#[derive(Debug, Clone)]
pub struct PlanEntry {
    pub name: Option<String>,
    pub on_error: OnError,
    pub enabled: bool,
    pub kind: PlanEntryKind,
    /// The flow of the *container* this entry sits directly inside
    /// (`None` for the document root) — DESIGN.md §2.1 rule 1 only
    /// applies to a chain-flow container.
    pub parent_flow: Option<Flow>,
}

/// One pass index resolved while matching a concrete path against a
/// `Pass` placeholder segment (DESIGN.md §2.3): the compiled loop's
/// ordinal, and the pass number taken from `iter_<n>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedPass {
    pub loop_id: u32,
    pub pass_index: u32,
}

pub struct Match<'a> {
    pub entries: &'a [PlanEntry],
    pub passes: Vec<ResolvedPass>,
}

#[derive(Default)]
struct TrieNode {
    exact: HashMap<String, TrieNode>,
    pass: Option<(u32, Box<TrieNode>)>,
    entries: Vec<PlanEntry>,
}

/// The compiled plan, joined against concrete state/event paths
/// (DESIGN.md §5's "Joining plan paths with state and event paths").
/// `PlanTree::empty()` is the no-plan fallback: every path matches
/// nothing, and `osp` builds its tree purely from what it observes.
pub struct PlanTree {
    root: TrieNode,
    has_plan: bool,
    all_paths: Vec<String>,
}

impl PlanTree {
    pub fn empty() -> Self {
        PlanTree {
            root: TrieNode::default(),
            has_plan: false,
            all_paths: Vec::new(),
        }
    }

    pub fn has_plan(&self) -> bool {
        self.has_plan
    }

    /// Every plan path, in a display-stable form (a `Pass` segment
    /// renders as `iter_*`, DESIGN.md §5's path-display ask) — used to
    /// seed rows for plan paths osp hasn't observed yet. A path under a
    /// named loop appears once per compiled body, not once per pass:
    /// per-pass rows come from what's observed.
    pub fn all_paths(&self) -> &[String] {
        &self.all_paths
    }

    pub fn from_program(program: &Program) -> Self {
        let mut trie = TrieNode::default();
        let mut all_paths = Vec::new();
        let base_dir = program
            .document
            .as_ref()
            .map(|d| d.resolved_directory.clone());
        let mut cycle_guard = Vec::new();
        if let Some(doc) = &program.document {
            if let Ok(canon) = PathBuf::from(&doc.path_as_given).canonicalize() {
                cycle_guard.push(canon);
            }
        }
        let mut ctx = WalkCtx {
            base_dir: base_dir.as_deref(),
            cycle_guard: &mut cycle_guard,
            all_paths: &mut all_paths,
        };
        walk_op(&program.root, None, None, &mut trie, &mut ctx);
        PlanTree {
            root: trie,
            has_plan: true,
            all_paths,
        }
    }

    /// Matches a concrete dotted path (as seen in state or an event)
    /// against the plan, resolving any `Pass` placeholders it crosses.
    /// Returns every candidate plan op at that path (DESIGN.md §5: a
    /// `then`/`else` reusing a name, or an unnamed `if`, can share one
    /// concrete path).
    pub fn match_path(&self, path: &str) -> Option<Match<'_>> {
        let mut node = &self.root;
        let mut passes = Vec::new();
        for part in path.split('.') {
            if let Some(child) = node.exact.get(part) {
                node = child;
                continue;
            }
            if let Some((loop_id, child)) = &node.pass {
                if let Some(idx) = part
                    .strip_prefix("iter_")
                    .and_then(|s| s.parse::<u32>().ok())
                {
                    passes.push(ResolvedPass {
                        loop_id: *loop_id,
                        pass_index: idx,
                    });
                    node = child;
                    continue;
                }
            }
            return None;
        }
        Some(Match {
            entries: &node.entries,
            passes,
        })
    }
}

struct WalkCtx<'a> {
    base_dir: Option<&'a Path>,
    cycle_guard: &'a mut Vec<PathBuf>,
    all_paths: &'a mut Vec<String>,
}

fn display_path(path: &EffectPath) -> String {
    path.0
        .iter()
        .map(|seg| match seg {
            PathSegment::Name(n) => n.clone(),
            PathSegment::Pass(_) => "iter_*".to_string(),
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn effective_path(op_path: &EffectPath, graft: Option<&EffectPath>) -> EffectPath {
    match graft {
        None => op_path.clone(),
        Some(prefix) => {
            let mut segs = prefix.0.clone();
            segs.extend(op_path.0.iter().skip(1).cloned());
            EffectPath(segs)
        }
    }
}

fn insert(trie: &mut TrieNode, path: &EffectPath, entry: PlanEntry) {
    let mut node = trie;
    for seg in &path.0 {
        node = match seg {
            PathSegment::Name(n) => node.exact.entry(n.clone()).or_default(),
            PathSegment::Pass(loop_id) => {
                &mut node
                    .pass
                    .get_or_insert_with(|| (loop_id.0, Box::default()))
                    .1
            }
        };
    }
    node.entries.push(entry);
}

fn region_kind(region: &Region) -> PlanEntryKind {
    match region {
        Region::Block { .. } => PlanEntryKind::Dynamic { flow: Flow::Chain },
        Region::Parallel { .. } => PlanEntryKind::Dynamic { flow: Flow::Tree },
        Region::If { .. } => PlanEntryKind::If { named: true },
        Region::Loop {
            flow,
            max_concurrency,
            ..
        } => PlanEntryKind::Loop {
            max_concurrency: *max_concurrency,
            flow: match flow {
                electricity_bytecode::LoopFlow::Chain => Flow::Chain,
                electricity_bytecode::LoopFlow::Tree => Flow::Tree,
            },
            named: true,
        },
        Region::TryFinally { .. } => PlanEntryKind::TryFinally,
    }
}

fn walk_op(
    op: &Op,
    graft: Option<&EffectPath>,
    parent_flow: Option<Flow>,
    trie: &mut TrieNode,
    ctx: &mut WalkCtx,
) {
    let path = effective_path(&op.path, graft);
    let kind = match &op.kind {
        NodeKind::Leaf(leaf) => match leaf.as_ref() {
            LeafKind::Use(_) => PlanEntryKind::Use,
            _ => PlanEntryKind::Leaf,
        },
        NodeKind::Control(region) => {
            let mut k = region_kind(region);
            if op.name.is_none() {
                if let PlanEntryKind::If { named } | PlanEntryKind::Loop { named, .. } = &mut k {
                    *named = false;
                }
            }
            k
        }
    };

    ctx.all_paths.push(display_path(&path));
    insert(
        trie,
        &path,
        PlanEntry {
            name: op.name.clone(),
            on_error: op.on_error,
            enabled: op.enabled,
            kind,
            parent_flow,
        },
    );

    match &op.kind {
        NodeKind::Leaf(leaf) => match leaf.as_ref() {
            LeafKind::Use(use_op) => {
                if let UseSource::Path(child_rel) = &use_op.source {
                    try_graft_use(child_rel, &path, trie, ctx);
                }
            }
            LeafKind::Reflector(r) => {
                walk_region(&r.inner, graft, trie, ctx);
            }
            _ => {}
        },
        NodeKind::Control(region) => walk_region(region, graft, trie, ctx),
    }
}

/// Recurses into a region's nested ops. Each `Op`'s own `parent_flow`
/// (DESIGN.md §2.1 rule 1) comes directly from *this* region's own
/// variant — `Block` is always the chain construct and `Parallel`
/// always the tree one, regardless of what (if anything) wraps them;
/// an `If`/`Loop`/`TryFinally` region has no flow of its own, only the
/// `Block`/`Parallel` nested inside it does.
fn walk_region(
    region: &Region,
    graft: Option<&EffectPath>,
    trie: &mut TrieNode,
    ctx: &mut WalkCtx,
) {
    match region {
        Region::Block { ops, .. } => {
            for op in ops {
                walk_op(op, graft, Some(Flow::Chain), trie, ctx);
            }
        }
        Region::Parallel { branches, .. } => {
            for op in branches {
                walk_op(op, graft, Some(Flow::Tree), trie, ctx);
            }
        }
        Region::If { then_, else_, .. } => {
            walk_region(then_, graft, trie, ctx);
            if let Some(else_) = else_ {
                walk_region(else_, graft, trie, ctx);
            }
        }
        Region::Loop { body, .. } => {
            walk_region(body, graft, trie, ctx);
        }
        Region::TryFinally { body, finally } => {
            walk_region(body, graft, trie, ctx);
            walk_region(finally, graft, trie, ctx);
        }
    }
}

/// Compiles a `use: path:`'s child document and grafts its root's
/// children under `use_path` (DESIGN.md §5), stripping the child's own
/// `prime` the same way the run's own state does. Any failure (missing
/// file, compile error, cycle) leaves the `use` a plain leaf: its
/// children are then discovered from observations alone, same as a
/// `use: inline` child.
fn try_graft_use(child_rel: &str, use_path: &EffectPath, trie: &mut TrieNode, ctx: &mut WalkCtx) {
    let Some(base_dir) = ctx.base_dir else { return };
    let child_file = base_dir.join(child_rel);
    let Ok(canon) = child_file.canonicalize() else {
        return;
    };
    if ctx.cycle_guard.contains(&canon) {
        return;
    }
    let Ok(child_program) = compile(&child_file) else {
        return;
    };

    ctx.cycle_guard.push(canon);
    let child_base_dir = child_program
        .document
        .as_ref()
        .map(|d| d.resolved_directory.clone());
    {
        let mut child_ctx = WalkCtx {
            base_dir: child_base_dir.as_deref(),
            cycle_guard: ctx.cycle_guard,
            all_paths: ctx.all_paths,
        };
        if let NodeKind::Control(region) = &child_program.root.kind {
            walk_region(region, Some(use_path), trie, &mut child_ctx);
        }
    }
    ctx.cycle_guard.pop();
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_bytecode::{LeafKind, LoopId, LoopSpec, NodeKind, OnError, Op, Region};

    fn tool_op(path: EffectPath, name: &str) -> Op {
        Op {
            path,
            name: Some(name.to_string()),
            kind: NodeKind::Leaf(Box::new(LeafKind::Tool(electricity_bytecode::ToolOp {
                provider: "shell".to_string(),
                params: electricity_bytecode::ParamNode::Literal(electricity_value::Value::None),
                params_json: None,
                prompt: None,
                model: None,
                timeout_ms: None,
                retries: Default::default(),
                expect: None,
                description: None,
                group: None,
            }))),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        }
    }

    #[test]
    fn matches_a_simple_chain_path() {
        let root_path = EffectPath::root();
        let child_path = root_path.push_name("step1");
        let root = Op {
            path: root_path,
            name: Some("prime".to_string()),
            kind: NodeKind::Control(Region::Block {
                ops: vec![tool_op(child_path, "step1")],
                overlay: false,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let program = Program {
            root,
            prompts: Default::default(),
            effect_names: Default::default(),
            document: None,
            runtime_block: None,
            interface: None,
            adapter: None,
            model: None,
        };
        let plan = PlanTree::from_program(&program);
        let m = plan.match_path("prime.step1").expect("should match");
        assert_eq!(m.entries.len(), 1);
        assert_eq!(m.entries[0].name.as_deref(), Some("step1"));
        assert!(plan.match_path("prime.nonexistent").is_none());
    }

    #[test]
    fn matches_a_pass_placeholder_under_a_named_loop() {
        let root_path = EffectPath::root();
        let loop_path = root_path.push_name("each_chain");
        let pass_path = loop_path.push_pass(LoopId(0));
        let body_path = pass_path.push_name("nap");
        let root = Op {
            path: root_path,
            name: Some("prime".to_string()),
            kind: NodeKind::Control(Region::Block {
                ops: vec![Op {
                    path: loop_path,
                    name: Some("each_chain".to_string()),
                    kind: NodeKind::Control(Region::Loop {
                        spec: LoopSpec::Each {
                            in_path: "prime.items".to_string(),
                            as_name: "item".to_string(),
                            truncate: false,
                        },
                        body: Box::new(Region::Block {
                            ops: vec![tool_op(body_path, "nap")],
                            overlay: true,
                        }),
                        flow: electricity_bytecode::LoopFlow::Chain,
                        max_concurrency: None,
                        max_iterations: None,
                        min_iterations: 0,
                        collect: None,
                    }),
                    on_error: OnError::Fail,
                    labels: None,
                    enabled: true,
                }],
                overlay: false,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let program = Program {
            root,
            prompts: Default::default(),
            effect_names: Default::default(),
            document: None,
            runtime_block: None,
            interface: None,
            adapter: None,
            model: None,
        };
        let plan = PlanTree::from_program(&program);
        let m = plan
            .match_path("prime.each_chain.iter_2.nap")
            .expect("should match a pass");
        assert_eq!(
            m.passes,
            vec![ResolvedPass {
                loop_id: 0,
                pass_index: 2
            }]
        );
        assert_eq!(m.entries[0].name.as_deref(), Some("nap"));
    }

    #[test]
    fn then_and_else_reusing_a_name_merge_into_one_row() {
        let root_path = EffectPath::root();
        let if_path = root_path.push_name("gate");
        let chosen_then = if_path.push_name("chosen");
        let chosen_else = if_path.push_name("chosen");
        let root = Op {
            path: root_path,
            name: Some("prime".to_string()),
            kind: NodeKind::Control(Region::Block {
                ops: vec![Op {
                    path: if_path,
                    name: Some("gate".to_string()),
                    kind: NodeKind::Control(Region::If {
                        cond: electricity_bytecode::Condition::Cel {
                            expr: "true".to_string(),
                            strict: false,
                        },
                        then_: Box::new(Region::Block {
                            ops: vec![tool_op(chosen_then, "chosen")],
                            overlay: true,
                        }),
                        else_: Some(Box::new(Region::Block {
                            ops: vec![tool_op(chosen_else, "chosen")],
                            overlay: true,
                        })),
                        threshold: 0.5,
                    }),
                    on_error: OnError::Fail,
                    labels: None,
                    enabled: true,
                }],
                overlay: false,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let program = Program {
            root,
            prompts: Default::default(),
            effect_names: Default::default(),
            document: None,
            runtime_block: None,
            interface: None,
            adapter: None,
            model: None,
        };
        let plan = PlanTree::from_program(&program);
        let m = plan.match_path("prime.gate.chosen").expect("should match");
        assert_eq!(m.entries.len(), 2);
    }

    #[test]
    fn empty_plan_matches_nothing() {
        let plan = PlanTree::empty();
        assert!(plan.match_path("prime.anything").is_none());
        assert!(!plan.has_plan());
    }

    #[test]
    fn compile_a_missing_file_is_an_error() {
        let err = compile(Path::new("/nonexistent-osp-path/does-not-exist.yml"));
        assert!(err.is_err());
    }
}
