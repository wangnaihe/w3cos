//! Debug-only cycle detector and retainer dump (`--features leak-detector`).
//!
//! Reference counting cannot reclaim cycles. This module finds them by trial
//! deletion over the page arena's strong counts:
//!
//! 1. Snapshot every live arena slot (object / array / function) and every
//!    host `Rc` value reachable from one, with their visible strong edges
//!    (properties, prototype, call slot, private elements, array items, AOT
//!    capture cells, function properties).
//! 2. A node whose strong count exceeds the edges pointing at it from other
//!    nodes is *externally rooted*: held by the Rust stack, a register file,
//!    module/realm state, a host side table, or a native closure whose
//!    captures cannot be enumerated.
//! 3. Everything reachable from a root is alive for a reason; the retainer
//!    chain from the nearest root is reported for live objects that are not
//!    part of the [`mark_baseline`] snapshot (realm/module state).
//! 4. Live nodes **not** reachable from any root are kept alive only by each
//!    other: a leaked cycle. Its strongly connected component is reported
//!    with the full reference chain and a suggested edge to turn into a
//!    `WeakRef`.
//!
//! It only reads: nothing is cloned, dropped or freed. Edges hidden inside
//! native closures make their targets look externally rooted, so such
//! cycles show up as "residual (externally retained)" rather than as cycles;
//! the detector never reports a false cycle from them.
//!
//! On demand: [`report`] (API), or set `W3COS_LEAK_REPORT` (`1`/`text`,
//! `json`, or a file path; `.json` paths get JSON) to print a report at every
//! [`crate::page_arena::reset`] and from [`report_from_env`].

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;

use crate::page_arena::{self, SlotKind};
use crate::value::{ArrayStorage, FunctionData, Value};

/// Strong edge target extracted from a payload.
#[derive(Clone, Copy)]
pub(crate) enum LeakTarget {
    Arena(SlotKind, u32),
    HostObject(*const RefCell<crate::JsObject>, u32),
    HostArray(*const RefCell<ArrayStorage>, u32),
    HostFunction(*const FunctionData, u32),
    /// Shared AOT capture cell (`let` captured by closures).
    HostCell(*const RefCell<(Value, bool)>, u32),
}

