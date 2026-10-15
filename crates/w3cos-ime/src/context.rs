use crate::intent::Purpose;

/// Characters of surrounding text a local engine may see without a further grant.
pub const SURROUNDING_CAP: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Default for Rect {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Surrounding {
    pub before: String,
    pub after: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    Insert { text: String },
    Replace { range: TextRange, text: String },
    Preedit { text: String, cursor: usize },
}

pub trait ImeContext {
    fn purpose(&self) -> Purpose;
    fn surrounding(&self, before: usize, after: usize) -> Option<Surrounding>;
    fn selection(&self) -> Option<TextRange>;
    fn caret_rect(&self) -> Rect;
    fn apply(&mut self, edit: Edit) -> CommitId;
    fn undo(&mut self, id: CommitId) -> bool;
    fn accepts_action(&self, _kind: &str) -> bool {
        false
    }
}

#[derive(Clone, Debug)]
struct Snapshot {
    id: CommitId,
    text: String,
    caret: usize,
    selection: Option<TextRange>,
    preedit: String,
}

/// In-memory field used to exercise the trait. Not an input engine.
#[derive(Clone, Debug)]
pub struct BufferContext {
    purpose: Purpose,
    text: String,
    caret: usize,
    selection: Option<TextRange>,
    caret_rect: Rect,
    preedit: String,
    commits: Vec<Snapshot>,
    next_id: u64,
}

impl BufferContext {
    pub fn new(purpose: Purpose) -> Self {
        Self {
            purpose,
            text: String::new(),
            caret: 0,
            selection: None,
            caret_rect: Rect::default(),
            preedit: String::new(),
            commits: Vec::new(),
            next_id: 1,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn preedit(&self) -> &str {
        &self.preedit
    }

    fn snapshot(&self, id: CommitId) -> Snapshot {
        Snapshot {
            id,
            text: self.text.clone(),
            caret: self.caret,
            selection: self.selection.clone(),
            preedit: self.preedit.clone(),
        }
    }
}

impl ImeContext for BufferContext {
    fn purpose(&self) -> Purpose {
        self.purpose
    }

    fn surrounding(&self, before: usize, after: usize) -> Option<Surrounding> {
        if self.purpose.is_secure() {
            return None;
        }
        let before = before.min(SURROUNDING_CAP);
        let after = after.min(SURROUNDING_CAP);
        let chars: Vec<char> = self.text.chars().collect();
        let caret = self.caret.min(chars.len());
        let start = caret.saturating_sub(before);
        let end = (caret + after).min(chars.len());
        Some(Surrounding {
            before: chars[start..caret].iter().collect(),
            after: chars[caret..end].iter().collect(),
        })
    }

    fn selection(&self) -> Option<TextRange> {
        if self.purpose.is_secure() {
            None
        } else {
            self.selection.clone()
        }
    }

    fn caret_rect(&self) -> Rect {
        self.caret_rect
    }

    fn apply(&mut self, edit: Edit) -> CommitId {
        let id = CommitId(self.next_id);
        self.next_id += 1;
        self.commits.push(self.snapshot(id));
        match edit {
            Edit::Insert { text } => {
                let mut chars: Vec<char> = self.text.chars().collect();
                let caret = self.caret.min(chars.len());
                let inserted: Vec<char> = text.chars().collect();
                let added = inserted.len();
                chars.splice(caret..caret, inserted);
                self.text = chars.into_iter().collect();
                self.caret = caret + added;
                self.preedit.clear();
            }
            Edit::Replace { range, text } => {
                let mut chars: Vec<char> = self.text.chars().collect();
                let start = range.start.min(chars.len());
                let end = range.end.min(chars.len()).max(start);
                let inserted: Vec<char> = text.chars().collect();
                let added = inserted.len();
                chars.splice(start..end, inserted);
                self.text = chars.into_iter().collect();
                self.caret = start + added;
                self.selection = None;
                self.preedit.clear();
            }
            Edit::Preedit { text, cursor } => {
                self.preedit = text;
                let _ = cursor;
            }
        }
        id
    }

    fn undo(&mut self, id: CommitId) -> bool {
        let Some(index) = self.commits.iter().position(|snap| snap.id == id) else {
            return false;
        };
        let snap = self.commits[index].clone();
        self.text = snap.text;
        self.caret = snap.caret;
        self.selection = snap.selection;
        self.preedit = snap.preedit;
        self.commits.truncate(index);
        true
    }
}
