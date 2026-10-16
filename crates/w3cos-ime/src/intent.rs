use serde::{Deserialize, Serialize};

/// How the person produced this intent. The hub does not interpret the payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modality {
    Key,
    Swipe,
    Handwriting,
    Voice,
    Gesture,
    Camera,
    Clipboard,
    Handoff,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IntentHint {
    Insert,
    ReplaceSelection,
    Command,
    Search,
}

/// Session-only payload. The hub does not persist it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum IntentPayload {
    Text { text: String },
    Keys { keys: Vec<String> },
    Ink { strokes: u32 },
    Audio { reference: String },
    Empty,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputIntent {
    pub modality: Modality,
    pub payload: IntentPayload,
    pub interim: bool,
    /// BCP 47 tags. Mixed input is a list, for example `zh-Hans` then `en`.
    pub locale: Vec<String>,
    pub hint: Option<IntentHint>,
}

impl InputIntent {
    pub fn text(modality: Modality, text: impl Into<String>, locale: &[&str]) -> Self {
        Self {
            modality,
            payload: IntentPayload::Text { text: text.into() },
            interim: false,
            locale: locale.iter().map(|tag| (*tag).to_string()).collect(),
            hint: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Purpose {
    Text,
    Chat,
    Email,
    Url,
    Number,
    Search,
    Command,
    Code,
    Password,
    Secure,
}

impl Purpose {
    pub fn is_secure(self) -> bool {
        matches!(self, Purpose::Password | Purpose::Secure)
    }

    /// Shell field role (`--w3cos-ime`) plus the secure-input flag.
    ///
    /// A secure flag always bypasses, even when the role string is ordinary.
    pub fn for_field(role: &str, secure: bool) -> Self {
        if secure {
            return if role == "password" {
                Purpose::Password
            } else {
                Purpose::Secure
            };
        }
        match role {
            "intent" | "command" => Purpose::Command,
            "terminal" | "editor" | "code" => Purpose::Code,
            "launcher" | "search" => Purpose::Search,
            "password" => Purpose::Password,
            "secure" => Purpose::Secure,
            _ => Purpose::Text,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scope {
    Purpose,
    Surrounding,
    Selection,
    Document,
    App,
    Screen,
    Personal,
}

impl Scope {
    pub fn as_protocol(self) -> &'static str {
        match self {
            Scope::Purpose => "ctx.purpose",
            Scope::Surrounding => "ctx.surrounding",
            Scope::Selection => "ctx.selection",
            Scope::Document => "ctx.document",
            Scope::App => "ctx.app",
            Scope::Screen => "ctx.screen",
            Scope::Personal => "ctx.personal",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub purpose: Purpose,
    pub locales: Vec<String>,
    pub granted: Vec<Scope>,
}

impl Session {
    pub fn allows(&self, scope: Scope) -> bool {
        self.granted.contains(&scope)
    }

    /// Drop everything the session was holding. The shell calls this on blur.
    pub fn end(&mut self) {
        self.id.clear();
        self.locales.clear();
        self.granted.clear();
    }
}

/// What an engine may offer. Action proposals always need a shell confirmation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Proposal {
    Preedit {
        text: String,
        cursor: usize,
    },
    Candidate {
        id: String,
        text: String,
        score: f32,
        source: String,
    },
    Completion {
        id: String,
        text: String,
    },
    Edit {
        id: String,
        edit: crate::Edit,
        source: String,
    },
    Action {
        id: String,
        /// Existing shell utterance, such as `open files` or `打开终端`.
        #[serde(rename = "action")]
        kind: String,
        requires_confirm: bool,
        source: String,
        /// Surface text for the chip, such as `文件`.
        #[serde(default)]
        summary: String,
    },
}

impl Proposal {
    pub fn candidate(
        id: impl Into<String>,
        text: impl Into<String>,
        score: f32,
        source: impl Into<String>,
    ) -> Self {
        Self::Candidate {
            id: id.into(),
            text: text.into(),
            score,
            source: source.into(),
        }
    }

    pub fn action(
        id: impl Into<String>,
        kind: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        let kind = kind.into();
        Self::Action {
            id: id.into(),
            summary: kind.clone(),
            kind,
            requires_confirm: true,
            source: source.into(),
        }
    }

    /// A shell command the person still has to confirm. `utterance` is an
    /// existing intent phrase, not a new action.
    pub fn command(
        id: impl Into<String>,
        utterance: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self::Action {
            id: id.into(),
            kind: utterance.into(),
            summary: summary.into(),
            requires_confirm: true,
            source: "L0".into(),
        }
    }
}