/// Payload pointer of a live arena slot.
#[derive(Clone, Copy)]
pub(crate) enum Payload {
    Object(*const RefCell<crate::JsObject>),
    Array(*const RefCell<ArrayStorage>),
    Function(*const FunctionData),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum NodeKey {
    Arena(SlotKind, usize),
    Host(usize),
}

struct Node {
    key: NodeKey,
    kind: &'static str,
    label: String,
    strong: u32,
    edges: Vec<(String, usize)>,
    /// Some strong edges could not be enumerated (native closure, proxy).
    opaque: bool,
    baseline: bool,
}

/// `(epoch, live slots)` recorded by [`mark_baseline`].
type Baseline = Option<(u32, HashSet<(SlotKind, usize, u32)>)>;
/// Walk work item: node, its payload, or its capture cell.
type WorkItem = (
    usize,
    Option<Payload>,
    Option<*const RefCell<(Value, bool)>>,
);

thread_local! {
    static BASELINE: RefCell<Baseline> = const { RefCell::new(None) };
}

/// Record the currently live arena slots as the baseline (realm, builtin
/// prototypes, module state). Reports label them `realm` and do not count
/// them as residual. Cleared by [`crate::page_arena::reset`].
pub fn mark_baseline() {
    let slots = page_arena::scan_live_slots()
        .into_iter()
        .map(|slot| (slot.kind, slot.index, slot.generation))
        .collect();
    BASELINE.with(|baseline| {
        *baseline.borrow_mut() = Some((page_arena::current_epoch(), slots));
    });
}

pub fn clear_baseline() {
    BASELINE.with(|baseline| *baseline.borrow_mut() = None);
}

/// One link of a reference chain.
#[derive(Clone, Debug)]
pub struct ChainLink {
    pub node: String,
    /// Edge from this node to the next one (empty on the last link).
    pub edge: String,
}

/// A leaked strongly connected component.
#[derive(Clone, Debug)]
pub struct LeakCycle {
    /// Number of nodes in the component.
    pub size: usize,
    /// Closed chain: the last link's edge points back at the first node.
    pub chain: Vec<ChainLink>,
    /// The edge suggested to become a `WeakRef`, as `node --edge--> target`.
    pub suggested_weak_edge: String,
}

/// A live, non-baseline arena value that is still reachable.
#[derive(Clone, Debug)]
pub struct Residual {
    pub node: String,
    /// From the externally rooted node down to this one.
    pub retainer_chain: Vec<ChainLink>,
    /// Strong references from outside the visible graph at the chain root.
    pub external_refs: u32,
}

#[derive(Clone, Debug, Default)]
pub struct LeakReport {
    pub live_objects: usize,
    pub live_arrays: usize,
    pub live_functions: usize,
    pub baseline: usize,
    pub cycles: Vec<LeakCycle>,
    /// Nodes held only by leaked cycles (not themselves in a cycle).
    pub held_by_cycles: usize,
    pub residual: Vec<Residual>,
    pub residual_total: usize,
}

impl LeakReport {
    pub fn is_clean(&self) -> bool {
        self.cycles.is_empty() && self.held_by_cycles == 0 && self.residual_total == 0
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "w3cos leak report: live objects={} arrays={} functions={} (baseline/realm {}), leaked cycles={}, held by cycles={}, residual={}",
            self.live_objects,
            self.live_arrays,
            self.live_functions,
            self.baseline,
            self.cycles.len(),
            self.held_by_cycles,
            self.residual_total
        );
        for (index, cycle) in self.cycles.iter().enumerate() {
            let _ = writeln!(
                out,
                "cycle #{index} ({} nodes, unreachable from any root):",
                cycle.size
            );
            for link in &cycle.chain {
                let _ = writeln!(out, "    {}", link.node);
                if !link.edge.is_empty() {
                    let _ = writeln!(out, "      --{}-->", link.edge);
                }
            }
            let _ = writeln!(out, "    (back to the first node)");
            let _ = writeln!(
                out,
                "  suggestion: make {} a WeakRef",
                cycle.suggested_weak_edge
            );
        }
        for residual in &self.residual {
            let _ = writeln!(out, "residual: {}", residual.node);
            let _ = writeln!(
                out,
                "  retained via ({} external ref(s) at the root: Rust stack/registers, host state, or a native closure's captures):",
                residual.external_refs
            );
            for link in &residual.retainer_chain {
                if link.edge.is_empty() {
                    let _ = writeln!(out, "    {}", link.node);
                } else {
                    let _ = writeln!(out, "    {} --{}-->", link.node, link.edge);
                }
            }
        }
        if self.residual_total > self.residual.len() {
            let _ = writeln!(
                out,
                "... {} more residual values",
                self.residual_total - self.residual.len()
            );
        }
        out
    }

    pub fn to_json(&self) -> serde_json::Value {
        let chain = |links: &[ChainLink]| {
            links
                .iter()
                .map(|link| serde_json::json!({ "node": link.node, "edge": link.edge }))
                .collect::<Vec<_>>()
        };
        serde_json::json!({
            "live": { "objects": self.live_objects, "arrays": self.live_arrays, "functions": self.live_functions },
            "baseline": self.baseline,
            "held_by_cycles": self.held_by_cycles,
            "residual_total": self.residual_total,
            "cycles": self.cycles.iter().map(|cycle| serde_json::json!({
                "size": cycle.size,
                "chain": chain(&cycle.chain),
                "suggested_weak_edge": cycle.suggested_weak_edge,
            })).collect::<Vec<_>>(),
            "residual": self.residual.iter().map(|residual| serde_json::json!({
                "node": residual.node,
                "external_refs": residual.external_refs,
                "retainer_chain": chain(&residual.retainer_chain),
            })).collect::<Vec<_>>(),
        })
    }
}

/// Upper bound on listed residual values (the total is always counted).
const RESIDUAL_LIMIT: usize = 20;
const CYCLE_LIMIT: usize = 50;

struct Graph {
    nodes: Vec<Node>,
    index: HashMap<NodeKey, usize>,
}

