//! Floats, stage S3b: the exclusion space of a block formatting context.
//!
//! Ported in structure from Blink's NGExclusionSpace / NGLayoutOpportunity
//! and PositionFloat (third_party/blink/renderer/core/layout/
//! exclusions/exclusion_space.cc, floats_utils.cc, Copyright The Chromium
//! Authors, BSD-style license).  Coordinates are BFC offsets (NGBfcOffset):
//! relative to the border box of the formatting context root.  Floats are
//! placed by CSS2 9.5.1 (the highest band at or below the current offset,
//! and no higher than earlier floats, where the float's margin box fits
//! between the left floats' and the right floats' edges); clearance by
//! CSS2 9.5.2.

/// One float's margin box (NGExclusion).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Exclusion {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub is_left: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ExclusionSpace {
    exclusions: Vec<Exclusion>,
    /// CSS2 9.5.1 rule 5: a float's top is not above earlier floats' tops.
    last_float_top: f32,
}

/// What `snapshot` saves for a relayout (the space is append-only).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Snapshot(usize, f32);

impl ExclusionSpace {
    pub fn is_empty(&self) -> bool {
        self.exclusions.is_empty()
    }

    /// A float on the given side(s) has its margin box top at or below
    /// `top` (Blink's adjoining float types: floats placed at a BFC block
    /// offset that is not resolved yet).
    pub fn has_float_from(&self, top: f32, left: bool, right: bool) -> bool {
        self.exclusions
            .iter()
            .any(|e| ((e.is_left && left) || (!e.is_left && right)) && e.top >= top - 1.0 / 128.0)
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot(self.exclusions.len(), self.last_float_top)
    }

    pub fn restore(&mut self, s: Snapshot) {
        self.exclusions.truncate(s.0);
        self.last_float_top = s.1;
    }

    /// NGExclusionSpace::ClearanceOffset: the bottom of the lowest float
    /// on the cleared side(s).
    pub fn clearance_offset(&self, left: bool, right: bool) -> Option<f32> {
        self.exclusions
            .iter()
            .filter(|e| (e.is_left && left) || (!e.is_left && right))
            .map(|e| e.bottom)
            .reduce(f32::max)
    }

    /// The lowest float bottom (CSS2 10.6.7: a BFC root's auto height).
    pub fn max_bottom(&self) -> Option<f32> {
        self.exclusions.iter().map(|e| e.bottom).reduce(f32::max)
    }

    /// Some float's margin box intersects the rectangle (positive area).
    pub fn overlaps(&self, left: f32, top: f32, right: f32, bottom: f32) -> bool {
        self.exclusions.iter().any(|e| {
            e.left < right
                && e.right > left
                && e.top < bottom
                && e.bottom > top
                && e.right > e.left
                && e.bottom > e.top
        })
    }

    /// The band [top, top + height): the rightmost left-float margin edge
    /// and the leftmost right-float margin edge intersecting it
    /// (NGExclusionSpace::FindLayoutOpportunity for line boxes).
    pub fn band(&self, top: f32, height: f32) -> (Option<f32>, Option<f32>) {
        let mut left: Option<f32> = None;
        let mut right: Option<f32> = None;
        for e in &self.exclusions {
            if e.right <= e.left || e.bottom <= top || e.top >= top + height.max(1.0 / 64.0) {
                continue;
            }
            if e.is_left {
                left = Some(left.map_or(e.right, |l| l.max(e.right)));
            } else {
                right = Some(right.map_or(e.left, |r| r.min(e.left)));
            }
        }
        (left, right)
    }

    /// The nearest float bottom below `top` among floats intersecting the
    /// band: where a line moved past floats goes next.
    pub fn next_bottom(&self, top: f32, height: f32) -> Option<f32> {
        self.exclusions
            .iter()
            .filter(|e| e.right > e.left && e.bottom > top && e.top < top + height.max(1.0 / 64.0))
            .map(|e| e.bottom)
            .reduce(f32::min)
    }

    /// PositionFloat over NGLayoutOpportunity bands: the border-box offset
    /// of a float whose margin box is `width` x `height`, in a containing
    /// block spanning `start..end` inline, at or below `block_start`.
    /// Returns the margin-box top-left; the float's exclusion is added.
    pub fn place_float(
        &mut self,
        width: f32,
        height: f32,
        is_left: bool,
        start: f32,
        end: f32,
        block_start: f32,
    ) -> (f32, f32) {
        let top_min = block_start.max(self.last_float_top);
        let mut candidates: Vec<f32> = std::iter::once(top_min)
            .chain(
                self.exclusions
                    .iter()
                    .map(|e| e.bottom)
                    .filter(|&b| b > top_min),
            )
            .collect();
        candidates.sort_by(f32::total_cmp);
        candidates.dedup();
        // A zero-height float still occupies its offset.
        let band = height.max(1.0 / 64.0);
        let mut placed = (if is_left { start } else { end - width }, top_min);
        for &y in &candidates {
            // The float's own side is bounded by its containing block
            // (rules 1, 2); the far side only by opposite floats (rule 3,
            // even outside the containing block), and by the containing
            // block when a same-side float is beside it (rules 7, 8).
            let (mut near, mut far, mut same) = (if is_left { start } else { end }, None, false);
            for e in &self.exclusions {
                if !(e.top < y + band && e.bottom > y && e.right > e.left) {
                    continue;
                }
                if e.is_left == is_left {
                    same = true;
                    near = if is_left {
                        near.max(e.right)
                    } else {
                        near.min(e.left)
                    };
                } else {
                    let edge = if is_left { e.left } else { e.right };
                    far = Some(far.map_or(
                        edge,
                        |f: f32| if is_left { f.min(edge) } else { f.max(edge) },
                    ));
                }
            }
            if same {
                let cb = if is_left { end } else { start };
                far = Some(far.map_or(cb, |f| if is_left { f.min(cb) } else { f.max(cb) }));
            }
            // Signed: a negative margin-box width (negative margins) fits
            // even where the floats already overrun the band
            // (negative-margin-float-positioning).
            let room = far.map_or(f32::INFINITY, |f| if is_left { f - near } else { near - f });
            let fits = room >= width - 1.0 / 128.0;
            if fits {
                placed = (if is_left { near } else { near - width }, y);
                break;
            }
        }
        self.exclusions.push(Exclusion {
            left: placed.0,
            top: placed.1,
            right: placed.0 + width,
            bottom: placed.1 + height,
            is_left,
        });
        self.last_float_top = placed.1;
        placed
    }
}
