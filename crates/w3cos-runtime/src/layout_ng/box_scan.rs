//! Box-tree mode (`W3COS_NG_BOX_TREE=1`): style-derived scans of the flat
//! tree kept across the patch flushes of one structure. Blink keeps such
//! per-object bits on its LayoutObjects and updates them on style change;
//! here each scan keeps the indices it selects and re-evaluates only the
//! boxes a restyle can affect (the restyled boxes, the host-less boxes
//! lowered below them, their parents and their children: every scan reads
//! at most a box, its parent, and its children).
use std::cell::RefCell;

use crate::layout::FlatNodeInfo;

#[derive(Clone, Copy)]
pub(super) enum Scan {
    Hidden = 0,
    EmptyLeaf,
    Balanced,
    Logical,
    RunIn,
}

struct ScanMemo {
    key: (u64, u64),
    len: usize,
    seq: u64,
    hits: Vec<usize>,
}

struct BoxIndex {
    key: (u64, u64),
    len: usize,
    host: Vec<Option<u32>>,
}

thread_local! {
    /// In-place restyles: (sequence number of the first entry, ids).
    static RESTYLE_LOG: RefCell<(u64, Vec<u32>)> = const { RefCell::new((0, Vec::new())) };
    static BOX_INDEX: RefCell<Option<BoxIndex>> = const { RefCell::new(None) };
    static SCANS: RefCell<[Option<ScanMemo>; 5]> = const { RefCell::new([None, None, None, None, None]) };
}

pub(super) fn note_restyled(id: u32) {
    RESTYLE_LOG.with(|log| {
        let mut log = log.borrow_mut();
        if log.1.len() >= 4096 {
            log.0 += log.1.len() as u64;
            log.1.clear();
        }
        log.1.push(id);
    });
}

pub(super) fn forget() {
    SCANS.with(|scans| *scans.borrow_mut() = Default::default());
    BOX_INDEX.with(|index| *index.borrow_mut() = None);
}

fn log_seq() -> u64 {
    RESTYLE_LOG.with(|log| {
        let log = log.borrow();
        log.0 + log.1.len() as u64
    })
}

fn log_since(seq: u64) -> Option<Vec<u32>> {
    RESTYLE_LOG.with(|log| {
        let log = log.borrow();
        (seq >= log.0).then(|| log.1[(seq - log.0) as usize..].to_vec())
    })
}

fn host_id(node: &FlatNodeInfo<'_>) -> Option<u32> {
    match node.on_click {
        w3cos_std::EventAction::NativeHost { id, .. } => u32::try_from(*id).ok(),
        _ => None,
    }
}

fn dirty_indices(flat: &[FlatNodeInfo<'_>], key: (u64, u64), ids: &[u32]) -> Vec<usize> {
    BOX_INDEX.with(|slot| {
        let mut slot = slot.borrow_mut();
        if !slot
            .as_ref()
            .is_some_and(|index| index.key == key && index.len == flat.len())
        {
            *slot = Some(BoxIndex {
                key,
                len: flat.len(),
                host: flat.iter().map(host_id).collect(),
            });
        }
        let host = &slot.as_ref().unwrap().host;
        let mut boxes = Vec::new();
        for index in 0..flat.len() {
            if !host[index].is_some_and(|id| ids.contains(&id)) {
                continue;
            }
            boxes.push(index);
            let end = flat[index].subtree_end.max(index + 1).min(flat.len());
            let mut child = index + 1;
            while child < end {
                if host[child].is_some() {
                    child = flat[child].subtree_end.max(child + 1);
                } else {
                    boxes.push(child);
                    child += 1;
                }
            }
        }
        let mut dirty = Vec::with_capacity(boxes.len() * 4);
        for &index in &boxes {
            dirty.push(index);
            if let Some(parent) = flat[index].parent {
                dirty.push(parent);
            }
            let end = flat[index].subtree_end.max(index + 1).min(flat.len());
            let mut child = index + 1;
            while child < end {
                dirty.push(child);
                child = flat[child].subtree_end.max(child + 1);
            }
        }
        dirty.sort_unstable();
        dirty.dedup();
        dirty
    })
}

/// The indices `pred` selects, in order; `None` outside a box-tree patch
/// flush over `flat` (the caller scans every box).
pub(super) fn retained(
    scan: Scan,
    flat: &[FlatNodeInfo<'_>],
    pred: impl Fn(usize) -> bool,
) -> Option<Vec<usize>> {
    let key = super::box_tree_key(flat)?;
    let seq = log_seq();
    let memo = SCANS
        .with(|scans| scans.borrow_mut()[scan as usize].take())
        .filter(|memo| memo.key == key && memo.len == flat.len());
    let refreshed = memo.and_then(|memo| {
        let ids = log_since(memo.seq).filter(|ids| ids.len() <= 8)?;
        let mut hits = memo.hits;
        if !ids.is_empty() {
            for index in dirty_indices(flat, key, &ids) {
                let now = pred(index);
                match hits.binary_search(&index) {
                    Ok(at) if !now => {
                        hits.remove(at);
                    }
                    Err(at) if now => hits.insert(at, index),
                    _ => {}
                }
            }
        }
        Some(hits)
    });
    let hits = refreshed.unwrap_or_else(|| (0..flat.len()).filter(|&index| pred(index)).collect());
    SCANS.with(|scans| {
        scans.borrow_mut()[scan as usize] = Some(ScanMemo {
            key,
            len: flat.len(),
            seq,
            hits: hits.clone(),
        })
    });
    Some(hits)
}
