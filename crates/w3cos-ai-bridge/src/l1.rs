//! Local L1 intent resolver.
//!
//! This runs only after the L0 rule table misses, and only from `propose`.
//! It is a deterministic synonym list plus a small edit-distance check.
//! [`UnimplementedLocalModel`] is the hook a future on-device classifier
//! would replace. It returns nothing and does not load a model file.

use crate::intent::{IntentResolver, IntentTier, ResolvedIntent, ShellApp, ShellCommand};

/// On-device classifier hook. The shipping stub never loads a GGUF model.
pub trait LocalClassifier {
    fn classify(&self, normalized: &str) -> Option<ShellCommand>;
}

/// Placeholder until a real local model exists.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnimplementedLocalModel;

impl LocalClassifier for UnimplementedLocalModel {
    fn classify(&self, _normalized: &str) -> Option<ShellCommand> {
        None
    }
}

/// Synonyms and typos the fixed rule table does not already accept.
#[derive(Debug, Clone)]
pub struct FuzzySynonymResolver<M: LocalClassifier = UnimplementedLocalModel> {
    model: M,
}

impl Default for FuzzySynonymResolver<UnimplementedLocalModel> {
    fn default() -> Self {
        Self::new()
    }
}

impl FuzzySynonymResolver<UnimplementedLocalModel> {
    pub fn new() -> Self {
        Self {
            model: UnimplementedLocalModel,
        }
    }
}

impl<M: LocalClassifier> FuzzySynonymResolver<M> {
    pub fn with_model(model: M) -> Self {
        Self { model }
    }
}

impl<M: LocalClassifier> IntentResolver for FuzzySynonymResolver<M> {
    fn resolve(&self, normalized: &str) -> Option<ResolvedIntent> {
        if normalized.is_empty() || normalized.contains('#') {
            return None;
        }
        let command = synonym_match(normalized)
            .or_else(|| fuzzy_match(normalized))
            .or_else(|| self.model.classify(normalized))?;
        Some(ResolvedIntent {
            command,
            tier: IntentTier::L1,
        })
    }
}

fn synonym_match(normalized: &str) -> Option<ShellCommand> {
    const SYNONYMS: &[(&str, ShellCommand)] = &[
        ("file manager", ShellCommand::OpenApp(ShellApp::Files)),
        ("file browser", ShellCommand::OpenApp(ShellApp::Files)),
        ("文件夹", ShellCommand::OpenApp(ShellApp::Files)),
        ("文件管理器", ShellCommand::OpenApp(ShellApp::Files)),
        ("command line", ShellCommand::OpenApp(ShellApp::Terminal)),
        ("控制台", ShellCommand::OpenApp(ShellApp::Terminal)),
        ("命令行", ShellCommand::OpenApp(ShellApp::Terminal)),
        ("preferences", ShellCommand::OpenApp(ShellApp::Settings)),
        ("偏好设置", ShellCommand::OpenApp(ShellApp::Settings)),
        ("系统偏好", ShellCommand::OpenApp(ShellApp::Settings)),
        ("dark theme", ShellCommand::ToggleNight),
        ("light mode", ShellCommand::ToggleNight),
        ("暗色模式", ShellCommand::ToggleNight),
        ("go home", ShellCommand::OpenApp(ShellApp::Desktop)),
        ("回主页", ShellCommand::OpenApp(ShellApp::Desktop)),
        ("主屏幕", ShellCommand::OpenApp(ShellApp::Desktop)),
        ("which apps", ShellCommand::ListApps),
        ("有什么应用", ShellCommand::ListApps),
        ("help me", ShellCommand::Help),
        ("怎么用", ShellCommand::Help),
    ];
    SYNONYMS
        .iter()
        .filter(|(phrase, _)| normalized.contains(phrase))
        .max_by_key(|(phrase, _)| phrase.len())
        .map(|(_, command)| *command)
}

fn fuzzy_match(normalized: &str) -> Option<ShellCommand> {
    const CANON: &[(&str, ShellCommand)] = &[
        ("open files", ShellCommand::OpenApp(ShellApp::Files)),
        ("open terminal", ShellCommand::OpenApp(ShellApp::Terminal)),
        ("open settings", ShellCommand::OpenApp(ShellApp::Settings)),
        ("night mode", ShellCommand::ToggleNight),
        ("dark mode", ShellCommand::ToggleNight),
        ("list apps", ShellCommand::ListApps),
        ("settings", ShellCommand::OpenApp(ShellApp::Settings)),
        ("terminal", ShellCommand::OpenApp(ShellApp::Terminal)),
        ("files", ShellCommand::OpenApp(ShellApp::Files)),
    ];
    let mut best: Option<(usize, ShellCommand)> = None;
    for (phrase, command) in CANON {
        let distance = edit_distance(normalized, phrase);
        let limit = if phrase.len() >= 8 { 2 } else { 1 };
        if phrase.len() >= 5 && distance > 0 && distance <= limit {
            match best {
                Some((best_distance, _)) if best_distance <= distance => {}
                _ => best = Some((distance, *command)),
            }
        }
    }
    best.map(|(_, command)| command)
}

fn edit_distance(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    if left.len() > 64 || right.len() > 64 {
        return usize::MAX;
    }
    let mut prev: Vec<usize> = (0..=right.len()).collect();
    let mut next = vec![0; right.len() + 1];
    for (i, left_char) in left.iter().enumerate() {
        next[0] = i + 1;
        for (j, right_char) in right.iter().enumerate() {
            let cost = usize::from(left_char != right_char);
            next[j + 1] = (prev[j + 1] + 1).min(next[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut next);
    }
    prev[right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::IntentResolver;

    #[test]
    fn synonyms_cover_phrases_the_rule_table_misses() {
        let resolver = FuzzySynonymResolver::new();
        let files = resolver.resolve("file manager").unwrap();
        assert_eq!(files.command, ShellCommand::OpenApp(ShellApp::Files));
        assert_eq!(files.tier, IntentTier::L1);
        assert_eq!(
            resolver.resolve("dark theme").unwrap().command,
            ShellCommand::ToggleNight
        );
        assert_eq!(
            resolver.resolve("preferences").unwrap().command,
            ShellCommand::OpenApp(ShellApp::Settings)
        );
        assert!(resolver.resolve("make me a sandwich").is_none());
        assert!(resolver.resolve("click #secret").is_none());
    }

    #[test]
    fn a_close_typo_is_l1_and_the_model_hook_is_silent() {
        let resolver = FuzzySynonymResolver::new();
        assert_eq!(
            resolver.resolve("settigns").unwrap().command,
            ShellCommand::OpenApp(ShellApp::Settings)
        );
        assert!(UnimplementedLocalModel.classify("file manager").is_none());
    }
}
