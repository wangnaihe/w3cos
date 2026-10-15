//! L0 command phrases the shell already knows how to propose.
//!
//! Each hit returns an utterance `IntentSession::propose` already accepts.
//! Nothing here is a new shell action, and nothing here executes.

/// Longest containing phrase wins. The second value is the utterance to propose.
const CONTAINS: &[(&str, &str)] = &[
    ("open files", "open files"),
    ("打开文件", "open files"),
    ("文件管理", "open files"),
    ("open terminal", "打开终端"),
    ("打开终端", "打开终端"),
    ("open settings", "打开设置"),
    ("打开设置", "打开设置"),
    ("系统设置", "打开设置"),
    ("open browser", "open browser"),
    ("打开浏览器", "open browser"),
    ("open editor", "open editor"),
    ("打开编辑器", "open editor"),
    ("open agent", "ai agent"),
    ("ai agent", "ai agent"),
    ("打开助手", "打开助手"),
    ("回到桌面", "回到桌面"),
    ("返回桌面", "返回桌面"),
    ("show desktop", "show desktop"),
    ("night mode", "night mode"),
    ("dark mode", "night mode"),
    ("夜间模式", "night mode"),
    ("list apps", "list apps"),
    ("有哪些应用", "list apps"),
    ("应用列表", "list apps"),
    ("where am i", "where am i"),
    ("current app", "where am i"),
    ("我在哪", "where am i"),
    ("当前应用", "where am i"),
    ("命令帮助", "命令帮助"),
    ("show help", "show help"),
];

const EXACT: &[(&str, &str)] = &[
    ("files", "open files"),
    ("文件", "open files"),
    ("terminal", "打开终端"),
    ("终端", "打开终端"),
    ("settings", "打开设置"),
    ("设置", "打开设置"),
    ("browser", "open browser"),
    ("浏览器", "open browser"),
    ("editor", "open editor"),
    ("编辑器", "open editor"),
    ("desktop", "show desktop"),
    ("home", "show desktop"),
    ("桌面", "show desktop"),
    ("apps", "list apps"),
    ("context", "where am i"),
    ("help", "show help"),
    ("?", "show help"),
    ("帮助", "帮助"),
    ("night", "night mode"),
    ("夜间", "night mode"),
];

fn normalize(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Utterance to hand to the shell confirm path, when `text` is an existing command.
pub fn command_utterance(text: &str) -> Option<&'static str> {
    let normalized = normalize(text);
    if normalized.is_empty() {
        return None;
    }
    let mut best: Option<(&str, &str)> = None;
    for (phrase, utterance) in CONTAINS {
        if normalized.contains(phrase)
            && best
                .map(|(current, _)| phrase.len() > current.len())
                .unwrap_or(true)
        {
            best = Some((phrase, utterance));
        }
    }
    if let Some((_, utterance)) = best {
        return Some(utterance);
    }
    EXACT
        .iter()
        .find(|(phrase, _)| *phrase == normalized)
        .map(|(_, utterance)| *utterance)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_shell_phrases_map_to_existing_utterances() {
        assert_eq!(command_utterance("文件"), Some("open files"));
        assert_eq!(command_utterance("打开文件"), Some("open files"));
        assert_eq!(command_utterance("Open Files"), Some("open files"));
        assert_eq!(command_utterance("终端"), Some("打开终端"));
        assert_eq!(command_utterance("设置"), Some("打开设置"));
        assert_eq!(command_utterance("夜间模式"), Some("night mode"));
        assert_eq!(command_utterance("night mode"), Some("night mode"));
        assert_eq!(command_utterance("list apps"), Some("list apps"));
        assert_eq!(command_utterance("where am i"), Some("where am i"));
        assert_eq!(command_utterance("你好"), None);
        assert_eq!(command_utterance("wenjian"), None);
        assert_eq!(command_utterance(""), None);
    }
}