impl Graph {
    fn intern(&mut self, target: LeakTarget, pending: &mut Vec<usize>) -> Option<usize> {
        let (key, strong) = match target {
            LeakTarget::Arena(kind, handle) => {
                (NodeKey::Arena(kind, page_arena::handle_index(handle)?), 0)
            }
            LeakTarget::HostObject(ptr, strong) => (NodeKey::Host(ptr as usize), strong),
            LeakTarget::HostArray(ptr, strong) => (NodeKey::Host(ptr as usize), strong),
            LeakTarget::HostFunction(ptr, strong) => (NodeKey::Host(ptr as usize), strong),
            LeakTarget::HostCell(ptr, strong) => (NodeKey::Host(ptr as usize), strong),
        };
        if let Some(&index) = self.index.get(&key) {
            return Some(index);
        }
        // Arena slots are all added up front; an unknown arena key is a
        // stale handle, not an edge.
        let NodeKey::Host(_) = key else { return None };
        let (kind, label, payload) = match target {
            LeakTarget::HostObject(ptr, _) => {
                ("host object", String::new(), Some(Payload::Object(ptr)))
            }
            LeakTarget::HostArray(ptr, _) => {
                ("host array", String::new(), Some(Payload::Array(ptr)))
            }
            LeakTarget::HostFunction(ptr, _) => {
                ("host function", String::new(), Some(Payload::Function(ptr)))
            }
            LeakTarget::HostCell(_, _) => ("capture cell", String::new(), None),
            LeakTarget::Arena(..) => unreachable!(),
        };
        let index = self.nodes.len();
        self.nodes.push(Node {
            key,
            kind,
            label,
            strong,
            edges: Vec::new(),
            opaque: false,
            baseline: false,
        });
        self.index.insert(key, index);
        let _ = payload;
        pending.push(index);
        Some(index)
    }
}

/// Edges and label of a payload. Safety: the payload is alive for the whole
/// scan (nothing is dropped while it runs) and only read.
unsafe fn describe(payload: Payload, out: &mut Vec<(String, LeakTarget)>) -> (String, bool) {
    match payload {
        Payload::Object(ptr) => {
            let cell = unsafe { &*ptr };
            match cell.try_borrow() {
                Ok(object) => {
                    let opaque = object.leak_edges(out);
                    let (keys, total) = object.leak_keys(6);
                    let more = if total > keys.len() { ", …" } else { "" };
                    (format!("{{{}{more}}}", keys.join(", ")), opaque)
                }
                Err(_) => ("{<borrowed>}".to_string(), true),
            }
        }
        Payload::Array(ptr) => {
            let cell = unsafe { &*ptr };
            match cell.try_borrow() {
                Ok(array) => {
                    array.leak_edges(out);
                    (format!("[len {}]", array.len()), false)
                }
                Err(_) => ("[<borrowed>]".to_string(), true),
            }
        }
        Payload::Function(ptr) => {
            let function = unsafe { &*ptr };
            let opaque = function.leak_edges(out);
            (function.leak_label(), opaque)
        }
    }
}

fn build_graph() -> Graph {
    let slots = page_arena::scan_live_slots();
    let baseline = BASELINE.with(|baseline| {
        baseline
            .borrow()
            .as_ref()
            .filter(|(epoch, _)| *epoch == page_arena::current_epoch())
            .map(|(_, slots)| slots.clone())
    });
    let mut graph = Graph {
        nodes: Vec::new(),
        index: HashMap::new(),
    };
    let mut payloads = Vec::new();
    for slot in &slots {
        let key = NodeKey::Arena(slot.kind, slot.index);
        let kind = match slot.kind {
            SlotKind::Object => "object",
            SlotKind::Array => "array",
            SlotKind::Function => "function",
        };
        let index = graph.nodes.len();
        graph.nodes.push(Node {
            key,
            kind,
            label: String::new(),
            strong: slot.strong,
            edges: Vec::new(),
            opaque: false,
            baseline: baseline
                .as_ref()
                .is_some_and(|set| set.contains(&(slot.kind, slot.index, slot.generation))),
        });
        graph.index.insert(key, index);
        payloads.push((index, Some(slot.payload), None));
    }
    // Host nodes are discovered while walking edges.
    let mut queue: VecDeque<WorkItem> = payloads.into_iter().collect();
    while let Some((index, payload, cell)) = queue.pop_front() {
        let mut raw = Vec::new();
        if let Some(payload) = payload {
            let (label, opaque) = unsafe { describe(payload, &mut raw) };
            graph.nodes[index].label = label;
            graph.nodes[index].opaque = opaque;
        } else if let Some(cell) = cell {
            let cell = unsafe { &*cell };
            match cell.try_borrow() {
                Ok(binding) => {
                    crate::value::leak_edges::push_value(&mut raw, "value".to_string(), &binding.0)
                }
                Err(_) => graph.nodes[index].opaque = true,
            }
        }
        for (label, target) in raw {
            let mut pending = Vec::new();
            let Some(to) = graph.intern(target, &mut pending) else {
                continue;
            };
            for new in pending {
                let next = match target {
                    LeakTarget::HostObject(ptr, _) => (new, Some(Payload::Object(ptr)), None),
                    LeakTarget::HostArray(ptr, _) => (new, Some(Payload::Array(ptr)), None),
                    LeakTarget::HostFunction(ptr, _) => (new, Some(Payload::Function(ptr)), None),
                    LeakTarget::HostCell(ptr, _) => (new, None, Some(ptr)),
                    LeakTarget::Arena(..) => continue,
                };
                queue.push_back(next);
            }
            graph.nodes[index].edges.push((label, to));
        }
    }
    graph
}

