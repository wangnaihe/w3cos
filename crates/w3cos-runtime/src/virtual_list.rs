//! Keyed, variable-height list virtualization primitives.
//!
//! The index stores only deviations from the estimated item height. It does
//! not allocate an entry per data item, so a very large logical list has a
//! small runtime footprint until rows are actually measured or mounted.

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VirtualListConfig {
    pub item_count: usize,
    pub estimated_item_height: f32,
    /// Extra logical pixels materialized before and after the viewport.
    pub overscan: f32,
}

impl VirtualListConfig {
    pub fn new(item_count: usize, estimated_item_height: f32, overscan: f32) -> Self {
        Self {
            item_count,
            estimated_item_height: estimated_item_height.max(1.0),
            overscan: overscan.max(0.0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VisibleWindow {
    pub start: usize,
    pub end: usize,
    pub before_extent: f32,
    pub visible_extent: f32,
    pub after_extent: f32,
}

impl VisibleWindow {
    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    pub fn len(self) -> usize {
        self.end.saturating_sub(self.start)
    }
}

#[derive(Debug, Clone)]
pub struct SparseHeightIndex {
    item_count: usize,
    estimate: f32,
    measured: HashMap<usize, f32>,
    /// Sparse Fenwick tree containing `(measured - estimate)` corrections.
    corrections: HashMap<usize, f32>,
    total_correction: f32,
}

impl SparseHeightIndex {
    pub fn new(item_count: usize, estimated_item_height: f32) -> Self {
        Self {
            item_count,
            estimate: estimated_item_height.max(1.0),
            measured: HashMap::new(),
            corrections: HashMap::new(),
            total_correction: 0.0,
        }
    }

    pub fn item_count(&self) -> usize {
        self.item_count
    }

    pub fn measured_count(&self) -> usize {
        self.measured.len()
    }

    pub fn total_extent(&self) -> f32 {
        self.item_count as f32 * self.estimate + self.total_correction
    }

    pub fn height_at(&self, index: usize) -> f32 {
        self.measured.get(&index).copied().unwrap_or(self.estimate)
    }

    /// Offset of the leading edge of `index`. `index == item_count` returns
    /// the total extent without visiting every item.
    pub fn offset_of(&self, index: usize) -> f32 {
        let index = index.min(self.item_count);
        index as f32 * self.estimate + self.prefix_correction(index)
    }

    /// Returns the item containing `offset`, in O(log item_count).
    pub fn index_at_offset(&self, offset: f32) -> usize {
        if self.item_count == 0 {
            return 0;
        }
        let target = offset.clamp(0.0, self.total_extent().max(0.0));
        let mut low = 0usize;
        let mut high = self.item_count;
        while low < high {
            let mid = low + (high - low) / 2;
            if self.offset_of(mid + 1) <= target {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        low.min(self.item_count - 1)
    }

    /// Records an actual row height and returns its change from the previous
    /// known height. Callers use the delta for scroll-anchor correction.
    pub fn measure(&mut self, index: usize, height: f32) -> f32 {
        if index >= self.item_count || !height.is_finite() || height <= 0.0 {
            return 0.0;
        }
        let previous = self.height_at(index);
        if (previous - height).abs() <= 0.01 {
            return 0.0;
        }
        let old_correction = previous - self.estimate;
        let new_correction = height - self.estimate;
        self.measured.insert(index, height);
        self.add_correction(index, new_correction - old_correction);
        self.total_correction += new_correction - old_correction;
        height - previous
    }

    pub fn resize(&mut self, item_count: usize) {
        if item_count == self.item_count {
            return;
        }
        self.item_count = item_count;
        self.measured.retain(|index, _| *index < item_count);
        self.rebuild_corrections();
    }

    pub fn visible_window(
        &self,
        scroll_offset: f32,
        viewport_extent: f32,
        overscan: f32,
    ) -> VisibleWindow {
        let total = self.total_extent();
        if self.item_count == 0 || viewport_extent <= 0.0 {
            return VisibleWindow {
                start: 0,
                end: 0,
                before_extent: 0.0,
                visible_extent: 0.0,
                after_extent: total,
            };
        }
        let start_offset = (scroll_offset - overscan.max(0.0)).clamp(0.0, total);
        let end_offset = (scroll_offset + viewport_extent + overscan.max(0.0)).clamp(0.0, total);
        let start = self.index_at_offset(start_offset);
        let end = if end_offset >= total {
            self.item_count
        } else {
            (self.index_at_offset(end_offset) + 1).min(self.item_count)
        };
        let before_extent = self.offset_of(start);
        let materialized_end = self.offset_of(end);
        VisibleWindow {
            start,
            end,
            before_extent,
            visible_extent: materialized_end - before_extent,
            after_extent: (total - materialized_end).max(0.0),
        }
    }

    fn prefix_correction(&self, end: usize) -> f32 {
        let mut cursor = end;
        let mut sum = 0.0;
        while cursor > 0 {
            sum += self.corrections.get(&cursor).copied().unwrap_or(0.0);
            cursor &= cursor - 1;
        }
        sum
    }

    fn add_correction(&mut self, index: usize, delta: f32) {
        let mut cursor = index + 1;
        while cursor <= self.item_count {
            let value = self.corrections.entry(cursor).or_default();
            *value += delta;
            if value.abs() <= f32::EPSILON {
                self.corrections.remove(&cursor);
            }
            cursor += cursor & cursor.wrapping_neg();
        }
    }

    fn rebuild_corrections(&mut self) {
        self.corrections.clear();
        self.total_correction = 0.0;
        let measured: Vec<(usize, f32)> = self
            .measured
            .iter()
            .map(|(&index, &height)| (index, height))
            .collect();
        for (index, height) in measured {
            let correction = height - self.estimate;
            self.add_correction(index, correction);
            self.total_correction += correction;
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScrollAnchor<K> {
    pub key: K,
    pub index: usize,
    /// Item leading-edge position relative to the viewport.
    pub viewport_offset: f32,
}

#[derive(Debug)]
pub struct CachedItemLayer<L> {
    pub generation: u64,
    pub raster: Option<L>,
    pub dirty: bool,
}

impl<L> Default for CachedItemLayer<L> {
    fn default() -> Self {
        Self {
            generation: 0,
            raster: None,
            dirty: true,
        }
    }
}

#[derive(Debug)]
pub struct MountedItem<K, V, L> {
    pub key: K,
    pub index: usize,
    pub node: V,
    pub layer: CachedItemLayer<L>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconcileStats {
    pub created: usize,
    pub reused: usize,
    pub retained: usize,
    pub recycled: usize,
}

/// Owns only the mounted window, a bounded node pool, measured heights and
/// explicitly saved offscreen state. Data remains owned by the application.
#[derive(Debug)]
pub struct KeyedVirtualList<K, V, S, L>
where
    K: Clone + Eq + Hash,
{
    config: VirtualListConfig,
    heights: SparseHeightIndex,
    mounted: BTreeMap<usize, MountedItem<K, V, L>>,
    saved_state: HashMap<K, S>,
    recycle_pool: Vec<V>,
    anchor: Option<ScrollAnchor<K>>,
    generation: u64,
}

impl<K, V, S, L> KeyedVirtualList<K, V, S, L>
where
    K: Clone + Eq + Hash,
{
    pub fn new(config: VirtualListConfig) -> Self {
        Self {
            heights: SparseHeightIndex::new(config.item_count, config.estimated_item_height),
            config,
            mounted: BTreeMap::new(),
            saved_state: HashMap::new(),
            recycle_pool: Vec::new(),
            anchor: None,
            generation: 0,
        }
    }

    pub fn total_extent(&self) -> f32 {
        self.heights.total_extent()
    }

    pub fn index_at_offset(&self, offset: f32) -> usize {
        self.heights.index_at_offset(offset)
    }

    pub fn offset_of(&self, index: usize) -> f32 {
        self.heights.offset_of(index)
    }

    pub fn mounted_len(&self) -> usize {
        self.mounted.len()
    }

    pub fn pooled_len(&self) -> usize {
        self.recycle_pool.len()
    }

    pub fn saved_state_len(&self) -> usize {
        self.saved_state.len()
    }

    pub fn measured_count(&self) -> usize {
        self.heights.measured_count()
    }

    pub fn mounted(&self) -> impl Iterator<Item = &MountedItem<K, V, L>> {
        self.mounted.values()
    }

    pub fn mounted_mut(&mut self) -> impl Iterator<Item = &mut MountedItem<K, V, L>> {
        self.mounted.values_mut()
    }

    pub fn visible_window(&self, scroll_offset: f32, viewport_extent: f32) -> VisibleWindow {
        self.heights
            .visible_window(scroll_offset, viewport_extent, self.config.overscan)
    }

    pub fn set_anchor(&mut self, key: K, index: usize, viewport_offset: f32) {
        self.anchor = Some(ScrollAnchor {
            key,
            index,
            viewport_offset,
        });
    }

    pub fn anchor(&self) -> Option<&ScrollAnchor<K>> {
        self.anchor.as_ref()
    }

    /// Returns the scroll delta needed to keep the anchor visually stationary.
    pub fn measure(&mut self, index: usize, height: f32) -> f32 {
        let delta = self.heights.measure(index, height);
        if self
            .anchor
            .as_ref()
            .is_some_and(|anchor| index < anchor.index)
        {
            delta
        } else {
            0.0
        }
    }

    pub fn resize(&mut self, item_count: usize) {
        self.config.item_count = item_count;
        self.heights.resize(item_count);
    }

    /// Reconciles only `window`. Key lookup and node creation are never called
    /// for offscreen indexes.
    #[allow(clippy::too_many_arguments)]
    pub fn reconcile<KeyAt, Create, Rebind, Capture, Restore>(
        &mut self,
        window: VisibleWindow,
        mut key_at: KeyAt,
        mut create: Create,
        mut rebind: Rebind,
        mut capture: Capture,
        mut restore: Restore,
    ) -> ReconcileStats
    where
        KeyAt: FnMut(usize) -> K,
        Create: FnMut(&K, usize) -> V,
        Rebind: FnMut(&mut V, &K, usize),
        Capture: FnMut(&V) -> Option<S>,
        Restore: FnMut(&mut V, &S),
    {
        let desired: Vec<(usize, K)> = (window.start..window.end)
            .map(|index| (index, key_at(index)))
            .collect();
        let desired_keys: HashMap<K, usize> = desired
            .iter()
            .map(|(index, key)| (key.clone(), *index))
            .collect();
        let old = std::mem::take(&mut self.mounted);
        let mut by_key: HashMap<K, MountedItem<K, V, L>> = HashMap::with_capacity(old.len());
        for (_, item) in old {
            by_key.insert(item.key.clone(), item);
        }

        let mut stats = ReconcileStats::default();
        for (key, item) in by_key.extract_if(|key, _| !desired_keys.contains_key(key)) {
            if let Some(state) = capture(&item.node) {
                self.saved_state.insert(key, state);
            }
            self.recycle_pool.push(item.node);
            stats.recycled += 1;
        }

        self.generation = self.generation.wrapping_add(1);
        for (index, key) in desired {
            if let Some(mut item) = by_key.remove(&key) {
                item.index = index;
                self.mounted.insert(index, item);
                stats.retained += 1;
                continue;
            }
            let (mut node, reused) = if let Some(mut node) = self.recycle_pool.pop() {
                rebind(&mut node, &key, index);
                (node, true)
            } else {
                (create(&key, index), false)
            };
            if let Some(state) = self.saved_state.get(&key) {
                restore(&mut node, state);
            }
            self.mounted.insert(
                index,
                MountedItem {
                    key,
                    index,
                    node,
                    layer: CachedItemLayer::default(),
                },
            );
            if reused {
                stats.reused += 1;
            } else {
                stats.created += 1;
            }
        }
        stats
    }

    pub fn mark_item_dirty(&mut self, key: &K) {
        if let Some(item) = self.mounted.values_mut().find(|item| &item.key == key) {
            item.layer.dirty = true;
            item.layer.raster = None;
        }
    }

    pub fn cache_item_layer(&mut self, key: &K, raster: L) {
        if let Some(item) = self.mounted.values_mut().find(|item| &item.key == key) {
            item.layer.generation = self.generation;
            item.layer.raster = Some(raster);
            item.layer.dirty = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn million_rows_need_no_per_item_metadata() {
        let index = SparseHeightIndex::new(1_000_000, 48.0);
        assert_eq!(index.total_extent(), 48_000_000.0);
        assert_eq!(index.measured_count(), 0);
        assert_eq!(index.index_at_offset(24_000_001.0), 500_000);
    }

    #[test]
    fn variable_heights_update_offsets_and_window() {
        let mut index = SparseHeightIndex::new(1000, 50.0);
        index.measure(2, 100.0);
        index.measure(4, 25.0);
        assert_eq!(index.offset_of(5), 275.0);
        assert_eq!(index.total_extent(), 50_025.0);
        let window = index.visible_window(250.0, 100.0, 50.0);
        assert_eq!((window.start, window.end), (3, 8));
    }

    #[test]
    fn reconcile_builds_only_window_and_reuses_nodes() {
        let mut list = KeyedVirtualList::<usize, String, String, Vec<u8>>::new(
            VirtualListConfig::new(1_000_000, 50.0, 100.0),
        );
        let first = list.visible_window(1000.0, 500.0);
        let mut created = 0;
        let stats = list.reconcile(
            first,
            |index| index,
            |_, index| {
                created += 1;
                format!("row-{index}")
            },
            |node, _, index| *node = format!("row-{index}"),
            |_| None,
            |_, _| {},
        );
        assert_eq!(stats.created, first.len());
        assert_eq!(created, first.len());
        assert!(created < 20);

        let second = list.visible_window(5000.0, 500.0);
        let stats = list.reconcile(
            second,
            |index| index,
            |_, index| format!("row-{index}"),
            |node, _, index| *node = format!("row-{index}"),
            |_| None,
            |_, _| {},
        );
        assert_eq!(stats.created, 0);
        assert_eq!(stats.reused, second.len());
        assert_eq!(list.mounted_len(), second.len());
    }

    #[test]
    fn offscreen_state_restores_by_key() {
        let mut list = KeyedVirtualList::<usize, String, String, ()>::new(VirtualListConfig::new(
            100, 50.0, 0.0,
        ));
        let first = list.visible_window(0.0, 50.0);
        list.reconcile(
            first,
            |index| index,
            |_, _| "draft".to_string(),
            |node, _, _| node.clear(),
            |node| Some(node.clone()),
            |node, state| node.clone_from(state),
        );
        let far = list.visible_window(1000.0, 50.0);
        list.reconcile(
            far,
            |index| index,
            |_, _| String::new(),
            |node, _, _| node.clear(),
            |node| Some(node.clone()),
            |node, state| node.clone_from(state),
        );
        list.reconcile(
            first,
            |index| index,
            |_, _| String::new(),
            |node, _, _| node.clear(),
            |node| Some(node.clone()),
            |node, state| node.clone_from(state),
        );
        assert_eq!(list.mounted().next().unwrap().node, "draft");
    }

    #[test]
    fn measuring_above_anchor_returns_scroll_correction() {
        let mut list =
            KeyedVirtualList::<usize, (), (), ()>::new(VirtualListConfig::new(1000, 50.0, 100.0));
        list.set_anchor(20, 20, 12.0);
        assert_eq!(list.measure(4, 80.0), 30.0);
        assert_eq!(list.measure(30, 80.0), 0.0);
    }

    #[test]
    fn cached_layer_is_invalidated_without_touching_other_items() {
        let mut list = KeyedVirtualList::<usize, (), (), &'static str>::new(
            VirtualListConfig::new(10, 50.0, 0.0),
        );
        let window = list.visible_window(0.0, 100.0);
        list.reconcile(
            window,
            |index| index,
            |_, _| (),
            |_, _, _| {},
            |_| None,
            |_, _| {},
        );
        list.cache_item_layer(&0, "raster-0");
        list.cache_item_layer(&1, "raster-1");
        list.mark_item_dirty(&0);
        let mut mounted = list.mounted();
        assert!(mounted.next().unwrap().layer.raster.is_none());
        assert_eq!(mounted.next().unwrap().layer.raster, Some("raster-1"));
    }
}
