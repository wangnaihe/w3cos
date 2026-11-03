//! Per-segment build profiles for split bundles.
//!
//! A split bundle (`esm_codegen::SplitBundle`) is a workspace of generated
//! library crates. Debug builds compile every segment at `opt-level = 0`,
//! which keeps the build short but runs the JavaScript an order of magnitude
//! slower than it needs to. Most of the run time is spent in a few modules, so
//! only the segments that contain a hot module are built at a higher level.
//!
//! The hot set is a list of module paths, never segment indices: segment
//! numbers change whenever the bundle is regenerated, module paths do not.
//!
//! * [`HotModules::parse`] reads the list (one path suffix per line, `#`
//!   comments), e.g. `out/vs/base/common/strings.js`.
//! * [`HotModules::from_symbolized_stacks`] builds the list from profiler
//!   samples: one stack per line, frames separated by `;`, innermost frame
//!   first, each frame a demangled symbol such as
//!   `w3cos_seg_6::m189::m189_Foo__ctor`. A sample is charged to the module of
//!   its innermost generated frame.
//! * [`SplitBundle::profile_overrides`] emits the
//!   `[profile.<profile>.package.<segment>]` tables for the workspace
//!   `Cargo.toml`.

use crate::esm_codegen::{SplitBundle, SplitSegment};
use std::collections::HashMap;
use std::path::Path;

/// A set of module paths, matched by trailing path components.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HotModules {
    entries: Vec<String>,
}

fn normalize(path: &str) -> String {
    path.trim()
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_string()
}

impl HotModules {
    pub fn new(entries: impl IntoIterator<Item = String>) -> Self {
        let mut entries: Vec<String> = entries
            .into_iter()
            .map(|entry| normalize(&entry))
            .filter(|entry| !entry.is_empty())
            .collect();
        entries.sort();
        entries.dedup();
        Self { entries }
    }

    /// One path per line; blank lines and `#` comments are ignored.
    pub fn parse(text: &str) -> Self {
        Self::new(
            text.lines()
                .map(|line| line.split('#').next().unwrap_or("").to_string()),
        )
    }

    /// The list in the format [`HotModules::parse`] reads.
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        for entry in &self.entries {
            text.push_str(entry);
            text.push('\n');
        }
        text
    }

    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether `path` ends with an entry on a path-component boundary, so the
    /// list keeps working when the project is checked out somewhere else.
    pub fn matches(&self, path: &Path) -> bool {
        let path = normalize(&path.to_string_lossy());
        self.entries.iter().any(|entry| {
            path == *entry
                || path
                    .strip_suffix(entry.as_str())
                    .is_some_and(|rest| rest.ends_with('/'))
        })
    }

    /// Rank modules by the samples whose innermost generated frame is in them
    /// and keep the smallest prefix covering `fraction` (0.0..=1.0) of the
    /// attributed samples. Samples with no generated frame are ignored.
    pub fn from_symbolized_stacks(stacks: &str, bundle: &SplitBundle, fraction: f64) -> Self {
        let mut path_of: HashMap<&str, &Path> = HashMap::new();
        for segment in &bundle.segments {
            for (namespace, path) in segment.modules.iter().zip(&segment.module_paths) {
                path_of.insert(namespace.as_str(), path.as_path());
            }
        }
        let mut counts: HashMap<&str, u64> = HashMap::new();
        let mut total = 0u64;
        for line in stacks.lines() {
            let Some(namespace) = line.split(';').find_map(generated_namespace) else {
                continue;
            };
            if let Some((namespace, _)) = path_of.get_key_value(namespace) {
                *counts.entry(namespace).or_default() += 1;
                total += 1;
            }
        }
        let mut ranked: Vec<(&str, u64)> = counts.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let wanted = (total as f64 * fraction.clamp(0.0, 1.0)).ceil() as u64;
        let mut covered = 0u64;
        let mut chosen = Vec::new();
        for (namespace, count) in ranked {
            if covered >= wanted {
                break;
            }
            covered += count;
            chosen.push(path_of[namespace].to_string_lossy().into_owned());
        }
        Self::new(chosen)
    }
}

/// `w3cos_seg_6::m189::...` (also inside `<...>` and after a `.so+off`
/// annotation) -> `m189`.
fn generated_namespace(frame: &str) -> Option<&str> {
    let start = frame.find("w3cos_seg_")?;
    let rest = &frame[start..];
    let rest = &rest[rest.find("::")? + 2..];
    let end = rest.find("::")?;
    let namespace = &rest[..end];
    let digits = namespace.strip_prefix('m')?;
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then_some(namespace)
}

impl SplitSegment {
    /// Whether any module of this segment is in `hot`.
    pub fn is_hot(&self, hot: &HotModules) -> bool {
        self.module_paths.iter().any(|path| hot.matches(path))
    }
}