fn node_name(node: &Node) -> String {
    let place = match node.key {
        NodeKey::Arena(_, index) => format!("#{index}"),
        NodeKey::Host(addr) => format!("@{addr:x}"),
    };
    let mut name = format!("{} {place} {}", node.kind, node.label);
    if node.baseline {
        name.push_str(" [realm]");
    }
    if node.opaque {
        name.push_str(" [has hidden edges]");
    }
    name.trim_end().to_string()
}

/// Strongly connected components (iterative Tarjan) restricted to `members`.
fn components(graph: &Graph, members: &HashSet<usize>) -> Vec<Vec<usize>> {
    let mut index_of = HashMap::new();
    let mut low = HashMap::new();
    let mut on_stack = HashSet::new();
    let mut stack = Vec::new();
    let mut result = Vec::new();
    let mut counter = 0usize;
    let mut roots = members.iter().copied().collect::<Vec<_>>();
    roots.sort_unstable();
    for root in roots {
        if index_of.contains_key(&root) {
            continue;
        }
        let mut work = vec![(root, 0usize)];
        while let Some((node, edge)) = work.pop() {
            if edge == 0 && !index_of.contains_key(&node) {
                index_of.insert(node, counter);
                low.insert(node, counter);
                counter += 1;
                stack.push(node);
                on_stack.insert(node);
            }
            let edges = &graph.nodes[node].edges;
            if edge < edges.len() {
                work.push((node, edge + 1));
                let next = edges[edge].1;
                if !members.contains(&next) {
                    continue;
                }
                if !index_of.contains_key(&next) {
                    work.push((next, 0));
                } else if on_stack.contains(&next) {
                    let value = low[&node].min(index_of[&next]);
                    low.insert(node, value);
                }
                continue;
            }
            // Done with `node`: propagate to its parent on the work stack.
            if let Some(&(parent, _)) = work.last()
                && members.contains(&parent)
                && on_stack.contains(&node)
            {
                let value = low[&parent].min(low[&node]);
                low.insert(parent, value);
            }
            if low[&node] == index_of[&node] {
                let mut component = Vec::new();
                while let Some(top) = stack.pop() {
                    on_stack.remove(&top);
                    component.push(top);
                    if top == node {
                        break;
                    }
                }
                result.push(component);
            }
        }
    }
    result
}

/// A closed walk through `component` starting and ending at `start`.
fn cycle_chain(graph: &Graph, component: &HashSet<usize>, start: usize) -> Vec<(usize, String)> {
    // BFS from start's successors back to start.
    let mut parent: HashMap<usize, (usize, String)> = HashMap::new();
    let mut queue = VecDeque::new();
    for (label, to) in &graph.nodes[start].edges {
        if *to == start {
            return vec![(start, label.clone())];
        }
        if component.contains(to) && !parent.contains_key(to) {
            parent.insert(*to, (start, label.clone()));
            queue.push_back(*to);
        }
    }
    while let Some(node) = queue.pop_front() {
        for (label, to) in &graph.nodes[node].edges {
            if *to == start {
                let mut path = vec![(node, label.clone())];
                let mut cursor = node;
                while cursor != start {
                    let (from, edge) = parent[&cursor].clone();
                    path.push((from, edge));
                    cursor = from;
                }
                path.reverse();
                return path;
            }
            if component.contains(to) && !parent.contains_key(to) && *to != start {
                parent.insert(*to, (node, label.clone()));
                queue.push_back(*to);
            }
        }
    }
    Vec::new()
}

