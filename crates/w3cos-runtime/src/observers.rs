use w3cos_dom::node::NodeId;

// --- ResizeObserver ---

pub struct ResizeObserverEntry {
    pub target: NodeId,
    pub content_width: f32,
    pub content_height: f32,
    pub border_box_width: f32,
    pub border_box_height: f32,
}

pub struct ResizeObserver {
    targets: Vec<NodeId>,
    last_sizes: Vec<(f32, f32)>,
}

impl ResizeObserver {
    pub fn new() -> Self {
        Self {
            targets: Vec::new(),
            last_sizes: Vec::new(),
        }
    }

    pub fn observe(&mut self, target: NodeId) {
        self.targets.push(target);
        self.last_sizes.push((0.0, 0.0));
    }

    pub fn unobserve(&mut self, target: NodeId) {
        if let Some(i) = self.targets.iter().position(|t| *t == target) {
            self.targets.remove(i);
            self.last_sizes.remove(i);
        }
    }

    pub fn disconnect(&mut self) {
        self.targets.clear();
        self.last_sizes.clear();
    }

    /// Check current sizes against last recorded sizes.
    /// Returns entries for any targets whose size has changed, and updates internal state.
    pub fn check_for_changes(&mut self, sizes: &[(NodeId, f32, f32)]) -> Vec<ResizeObserverEntry> {
        let mut entries = Vec::new();
        for (i, target) in self.targets.iter().enumerate() {
            if let Some(&(_, w, h)) = sizes.iter().find(|(id, _, _)| id == target) {
                let (last_w, last_h) = self.last_sizes[i];
                if (w - last_w).abs() > f32::EPSILON || (h - last_h).abs() > f32::EPSILON {
                    self.last_sizes[i] = (w, h);
                    entries.push(ResizeObserverEntry {
                        target: *target,
                        content_width: w,
                        content_height: h,
                        border_box_width: w,
                        border_box_height: h,
                    });
                }
            }
        }
        entries
    }
}

impl Default for ResizeObserver {
    fn default() -> Self {
        Self::new()
    }
}

// --- MutationObserver ---

/// W3C `MutationObserverInit` — configuration for `MutationObserver.observe()`.
/// https://dom.spec.whatwg.org/#dictdef-mutationobserverinit
#[derive(Debug, Clone, Default)]
pub struct MutationObserverInit {
    /// Observe child node additions/removals.
    pub child_list: bool,
    /// Observe attribute changes.
    pub attributes: bool,
    /// Observe text content changes.
    pub character_data: bool,
    /// Extend observation to all descendants.
    pub subtree: bool,
    /// Record old attribute value before change.
    pub attribute_old_value: bool,
    /// Record old character data before change.
    pub character_data_old_value: bool,
    /// Limit attribute observation to these names (None = all attributes).
    pub attribute_filter: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationType {
    ChildList,
    Attributes,
    CharacterData,
}

/// W3C `MutationRecord` — describes a single DOM mutation.
#[derive(Debug, Clone)]
pub struct MutationRecord {
    pub mutation_type: MutationType,
    pub target: NodeId,
    pub added_nodes: Vec<NodeId>,
    pub removed_nodes: Vec<NodeId>,
    pub previous_sibling: Option<NodeId>,
    pub next_sibling: Option<NodeId>,
    pub attribute_name: Option<String>,
    pub attribute_namespace: Option<String>,
    pub old_value: Option<String>,
}

impl MutationRecord {
    pub fn child_list(target: NodeId, added: Vec<NodeId>, removed: Vec<NodeId>) -> Self {
        Self {
            mutation_type: MutationType::ChildList,
            target,
            added_nodes: added,
            removed_nodes: removed,
            previous_sibling: None,
            next_sibling: None,
            attribute_name: None,
            attribute_namespace: None,
            old_value: None,
        }
    }

    pub fn attributes(target: NodeId, name: &str, old_value: Option<String>) -> Self {
        Self {
            mutation_type: MutationType::Attributes,
            target,
            added_nodes: Vec::new(),
            removed_nodes: Vec::new(),
            previous_sibling: None,
            next_sibling: None,
            attribute_name: Some(name.to_string()),
            attribute_namespace: None,
            old_value,
        }
    }

    pub fn character_data(target: NodeId, old_value: Option<String>) -> Self {
        Self {
            mutation_type: MutationType::CharacterData,
            target,
            added_nodes: Vec::new(),
            removed_nodes: Vec::new(),
            previous_sibling: None,
            next_sibling: None,
            attribute_name: None,
            attribute_namespace: None,
            old_value,
        }
    }
}

/// W3C `MutationObserver` — observes DOM mutations and delivers them via callback.
/// https://dom.spec.whatwg.org/#interface-mutationobserver
///
/// Usage in w3cos:
/// 1. Create with `MutationObserver::new(callback)`.
/// 2. Call `observe(target, init)` to register targets.
/// 3. The runtime calls `deliver(records)` after each DOM mutation batch.
/// 4. Call `disconnect()` to stop all observations.
/// 5. Call `take_records()` to drain the pending queue without firing callback.
pub struct MutationObserver {
    callback: Box<dyn FnMut(Vec<MutationRecord>)>,
    observations: Vec<(NodeId, MutationObserverInit)>,
    record_queue: Vec<MutationRecord>,
}

impl MutationObserver {
    pub fn new(callback: impl FnMut(Vec<MutationRecord>) + 'static) -> Self {
        Self {
            callback: Box::new(callback),
            observations: Vec::new(),
            record_queue: Vec::new(),
        }
    }

