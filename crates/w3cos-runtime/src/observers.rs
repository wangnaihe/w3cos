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

// --- MutationObserver types ---

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationType {
    ChildList,
    Attributes,
    CharacterData,
}

pub struct MutationRecord {
    pub mutation_type: MutationType,
    pub target: NodeId,
    pub added_nodes: Vec<NodeId>,
    pub removed_nodes: Vec<NodeId>,
    pub attribute_name: Option<String>,
    pub old_value: Option<String>,
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
                let crossed = self.thresholds.iter().any(|&t| {
                    (last < t && ratio >= t) || (last >= t && ratio < t)
                });
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