/// Prefer breaking a back-pointer-looking edge; otherwise the edge that
/// closes the cycle onto its oldest (lowest slot) node.
fn suggest_weak_edge(graph: &Graph, chain: &[(usize, String)]) -> String {
    const HINTS: [&str; 8] = [
        "parent", "owner", "back", "self", "prev", "capture", "iterator", "listener",
    ];
    let pick = chain
        .iter()
        .enumerate()
        .find(|(_, (_, edge))| {
            let edge = edge.to_ascii_lowercase();
            HINTS.iter().any(|hint| edge.contains(hint))
        })
        .map(|(position, _)| position)
        .unwrap_or(chain.len() - 1);
    let (from, edge) = &chain[pick];
    let to = chain[(pick + 1) % chain.len()].0;
    format!(
        "`{}` --{}--> `{}`",
        node_name(&graph.nodes[*from]),
        edge,
        node_name(&graph.nodes[to])
    )
}

/// Scan the live heap and build a report. Never frees anything.
pub fn report() -> LeakReport {
    let graph = build_graph();
    let count = graph.nodes.len();
    let mut internal = vec![0u32; count];
    for node in &graph.nodes {
        for (_, to) in &node.edges {
            internal[*to] = internal[*to].saturating_add(1);
        }
    }
    // Trial deletion: subtract internal edges; what remains is external.
    let external = (0..count)
        .map(|index| graph.nodes[index].strong.saturating_sub(internal[index]))
        .collect::<Vec<_>>();
    let mut parent: Vec<Option<(usize, String)>> = vec![None; count];
    let mut reached = vec![false; count];
    let mut queue = VecDeque::new();
    for index in 0..count {
        if external[index] > 0 {
            reached[index] = true;
            queue.push_back(index);
        }
    }
    while let Some(node) = queue.pop_front() {
        for (label, to) in &graph.nodes[node].edges {
            if !reached[*to] {
                reached[*to] = true;
                parent[*to] = Some((node, label.clone()));
                queue.push_back(*to);
            }
        }
    }
    let garbage = (0..count)
        .filter(|index| !reached[*index])
        .collect::<HashSet<_>>();
    let mut cycles = Vec::new();
    let mut in_cycles = HashSet::new();
    let mut comps = components(&graph, &garbage);
    comps.sort_by_key(|component| component.iter().copied().min());
    for component in comps {
        let self_loop = component.len() == 1
            && graph.nodes[component[0]]
                .edges
                .iter()
                .any(|(_, to)| *to == component[0]);
        if component.len() < 2 && !self_loop {
            continue;
        }
        in_cycles.extend(component.iter().copied());
        if cycles.len() >= CYCLE_LIMIT {
            continue;
        }
        let set = component.iter().copied().collect::<HashSet<_>>();
        let start = component
            .iter()
            .copied()
            .min_by_key(|index| match graph.nodes[*index].key {
                NodeKey::Arena(_, slot) => (0, slot),
                NodeKey::Host(addr) => (1, addr),
            })
            .unwrap();
        let chain = cycle_chain(&graph, &set, start);
        if chain.is_empty() {
            continue;
        }
        cycles.push(LeakCycle {
            size: component.len(),
            suggested_weak_edge: suggest_weak_edge(&graph, &chain),
            chain: chain
                .iter()
                .map(|(node, edge)| ChainLink {
                    node: node_name(&graph.nodes[*node]),
                    edge: edge.clone(),
                })
                .collect(),
        });
    }
    let held_by_cycles = garbage.len() - in_cycles.len();
    let mut residual = Vec::new();
    let mut residual_total = 0;
    let mut baseline = 0;
    for (index, node) in graph.nodes.iter().enumerate() {
        if !matches!(node.key, NodeKey::Arena(..)) {
            continue;
        }
        if node.baseline {
            baseline += 1;
            continue;
        }
        if !reached[index] {
            continue;
        }
        residual_total += 1;
        if residual.len() >= RESIDUAL_LIMIT {
            continue;
        }
        let mut chain = vec![ChainLink {
            node: node_name(node),
            edge: String::new(),
        }];
        let mut cursor = index;
        let mut guard = 0;
        while let Some((from, edge)) = &parent[cursor] {
            chain.push(ChainLink {
                node: node_name(&graph.nodes[*from]),
                edge: edge.clone(),
            });
            cursor = *from;
            guard += 1;
            if guard > 64 {
                break;
            }
        }
        chain.reverse();
        residual.push(Residual {
            node: node_name(node),
            retainer_chain: chain,
            external_refs: external[cursor],
        });
    }
    LeakReport {
        live_objects: page_arena::live_objects(),
        live_arrays: page_arena::live_arrays(),
        live_functions: page_arena::live_functions(),
        baseline,
        cycles,
        held_by_cycles,
        residual,
        residual_total,
    }
}

