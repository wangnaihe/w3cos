use crate::{InputEngine, InputIntent, Proposal, Purpose, Scope, Session};

/// `W3COS_L2=1`, `true`, or `script` turns L2 on. Anything else, including
/// unset, leaves it off.
pub fn l2_from_value(value: Option<&str>) -> bool {
    matches!(
        value.map(str::trim),
        Some("1") | Some("true") | Some("script")
    )
}

fn l2_from_env() -> bool {
    l2_from_value(std::env::var("W3COS_L2").ok().as_deref())
}

/// Routes intents to engines. Secure fields and L2 engines are skipped.
pub struct ImeHub {
    l2: bool,
}

impl Default for ImeHub {
    fn default() -> Self {
        Self::new()
    }
}

impl ImeHub {
    pub fn new() -> Self {
        Self { l2: l2_from_env() }
    }

    pub fn with_l2(enabled: bool) -> Self {
        Self { l2: enabled }
    }

    pub fn l2_enabled(&self) -> bool {
        self.l2
    }

    pub fn begin_session(
        &self,
        id: impl Into<String>,
        purpose: Purpose,
        locales: Vec<String>,
    ) -> Session {
        let granted = if purpose.is_secure() {
            Vec::new()
        } else {
            vec![Scope::Purpose, Scope::Surrounding, Scope::Selection]
        };
        Session {
            id: id.into(),
            purpose,
            locales,
            granted,
        }
    }