    /// Register a target node with the given observation options.
    /// Calling `observe` on an already-observed target replaces its options.
    pub fn observe(&mut self, target: NodeId, init: MutationObserverInit) {
        if let Some(entry) = self.observations.iter_mut().find(|(id, _)| *id == target) {
            entry.1 = init;
        } else {
            self.observations.push((target, init));
        }
    }

    /// Stop observing all targets and clear the record queue.
    pub fn disconnect(&mut self) {
        self.observations.clear();
        self.record_queue.clear();
    }

    /// Drain and return all queued records without invoking the callback.
    pub fn take_records(&mut self) -> Vec<MutationRecord> {
        std::mem::take(&mut self.record_queue)
    }

    /// Returns the list of currently observed (target, init) pairs.
    pub fn observations(&self) -> &[(NodeId, MutationObserverInit)] {
        &self.observations
    }

    /// Queue a mutation record if it matches any active observation.
    /// Called by the DOM runtime after each mutation.
    pub fn queue_mutation(&mut self, record: MutationRecord) {
        let matches = self.observations.iter().any(|(target, init)| {
            if *target != record.target {
                // Check subtree: if subtree is set we'd need ancestor check;
                // for now accept direct target matches (subtree handled by runtime).
                return false;
            }
            match record.mutation_type {
                MutationType::ChildList => init.child_list,
                MutationType::Attributes => {
                    if !init.attributes {
                        return false;
                    }
                    if let Some(filter) = &init.attribute_filter {
                        if let Some(name) = &record.attribute_name {
                            return filter.iter().any(|f| f == name);
                        }
                        return false;
                    }
                    true
                }
                MutationType::CharacterData => init.character_data,
            }
        });
        if matches {
            self.record_queue.push(record);
        }
    }

    /// Deliver all queued records to the callback and clear the queue.
    /// The runtime should call this at the end of each microtask checkpoint.
    pub fn deliver(&mut self) {
        if self.record_queue.is_empty() {
            return;
        }
        let records = std::mem::take(&mut self.record_queue);
        (self.callback)(records);
    }
}

// --- IntersectionObserver ---

pub struct IntersectionObserverEntry {
    pub target: NodeId,
    pub intersection_ratio: f64,
    pub is_intersecting: bool,
    pub bounding_client_rect: (f32, f32, f32, f32),
}

pub struct IntersectionObserver {
    targets: Vec<NodeId>,
    root: Option<NodeId>,
    thresholds: Vec<f64>,
    last_ratios: Vec<f64>,
}

impl IntersectionObserver {
    pub fn new(root: Option<NodeId>, thresholds: Vec<f64>) -> Self {
        Self {
            targets: Vec::new(),
            root,
            thresholds: if thresholds.is_empty() {
                vec![0.0]
            } else {
                thresholds
            },
            last_ratios: Vec::new(),
        }
    }

    pub fn observe(&mut self, target: NodeId) {
        self.targets.push(target);
        self.last_ratios.push(-1.0);
    }

    pub fn unobserve(&mut self, target: NodeId) {
        if let Some(i) = self.targets.iter().position(|t| *t == target) {
            self.targets.remove(i);
            self.last_ratios.remove(i);
        }
    }

    pub fn disconnect(&mut self) {
        self.targets.clear();
        self.last_ratios.clear();
    }

    pub fn root(&self) -> Option<NodeId> {
        self.root
    }

    pub fn thresholds(&self) -> &[f64] {
        &self.thresholds
    }

    /// Check intersection ratios against last recorded values.
    /// `ratios` maps each observed target to its current intersection ratio and bounding rect.
    /// Returns entries for targets that crossed a threshold boundary.
    pub fn check_for_intersections(
        &mut self,
        ratios: &[(NodeId, f64, (f32, f32, f32, f32))],
    ) -> Vec<IntersectionObserverEntry> {
        let mut entries = Vec::new();
        for (i, target) in self.targets.iter().enumerate() {
            if let Some(&(_, ratio, rect)) = ratios.iter().find(|(id, _, _)| id == target) {
                let last = self.last_ratios[i];
                let crossed = self
                    .thresholds
                    .iter()
                    .any(|&t| (last < t && ratio >= t) || (last >= t && ratio < t));
                if crossed || last < 0.0 {
                    self.last_ratios[i] = ratio;
                    entries.push(IntersectionObserverEntry {
                        target: *target,
                        intersection_ratio: ratio,
                        is_intersecting: ratio > 0.0,
                        bounding_client_rect: rect,
                    });
                }
            }
        }
        entries
    }
}