/// Print a report if `W3COS_LEAK_REPORT` is set: `1`/`text` → stderr text,
/// `json` → stderr JSON, anything else is a file path appended to (JSON when
/// it ends in `.json`, text otherwise). Returns the report when one was made.
pub fn report_from_env(context: &str) -> Option<LeakReport> {
    let target = std::env::var("W3COS_LEAK_REPORT").ok()?;
    if target.is_empty() || target == "0" {
        return None;
    }
    let report = report();
    let text = || format!("[{context}] {}", report.to_text());
    let json = || serde_json::json!({ "context": context, "report": report.to_json() }).to_string();
    match target.as_str() {
        "1" | "text" => eprint!("{}", text()),
        "json" => eprintln!("{}", json()),
        path => {
            use std::io::Write;
            let body = if path.ends_with(".json") {
                json() + "\n"
            } else {
                text()
            };
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = file.write_all(body.as_bytes());
            }
        }
    }
    Some(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn object() -> Value {
        Value::object(HashMap::new())
    }

    #[test]
    fn reports_a_deliberate_cycle_with_its_chain() {
        page_arena::reset();
        mark_baseline();
        {
            let parent = object();
            let child = object();
            parent.set_property("child", child.clone());
            child.set_property("parent", parent.clone());
            parent.set_property("name", Value::string("p"));
        }
        let report = report();
        assert_eq!(report.cycles.len(), 1, "{}", report.to_text());
        let cycle = &report.cycles[0];
        assert_eq!(cycle.size, 2);
        let edges = cycle
            .chain
            .iter()
            .map(|link| link.edge.as_str())
            .collect::<Vec<_>>();
        assert!(
            edges.contains(&".child") && edges.contains(&".parent"),
            "{edges:?}"
        );
        assert!(
            cycle
                .chain
                .iter()
                .any(|link| link.node.contains("{child, name}"))
        );
        assert!(
            cycle.suggested_weak_edge.contains("--.parent-->"),
            "{}",
            cycle.suggested_weak_edge
        );
        assert_eq!(report.residual_total, 0, "a leaked cycle is not reachable");
        assert!(!report.is_clean());
        let json = report.to_json();
        assert_eq!(json["cycles"][0]["size"], 2);
        // Reporting never frees: the cycle is still live afterwards.
        assert_eq!(page_arena::live_objects(), 2);
        page_arena::reset();
    }

    #[test]
    fn values_held_only_by_a_cycle_are_counted_but_not_called_cycles() {
        page_arena::reset();
        mark_baseline();
        {
            let a = object();
            a.set_property("me", a.clone());
            a.set_property("payload", Value::array(vec![object(), object()]));
        }
        let report = report();
        assert_eq!(report.cycles.len(), 1, "{}", report.to_text());
        assert_eq!(report.cycles[0].size, 1, "self-loop");
        assert_eq!(report.held_by_cycles, 3, "array + its two objects");
        page_arena::reset();
    }

    #[test]
    fn acyclic_freed_values_are_not_reported() {
        page_arena::reset();
        mark_baseline();
        {
            let root = object();
            let items = Value::array((0..8).map(|_| object()).collect());
            root.set_property("items", items);
        }
        assert_eq!(page_arena::live_objects(), 0);
        let report = report();
        assert!(report.is_clean(), "{}", report.to_text());
        page_arena::reset();
    }

    #[test]
    fn baseline_values_are_labelled_and_new_live_values_are_residual_with_a_chain() {
        page_arena::reset();
        let realm = object();
        realm.set_property("config", object());
        mark_baseline();
        let report_before = report();
        assert!(report_before.is_clean(), "{}", report_before.to_text());
        assert_eq!(report_before.baseline, 2);
        let cache = object();
        realm.set_property("cache", cache.clone());
        cache.set_property("entry", Value::array(vec![object()]));
        drop(cache);
        let report = report();
        assert!(report.cycles.is_empty());
        assert_eq!(report.residual_total, 3, "{}", report.to_text());
        let deepest = report
            .residual
            .iter()
            .find(|residual| residual.retainer_chain.len() == 4)
            .expect("object inside cache.entry");
        let chain = deepest
            .retainer_chain
            .iter()
            .map(|link| link.edge.as_str())
            .collect::<Vec<_>>();
        assert_eq!(chain, vec![".cache", ".entry", "[0]", ""]);
        assert!(deepest.retainer_chain[0].node.contains("[realm]"));
        assert_eq!(deepest.external_refs, 1, "`realm` on the Rust stack");
        drop(realm);
        page_arena::reset();
    }
}
