use crate::manifest::FuzzyAllowance;
use anyhow::{Result, bail};
use w3cos_runtime::headless::HeadlessFrame;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelDiff {
    pub max_difference: u8,
    pub different_pixels: u64,
    pub within_fuzzy: bool,
}

pub fn compare_frames(
    actual: &HeadlessFrame,
    expected: &HeadlessFrame,
    allowance: FuzzyAllowance,
) -> Result<(PixelDiff, Vec<u8>)> {
    if (actual.width, actual.height) != (expected.width, expected.height) {
        bail!(
            "reftest dimensions differ: actual={}x{}, expected={}x{}",
            actual.width,
            actual.height,
            expected.width,
            expected.height
        );
    }
    if actual.rgba.len() != expected.rgba.len() {
        bail!("reftest pixel buffers have inconsistent lengths");
    }

    let mut max_difference = 0_u8;
    let mut different_pixels = 0_u64;
    let mut diff = vec![0_u8; actual.rgba.len()];
    for ((actual, expected), output) in actual
        .rgba
        .chunks_exact(4)
        .zip(expected.rgba.chunks_exact(4))
        .zip(diff.chunks_exact_mut(4))
    {
        let pixel_difference = actual[..3]
            .iter()
            .zip(&expected[..3])
            .map(|(left, right)| left.abs_diff(*right))
            .max()
            .unwrap_or_default();
        max_difference = max_difference.max(pixel_difference);
        if pixel_difference > 0 {
            different_pixels += 1;
            output.copy_from_slice(&[255, 0, 0, 255]);
        } else {
            let gray =
                ((u16::from(actual[0]) + u16::from(actual[1]) + u16::from(actual[2])) / 3) as u8;
            output.copy_from_slice(&[gray, gray, gray, 96]);
        }
    }

    let within_fuzzy =
        max_difference <= allowance.max_difference && different_pixels <= allowance.total_pixels;
    Ok((
        PixelDiff {
            max_difference,
            different_pixels,
            within_fuzzy,
        },
        diff,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(pixels: &[[u8; 4]]) -> HeadlessFrame {
        HeadlessFrame {
            width: pixels.len() as u32,
            height: 1,
            rgba: pixels.iter().flatten().copied().collect(),
        }
    }

    #[test]
    fn enforces_both_wpt_fuzzy_dimensions() {
        let actual = frame(&[[10, 10, 10, 255], [20, 20, 20, 255]]);
        let expected = frame(&[[12, 10, 10, 255], [20, 20, 20, 255]]);
        let (strict, _) = compare_frames(&actual, &expected, FuzzyAllowance::default()).unwrap();
        assert_eq!(strict.max_difference, 2);
        assert_eq!(strict.different_pixels, 1);
        assert!(!strict.within_fuzzy);

        let (fuzzy, _) = compare_frames(
            &actual,
            &expected,
            FuzzyAllowance {
                max_difference: 2,
                total_pixels: 1,
            },
        )
        .unwrap();
        assert!(fuzzy.within_fuzzy);
    }

    #[test]
    fn rejects_dimension_mismatches() {
        let actual = frame(&[[0, 0, 0, 255]]);
        let expected = HeadlessFrame {
            width: 1,
            height: 2,
            rgba: vec![0; 8],
        };
        assert!(compare_frames(&actual, &expected, FuzzyAllowance::default()).is_err());
    }
}