    pub fn handle_intent(
        &self,
        session: &Session,
        intent: &InputIntent,
        engines: &mut [&mut dyn InputEngine],
    ) -> Vec<Proposal> {
        if session.purpose.is_secure() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for engine in engines {
            if engine.manifest().uses_l2 && !self.l2 {
                continue;
            }
            out.extend(engine.on_intent(session, intent));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BufferContext, CommitId, Edit, EngineManifest, Feedback, ImeContext, InputEngine,
        InputIntent, IntentPayload, Modality, Proposal, Purpose, SURROUNDING_CAP, Scope, TextRange,
    };

    struct CountingEngine {
        calls: u32,
        l2: bool,
        proposals: Vec<Proposal>,
    }

    impl InputEngine for CountingEngine {
        fn manifest(&self) -> EngineManifest {
            EngineManifest {
                id: if self.l2 {
                    "l2.rewrite".into()
                } else {
                    "l0.rules".into()
                },
                modalities: vec![Modality::Key, Modality::Voice],
                locales: vec!["zh-Hans".into()],
                scopes: vec![Scope::Purpose],
                network: false,
                uses_l2: self.l2,
            }
        }

        fn on_intent(&mut self, _session: &Session, _intent: &InputIntent) -> Vec<Proposal> {
            self.calls += 1;
            self.proposals.clone()
        }
    }

    struct SilentEngine;

    impl InputEngine for SilentEngine {
        fn manifest(&self) -> EngineManifest {
            EngineManifest {
                id: "silent".into(),
                modalities: vec![],
                locales: vec![],
                scopes: vec![],
                network: false,
                uses_l2: false,
            }
        }

        fn on_intent(&mut self, _session: &Session, _intent: &InputIntent) -> Vec<Proposal> {
            Vec::new()
        }
    }

    #[test]
    fn l2_stays_off_unless_the_value_asks_for_it() {
        assert!(!l2_from_value(None));
        assert!(!l2_from_value(Some("")));
        assert!(!l2_from_value(Some("0")));
        assert!(!l2_from_value(Some("false")));
        assert!(l2_from_value(Some("1")));
        assert!(l2_from_value(Some(" true ")));
        assert!(l2_from_value(Some("script")));
        assert!(!ImeHub::with_l2(false).l2_enabled());
        assert!(ImeHub::with_l2(true).l2_enabled());
    }

    #[test]
    fn secure_fields_skip_engines_and_hide_context() {
        let hub = ImeHub::with_l2(false);
        let session = hub.begin_session("s", Purpose::Password, vec!["zh-Hans".into()]);
        assert!(session.granted.is_empty());
        assert!(!session.allows(Scope::Surrounding));
        let mut engine = CountingEngine {
            calls: 0,
            l2: false,
            proposals: vec![Proposal::candidate("c", "secret", 1.0, "L0")],
        };
        let intent = InputIntent::text(Modality::Key, "secret", &["zh-Hans"]);
        let proposals = hub.handle_intent(&session, &intent, &mut [&mut engine]);
        assert!(proposals.is_empty());
        assert_eq!(engine.calls, 0);

        let field = BufferContext::new(Purpose::Secure);
        assert!(field.surrounding(8, 8).is_none());
        assert!(field.selection().is_none());
        assert!(!field.accepts_action("calendar.create"));
    }

    #[test]
    fn l2_engines_are_skipped_until_the_hub_enables_them() {
        let hub = ImeHub::with_l2(false);
        let session = hub.begin_session("s", Purpose::Chat, vec!["zh-Hans".into(), "en".into()]);
        assert!(session.allows(Scope::Surrounding));
        assert!(!session.allows(Scope::Document));
        let mut rules = CountingEngine {
            calls: 0,
            l2: false,
            proposals: vec![Proposal::candidate("c1", "明天", 0.9, "L0")],
        };
        let mut rewrite = CountingEngine {
            calls: 0,
            l2: true,
            proposals: vec![Proposal::action("a1", "calendar.create", "L2")],
        };
        let intent = InputIntent {
            modality: Modality::Voice,
            payload: IntentPayload::Text {
                text: "明天下午三点和老王开会".into(),
            },
            interim: true,
            locale: vec!["zh-Hans".into()],
            hint: None,
        };
        let proposals = hub.handle_intent(&session, &intent, &mut [&mut rules, &mut rewrite]);
        assert_eq!(rules.calls, 1);
        assert_eq!(rewrite.calls, 0);
        assert_eq!(proposals.len(), 1);

        let hub = ImeHub::with_l2(true);
        let proposals = hub.handle_intent(&session, &intent, &mut [&mut rules, &mut rewrite]);
        assert_eq!(rewrite.calls, 1);
        match proposals.last() {
            Some(Proposal::Action {
                kind,
                requires_confirm,
                ..
            }) => {
                assert_eq!(kind, "calendar.create");
                assert!(*requires_confirm);
            }
            other => panic!("expected a confirmed action, got {other:?}"),
        }
    }

    #[test]
    fn voice_intent_keeps_modality_and_locale() {
        let intent = InputIntent::text(Modality::Voice, "改到下周一上午", &["zh-Hans", "en"]);
        let json = serde_json::to_value(&intent).unwrap();
        assert_eq!(json["modality"], "voice");
        assert_eq!(json["locale"][0], "zh-Hans");
        assert_eq!(json["locale"][1], "en");
        assert_eq!(json["payload"]["kind"], "text");
        let back: InputIntent = serde_json::from_value(json).unwrap();
        assert_eq!(back, intent);
    }

    #[test]
    fn buffer_insert_and_undo_roundtrip_chinese() {
        let mut field = BufferContext::new(Purpose::Text);
        let id = field.apply(Edit::Insert {
            text: "周五的评审".into(),
        });
        assert_eq!(field.text(), "周五的评审");
        let around = field.surrounding(2, 0).unwrap();
        assert_eq!(around.before, "评审");
        assert!(around.after.is_empty());
        let preedit = field.apply(Edit::Preedit {
            text: "ming".into(),
            cursor: 4,
        });
        assert_eq!(field.preedit(), "ming");
        assert!(field.undo(preedit));
        assert!(field.preedit().is_empty());
        assert_eq!(field.text(), "周五的评审");
        assert!(field.undo(id));
        assert!(field.text().is_empty());
        assert!(!field.undo(CommitId(99)));
    }

    #[test]
    fn surrounding_is_capped_and_replace_is_undoable() {
        let mut field = BufferContext::new(Purpose::Chat);
        let long: String = "字".repeat(400);
        field.apply(Edit::Insert { text: long });
        let around = field.surrounding(400, 10).unwrap();
        assert_eq!(around.before.chars().count(), SURROUNDING_CAP);
        let id = field.apply(Edit::Replace {
            range: TextRange { start: 0, end: 2 },
            text: "甲".into(),
        });
        assert!(field.text().starts_with('甲'));
        assert!(field.undo(id));
        assert!(field.text().starts_with('字'));
    }

    #[test]
    fn default_feedback_hook_is_a_no_op() {
        let mut engine = SilentEngine;
        engine.on_feedback(Feedback::Rejected {
            proposal_id: "c1".into(),
        });
    }
}
