use crate::{InputIntent, Proposal, Scope, Session};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineManifest {
    pub id: String,
    pub modalities: Vec<crate::Modality>,
    pub locales: Vec<String>,
    pub scopes: Vec<Scope>,
    pub network: bool,
    /// When set, the hub ignores this engine unless L2 was explicitly enabled.
    pub uses_l2: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Feedback {
    Accepted { proposal_id: String },
    Rejected { proposal_id: String },
}

pub trait InputEngine: Send {
    fn manifest(&self) -> EngineManifest;
    fn on_intent(&mut self, session: &Session, intent: &InputIntent) -> Vec<Proposal>;
    fn on_feedback(&mut self, _feedback: Feedback) {}
}
