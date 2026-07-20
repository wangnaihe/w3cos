//! Viewport-driven tile scheduling and bounded raster resource accounting.
//!
//! The manager is renderer-agnostic. Raster backends consume `TileRequest`s,
//! publish completed resources with `mark_ready`, and may keep presenting a
//! stale generation until the replacement is ready.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use crate::layout::LayoutRect;

pub const DEFAULT_TILE_SIZE: u32 = 256;

#[cfg(any(target_os = "ios", target_os = "android"))]
pub const DEFAULT_TILE_BUDGET_BYTES: usize = 48 * 1024 * 1024;

#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub const DEFAULT_TILE_BUDGET_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileId {
    pub x: i32,
    pub y: i32,
    pub scale_bucket: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TilePriority {
    Now,
    Soon,
    Eventually,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileState {
    Missing,
    Pending { generation: u64 },
    Ready { generation: u64 },
    Stale {
        ready_generation: u64,
        pending_generation: u64,
    },
}

#[derive(Clone, Debug)]
struct TileEntry {
    state: TileState,
    bytes: usize,
    last_used: u64,
    priority: TilePriority,
    clients: HashSet<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileRequest {
    pub id: TileId,
    pub priority: TilePriority,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TileStats {
    pub resident_tiles: usize,
    pub pending_tiles: usize,
    pub stale_tiles: usize,
    pub resident_bytes: usize,
    pub evictions: u64,
}

pub struct TileManager {
    tile_size: u32,
    budget_bytes: usize,
    entries: HashMap<TileId, TileEntry>,
    clock: u64,
    evictions: u64,
}

impl Default for TileManager {
    fn default() -> Self {
        Self::new(DEFAULT_TILE_SIZE, DEFAULT_TILE_BUDGET_BYTES)
    }
}

impl TileManager {
    pub fn new(tile_size: u32, budget_bytes: usize) -> Self {
        assert!(tile_size > 0);
        Self {
            tile_size,
            budget_bytes: budget_bytes.max(tile_size as usize * tile_size as usize * 4),
            entries: HashMap::new(),
            clock: 0,
            evictions: 0,
        }
    }

    pub fn prepare_frame(
        &mut self,
        generation: u64,
        viewport: LayoutRect,
        velocity_y: f32,
        scale_factor: f32,
        candidates: impl IntoIterator<Item = (usize, LayoutRect)>,
    ) -> Vec<TileRequest> {
        self.clock = self.clock.wrapping_add(1);
        let clock = self.clock;
        let tile_size = self.tile_size as f32;
        let lookahead = (viewport.height * 1.5 + velocity_y.abs() * 0.12)
            .clamp(viewport.height, viewport.height * 3.0);
        let interest = if velocity_y >= 0.0 {
            LayoutRect {
                x: viewport.x,
                y: viewport.y - 32.0,
                width: viewport.width,
                height: viewport.height + lookahead + 32.0,
            }
        } else {
            LayoutRect {
                x: viewport.x,
                y: viewport.y - lookahead,
                width: viewport.width,
                height: viewport.height + lookahead + 32.0,
            }
        };
        let scale_bucket = (scale_factor.max(0.25) * 64.0).round() as u16;
        let scaled_tile = (self.tile_size as f32 * scale_factor.max(0.25)).ceil() as usize;
        let tile_bytes = scaled_tile.saturating_mul(scaled_tile).saturating_mul(4);
        let mut touched = HashSet::new();

        for (client, rect) in candidates {
            let Some(visible) = intersection(rect, interest) else {
                continue;
            };
            let min_x = (visible.x / tile_size).floor() as i32;
            let max_x = ((visible.x + visible.width) / tile_size).ceil() as i32 - 1;
            let min_y = (visible.y / tile_size).floor() as i32;
            let max_y = ((visible.y + visible.height) / tile_size).ceil() as i32 - 1;
            for y in min_y..=max_y {
                for x in min_x..=max_x {
                    let id = TileId {
                        x,
                        y,
                        scale_bucket,
                    };
                    let tile_rect = LayoutRect {
                        x: x as f32 * tile_size,
                        y: y as f32 * tile_size,
                        width: tile_size,
                        height: tile_size,
                    };
                    let priority = if intersects(tile_rect, viewport) {
                        TilePriority::Now
                    } else {
                        TilePriority::Soon
                    };
                    let entry = self.entries.entry(id).or_insert_with(|| TileEntry {
                        state: TileState::Missing,
                        bytes: tile_bytes,
                        last_used: clock,
                        priority,
                        clients: HashSet::new(),
                    });
                    entry.last_used = clock;
                    entry.priority = entry.priority.min(priority);
                    entry.clients.insert(client);
                    entry.bytes = tile_bytes;
                    entry.state = match entry.state {
                        TileState::Ready {
                            generation: ready,
                        } if ready != generation => TileState::Stale {
                            ready_generation: ready,
                            pending_generation: generation,
                        },
                        TileState::Stale {
                            ready_generation,
                            pending_generation,
                        } if pending_generation != generation => TileState::Stale {
                            ready_generation,
                            pending_generation: generation,
                        },
                        TileState::Missing => TileState::Pending { generation },
                        state => state,
                    };
                    touched.insert(id);
                }
            }
        }

        for (id, entry) in &mut self.entries {
            if !touched.contains(id) {
                entry.priority = TilePriority::Eventually;
                entry.clients.clear();
            }
        }
        self.evict_to_budget(&touched);

        let mut requests: Vec<_> = touched
            .into_iter()
            .filter_map(|id| {
                let entry = self.entries.get(&id)?;
                matches!(
                    entry.state,
                    TileState::Pending { generation: pending }
                        | TileState::Stale {
                            pending_generation: pending,
                            ..
                        } if pending == generation
                )
                .then_some(TileRequest {
                    id,
                    priority: entry.priority,
                    generation,
                })
            })
            .collect();
        requests.sort_by(|a, b| {
            a.priority.cmp(&b.priority).then_with(|| {
                distance_to_viewport(a.id, viewport, self.tile_size).total_cmp(
                    &distance_to_viewport(b.id, viewport, self.tile_size),
                )
            })
        });
        requests
    }

    pub fn mark_ready(&mut self, id: TileId, generation: u64) -> bool {
        let Some(entry) = self.entries.get_mut(&id) else {
            return false;
        };
        let accepts = matches!(
            entry.state,
            TileState::Pending {
                generation: pending
            } | TileState::Stale {
                pending_generation: pending,
                ..
            } if pending == generation
        );
        if accepts {
            entry.state = TileState::Ready { generation };
        }
        accepts
    }

    pub fn has_presentable_tile(&self, id: TileId) -> bool {
        self.entries.get(&id).is_some_and(|entry| {
            matches!(
                entry.state,
                TileState::Ready { .. } | TileState::Stale { .. }
            )
        })
    }

    pub fn clients_for(&self, id: TileId) -> impl Iterator<Item = usize> + '_ {
        self.entries
            .get(&id)
            .into_iter()
            .flat_map(|entry| entry.clients.iter().copied())
    }

    pub fn stats(&self) -> TileStats {
        TileStats {
            resident_tiles: self.entries.len(),
            pending_tiles: self
                .entries
                .values()
                .filter(|entry| matches!(entry.state, TileState::Pending { .. }))
                .count(),
            stale_tiles: self
                .entries
                .values()
                .filter(|entry| matches!(entry.state, TileState::Stale { .. }))
                .count(),
            resident_bytes: self.entries.values().map(|entry| entry.bytes).sum(),
            evictions: self.evictions,
        }
    }

    fn evict_to_budget(&mut self, touched: &HashSet<TileId>) {
        let mut bytes: usize = self.entries.values().map(|entry| entry.bytes).sum();
        if bytes <= self.budget_bytes {
            return;
        }
        let mut victims: Vec<_> = self
            .entries
            .iter()
            .filter(|(id, _)| !touched.contains(id))
            .map(|(id, entry)| (*id, entry.priority, entry.last_used, entry.bytes))
            .collect();
        victims.sort_by(|a, b| match b.1.cmp(&a.1) {
            Ordering::Equal => a.2.cmp(&b.2),
            order => order,
        });
        for (id, _, _, entry_bytes) in victims {
            if bytes <= self.budget_bytes {
                break;
            }
            if self.entries.remove(&id).is_some() {
                bytes = bytes.saturating_sub(entry_bytes);
                self.evictions += 1;
            }
        }
    }
}

fn distance_to_viewport(id: TileId, viewport: LayoutRect, tile_size: u32) -> f32 {
    let tile_center_y = (id.y as f32 + 0.5) * tile_size as f32;
    let viewport_center_y = viewport.y + viewport.height * 0.5;
    (tile_center_y - viewport_center_y).abs()
}

fn intersects(a: LayoutRect, b: LayoutRect) -> bool {
    a.x < b.x + b.width
        && a.x + a.width > b.x
        && a.y < b.y + b.height
        && a.y + a.height > b.y
}

fn intersection(a: LayoutRect, b: LayoutRect) -> Option<LayoutRect> {
    let left = a.x.max(b.x);
    let top = a.y.max(b.y);
    let right = (a.x + a.width).min(b.x + b.width);
    let bottom = (a.y + a.height).min(b.y + b.height);
    (left < right && top < bottom).then_some(LayoutRect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(y: f32, height: f32) -> LayoutRect {
        LayoutRect {
            x: 0.0,
            y,
            width: 320.0,
            height,
        }
    }

    #[test]
    fn viewport_tiles_are_scheduled_before_lookahead() {
        let mut manager = TileManager::new(128, 8 * 1024 * 1024);
        let requests = manager.prepare_frame(
            1,
            rect(256.0, 256.0),
            1200.0,
            1.0,
            [(0, rect(0.0, 1024.0))],
        );
        assert!(!requests.is_empty());
        assert_eq!(requests[0].priority, TilePriority::Now);
        assert!(requests.iter().any(|request| request.priority == TilePriority::Soon));
    }

    #[test]
    fn ready_tile_survives_as_stale_until_replacement_finishes() {
        let mut manager = TileManager::new(256, 8 * 1024 * 1024);
        let viewport = rect(0.0, 256.0);
        let first = manager.prepare_frame(1, viewport, 0.0, 1.0, [(0, viewport)]);
        let id = first[0].id;
        assert!(manager.mark_ready(id, 1));
        let second = manager.prepare_frame(2, viewport, 0.0, 1.0, [(0, viewport)]);
        assert!(second.iter().any(|request| request.id == id));
        assert!(manager.has_presentable_tile(id));
    }

    #[test]
    fn offscreen_lru_tiles_are_evicted_to_hard_budget() {
        let tile_bytes = 64 * 64 * 4;
        let mut manager = TileManager::new(64, tile_bytes * 2);
        let narrow = |y| LayoutRect {
            x: 0.0,
            y,
            width: 64.0,
            height: 64.0,
        };
        let first = manager.prepare_frame(1, narrow(0.0), 0.0, 1.0, [(0, narrow(0.0))]);
        manager.mark_ready(first[0].id, 1);
        let second = manager.prepare_frame(
            2,
            narrow(256.0),
            0.0,
            1.0,
            [(1, narrow(256.0))],
        );
        manager.mark_ready(second[0].id, 2);
        let third = manager.prepare_frame(
            3,
            narrow(512.0),
            0.0,
            1.0,
            [(2, narrow(512.0))],
        );
        manager.mark_ready(third[0].id, 3);

        let stats = manager.stats();
        assert!(stats.resident_bytes <= tile_bytes * 2);
        assert!(stats.evictions >= 1);
    }
}