impl SplitBundle {
    /// Segments that contain at least one hot module.
    pub fn hot_segments<'a>(
        &'a self,
        hot: &'a HotModules,
    ) -> impl Iterator<Item = &'a SplitSegment> {
        self.segments
            .iter()
            .filter(move |segment| segment.is_hot(hot))
    }

    /// `[profile.<profile>.package.<segment>]` tables raising the hot
    /// segments to `opt_level`; append them to the workspace manifest.
    pub fn profile_overrides(&self, hot: &HotModules, profile: &str, opt_level: u8) -> String {
        let mut out = String::new();
        for segment in self.hot_segments(hot) {
            out.push_str(&format!(
                "\n[profile.{profile}.package.{}]\nopt-level = {opt_level}\n",
                segment.name
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn bundle() -> SplitBundle {
        let segment = |name: &str, modules: &[(&str, &str)]| SplitSegment {
            name: name.into(),
            deps: Vec::new(),
            code: String::new(),
            modules: modules.iter().map(|(ns, _)| ns.to_string()).collect(),
            module_paths: modules.iter().map(|(_, p)| PathBuf::from(p)).collect(),
        };
        SplitBundle {
            segments: vec![
                segment(
                    "w3cos_seg_0",
                    &[
                        ("m0", "/src/out/vs/base/a.js"),
                        ("m1", "/src/out/vs/base/b.js"),
                    ],
                ),
                segment("w3cos_seg_1", &[("m2", "/src/out/vs/editor/c.js")]),
                segment("w3cos_seg_2", &[("m3", "/src/out/vs/editor/ba.js")]),
            ],
            root: String::new(),
        }
    }

    #[test]
    fn parse_ignores_comments_blanks_and_dot_prefix() {
        let hot =
            HotModules::parse("# header\n./out/vs/base/a.js  # inline\n\n out/vs/editor/c.js \n");
        assert_eq!(hot.entries(), ["out/vs/base/a.js", "out/vs/editor/c.js"]);
        assert_eq!(HotModules::parse(&hot.to_text()), hot);
    }

    #[test]
    fn matching_is_by_whole_trailing_components() {
        let hot = HotModules::parse("vs/base/a.js\n");
        assert!(hot.matches(Path::new("/src/out/vs/base/a.js")));
        assert!(hot.matches(Path::new("/other/checkout/out/vs/base/a.js")));
        assert!(hot.matches(Path::new("vs/base/a.js")));
        assert!(!hot.matches(Path::new("/src/out/vs/base/ba.js")));
        assert!(!hot.matches(Path::new("/src/out/xvs/base/a.js")));
        assert!(!HotModules::default().matches(Path::new("/src/out/vs/base/a.js")));
    }

    #[test]
    fn overrides_cover_exactly_the_segments_with_hot_modules() {
        let bundle = bundle();
        let hot = HotModules::parse("out/vs/base/b.js\nout/vs/editor/c.js\n");
        let toml = bundle.profile_overrides(&hot, "dev", 1);
        assert_eq!(
            toml,
            "\n[profile.dev.package.w3cos_seg_0]\nopt-level = 1\n\n[profile.dev.package.w3cos_seg_1]\nopt-level = 1\n"
        );
        assert!(
            bundle
                .profile_overrides(&HotModules::default(), "dev", 1)
                .is_empty()
        );
    }

    #[test]
    fn the_list_is_stable_when_segment_numbers_change() {
        let mut regenerated = bundle();
        regenerated.segments.reverse();
        for (index, segment) in regenerated.segments.iter_mut().enumerate() {
            segment.name = format!("w3cos_seg_{index}");
        }
        let hot = HotModules::parse("out/vs/editor/c.js\n");
        let names = |bundle: &SplitBundle| -> Vec<String> {
            bundle
                .hot_segments(&hot)
                .map(|s| s.modules.join(","))
                .collect()
        };
        assert_eq!(names(&bundle()), names(&regenerated));
        assert!(
            regenerated
                .profile_overrides(&hot, "dev", 1)
                .contains("w3cos_seg_1]")
        );
    }

    #[test]
    fn stacks_are_charged_to_the_innermost_generated_module() {
        let bundle = bundle();
        let stacks = "\
libc.so+1f;w3cos_seg_1::m2::m2_f;w3cos_seg_0::m0::m0_g\n\
w3cos_seg_1::m2::m2_h\n\
<w3cos_seg_1::m2::M2FooFrame>::run;w3cos_seg_0::m1::x\n\
w3cos_seg_0::m0::m0_g\n\
libw3cos_base.so+10;libc.so+2\n\
w3cos_seg_9::m77::unknown_module\n";
        // m2 x3, m0 x1, nothing for the unknown or generated-free stacks.
        let top = HotModules::from_symbolized_stacks(stacks, &bundle, 0.5);
        assert_eq!(top.entries(), ["/src/out/vs/editor/c.js"]);
        let all = HotModules::from_symbolized_stacks(stacks, &bundle, 1.0);
        assert_eq!(
            all.entries(),
            ["/src/out/vs/base/a.js", "/src/out/vs/editor/c.js"]
        );
        assert!(HotModules::from_symbolized_stacks(stacks, &bundle, 0.0).is_empty());
        // The round trip drives the same segments.
        let segments: Vec<&str> = bundle.hot_segments(&top).map(|s| s.name.as_str()).collect();
        assert_eq!(segments, ["w3cos_seg_1"]);
    }

    #[test]
    fn generated_namespace_needs_a_module_frame() {
        assert_eq!(generated_namespace("w3cos_seg_6::m189::f"), Some("m189"));
        assert_eq!(
            generated_namespace("<w3cos_seg_6::m2::Frame>::run"),
            Some("m2")
        );
        assert_eq!(generated_namespace("libw3cos_seg_6.so+5a"), None);
        assert_eq!(generated_namespace("w3cos_seg_6::mx::f"), None);
        assert_eq!(generated_namespace("w3cos_seg_6::m::f"), None);
        assert_eq!(generated_namespace("w3cos_core::m1::f"), None);
    }
}
