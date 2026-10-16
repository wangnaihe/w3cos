use crate::{
    InputEngine, InputIntent, IntentPayload, Proposal, Purpose, Scope, Session, command_utterance,
    route_p3,
};

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

    /// Extra scope for a local engine. Screen, personal, document, and app
    /// stay refused until a visible grant exists. Secure sessions refuse all.
    pub fn grant(session: &mut Session, scope: Scope) -> bool {
        if session.purpose.is_secure() || session.id.is_empty() {
            return false;
        }
        match scope {
            Scope::Purpose | Scope::Surrounding | Scope::Selection => {
                if !session.granted.contains(&scope) {
                    session.granted.push(scope);
                }
                true
            }
            Scope::Document | Scope::App | Scope::Screen | Scope::Personal => false,
        }
    }

    pub fn handle_intent(
        &self,
        session: &Session,
        intent: &InputIntent,
        engines: &mut [&mut dyn InputEngine],
    ) -> Vec<Proposal> {
        if session.purpose.is_secure() || session.id.is_empty() {
            return Vec::new();
        }
        if let Some(mut routed) = route_p3(session, intent.modality, &intent.payload) {
            if routed
                .iter()
                .any(|proposal| matches!(proposal, Proposal::Candidate { .. }))
            {
                append_command_actions(session.purpose, intent, &mut routed);
                force_confirm(&mut routed);
            }
            return routed;
        }
        let mut out = Vec::new();
        for engine in engines {
            if engine.manifest().uses_l2 && !self.l2 {
                continue;
            }
            out.extend(engine.on_intent(session, intent));
        }
        force_confirm(&mut out);
        append_command_actions(session.purpose, intent, &mut out);
        out
    }
}

fn force_confirm(proposals: &mut [Proposal]) {
    for proposal in proposals {
        if let Proposal::Action {
            requires_confirm, ..
        } = proposal
        {
            *requires_confirm = true;
        }
    }
}

/// Command fields can offer one existing shell utterance. Text candidates stay.
fn append_command_actions(purpose: Purpose, intent: &InputIntent, proposals: &mut Vec<Proposal>) {
    if purpose != Purpose::Command {
        return;
    }
    let mut texts = Vec::new();
    for proposal in proposals.iter() {
        if let Proposal::Candidate { text, .. } = proposal {
            texts.push(text.clone());
        }
    }
    if let IntentPayload::Text { text } = &intent.payload {
        texts.push(text.clone());
    }
    for text in texts {
        let Some(utterance) = command_utterance(&text) else {
            continue;
        };
        if proposals
            .iter()
            .any(|proposal| matches!(proposal, Proposal::Action { kind, .. } if kind == utterance))
        {
            continue;
        }
        proposals.push(Proposal::command(
            format!("cmd-{utterance}"),
            utterance,
            text,
        ));
        break;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BufferContext, CommitId, Edit, EngineManifest, Feedback, ImeContext, InputEngine,
        InputIntent, IntentPayload, Modality, PinyinEngine, Proposal, Purpose, SURROUNDING_CAP,
        Scope, TextRange,
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
    fn field_roles_pick_a_purpose_and_secure_always_bypasses() {
        assert_eq!(Purpose::for_field("intent", false), Purpose::Command);
        assert_eq!(Purpose::for_field("terminal", false), Purpose::Code);
        assert_eq!(Purpose::for_field("editor", false), Purpose::Code);
        assert_eq!(Purpose::for_field("code", false), Purpose::Code);
        assert_eq!(Purpose::for_field("launcher", false), Purpose::Search);
        assert_eq!(Purpose::for_field("search", false), Purpose::Search);
        assert_eq!(Purpose::for_field("text", false), Purpose::Text);
        assert_eq!(Purpose::for_field("password", false), Purpose::Password);
        assert_eq!(Purpose::for_field("secure", false), Purpose::Secure);
        assert_eq!(Purpose::for_field("intent", true), Purpose::Secure);
        assert_eq!(Purpose::for_field("password", true), Purpose::Password);
    }

    #[test]
    fn grants_stop_at_local_context_and_end_clears_them() {
        let hub = ImeHub::with_l2(false);
        let mut session = hub.begin_session("s", Purpose::Command, vec!["zh-Hans".into()]);
        assert!(session.allows(Scope::Purpose));
        assert!(session.allows(Scope::Surrounding));
        assert!(session.allows(Scope::Selection));
        assert!(!session.allows(Scope::Document));
        assert!(!session.allows(Scope::App));
        assert!(!session.allows(Scope::Screen));
        assert!(!session.allows(Scope::Personal));
        assert!(!ImeHub::grant(&mut session, Scope::Screen));
        assert!(!ImeHub::grant(&mut session, Scope::Personal));
        assert!(!ImeHub::grant(&mut session, Scope::Document));
        assert!(ImeHub::grant(&mut session, Scope::Surrounding));
        session.end();
        assert!(session.id.is_empty());
        assert!(session.granted.is_empty());
        assert!(!ImeHub::grant(&mut session, Scope::Purpose));

        let mut locked = hub.begin_session("lock", Purpose::Password, vec!["zh-Hans".into()]);
        assert!(locked.granted.is_empty());
        assert!(!ImeHub::grant(&mut locked, Scope::Purpose));
    }

    #[test]
    fn command_purpose_offers_one_confirmable_action() {
        let hub = ImeHub::with_l2(false);
        let session = hub.begin_session("intent", Purpose::Command, vec!["zh-Hans".into()]);
        let mut engine = PinyinEngine::new();
        let intent = InputIntent::text(Modality::Key, "wenjian", &["zh-Hans"]);
        let proposals = hub.handle_intent(&session, &intent, &mut [&mut engine]);
        assert!(proposals.iter().any(|proposal| {
            matches!(proposal, Proposal::Candidate { text, .. } if text == "文件")
        }));
        let action = proposals.iter().find_map(|proposal| match proposal {
            Proposal::Action {
                kind,
                summary,
                requires_confirm,
                source,
                ..
            } => Some((
                kind.as_str(),
                summary.as_str(),
                *requires_confirm,
                source.as_str(),
            )),
            _ => None,
        });
        assert_eq!(action, Some(("open files", "文件", true, "L0")));
        assert_eq!(
            proposals
                .iter()
                .filter(|proposal| matches!(proposal, Proposal::Action { .. }))
                .count(),
            1
        );

        let code = hub.begin_session("term", Purpose::Code, vec!["zh-Hans".into()]);
        let code_proposals = hub.handle_intent(&code, &intent, &mut [&mut engine]);
        assert!(code_proposals.iter().any(|proposal| {
            matches!(proposal, Proposal::Candidate { text, .. } if text == "文件")
        }));
        assert!(
            !code_proposals
                .iter()
                .any(|proposal| matches!(proposal, Proposal::Action { .. }))
        );

        let search = hub.begin_session("find", Purpose::Search, vec!["zh-Hans".into()]);
        assert!(
            !hub.handle_intent(&search, &intent, &mut [&mut engine])
                .iter()
                .any(|proposal| matches!(proposal, Proposal::Action { .. }))
        );
    }

    #[test]
    fn an_engine_cannot_mark_an_action_as_auto_run() {
        let hub = ImeHub::with_l2(true);
        let session = hub.begin_session("s", Purpose::Chat, vec!["zh-Hans".into()]);
        let mut engine = CountingEngine {
            calls: 0,
            l2: true,
            proposals: vec![Proposal::Action {
                id: "a".into(),
                kind: "calendar.create".into(),
                requires_confirm: false,
                source: "L2".into(),
                summary: "calendar.create".into(),
            }],
        };
        let intent = InputIntent::text(Modality::Key, "meet", &["zh-Hans"]);
        let proposals = hub.handle_intent(&session, &intent, &mut [&mut engine]);
        match proposals.last() {
            Some(Proposal::Action {
                requires_confirm, ..
            }) => assert!(*requires_confirm),
            other => panic!("expected an action, got {other:?}"),
        }
    }

    #[test]
    fn input_intent_and_action_json_match_the_schema_names() {
        let schema = include_str!("../../../spec/ime/w3cos-ime-1.schema.json");
        assert!(schema.contains("InputIntent"));
        assert!(schema.contains("ActionProposal"));
        assert!(schema.contains("ctx.purpose"));
        assert!(schema.contains("requires_confirm"));
        let intent = InputIntent::text(Modality::Key, "wenjian", &["zh-Hans"]);
        let json = serde_json::to_value(&intent).unwrap();
        assert_eq!(json["modality"], "key");
        assert_eq!(json["payload"]["kind"], "text");
        let action = Proposal::command("cmd-open-files", "open files", "文件");
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["kind"], "action");
        assert_eq!(json["action"], "open files");
        assert_eq!(json["summary"], "文件");
        assert_eq!(json["requires_confirm"], true);
        assert_eq!(json["source"], "L0");
    }

    #[test]
    fn default_feedback_hook_is_a_no_op() {
        let mut engine = SilentEngine;
        engine.on_feedback(Feedback::Rejected {
            proposal_id: "c1".into(),
        });
    }

    #[test]
    fn p3_surface_blocks_models_and_screen_and_readies_clipboard() {
        let hub = ImeHub::with_l2(false);
        let mut session = hub.begin_session("s", Purpose::Text, vec!["zh-Hans".into()]);
        let surface = crate::p3_surface(&session);
        assert_eq!(
            surface.map(|item| item.modality),
            [
                Modality::Voice,
                Modality::Handwriting,
                Modality::Clipboard,
                Modality::Camera,
            ]
        );
        assert_eq!(
            surface[0].state,
            crate::ModalityState::Blocked(crate::ModalityBlock::NoLocalModel)
        );
        assert_eq!(
            surface[1].state,
            crate::ModalityState::Blocked(crate::ModalityBlock::NoLocalModel)
        );
        assert_eq!(surface[2].state, crate::ModalityState::Ready);
        assert!(surface[2].scope.is_none());
        assert_eq!(
            surface[3].state,
            crate::ModalityState::Blocked(crate::ModalityBlock::ScreenRefused)
        );
        assert_eq!(surface[3].scope, Some(Scope::Screen));
        assert!(!ImeHub::grant(&mut session, Scope::Screen));
        assert!(!session.allows(Scope::Screen));
    }

    #[test]
    fn secure_and_ended_sessions_block_clipboard_capture() {
        let hub = ImeHub::with_l2(false);
        let secure = hub.begin_session("lock", Purpose::Password, vec!["zh-Hans".into()]);
        assert!(crate::p3_surface(&secure).iter().all(|item| {
            item.state == crate::ModalityState::Blocked(crate::ModalityBlock::SecureField)
        }));
        assert_eq!(
            crate::clipboard_decision(&secure, "secret"),
            crate::ClipboardDecision::Refused(crate::ModalityBlock::SecureField)
        );
        let mut ended = hub.begin_session("s", Purpose::Text, vec!["zh-Hans".into()]);
        ended.end();
        assert!(crate::p3_surface(&ended).iter().all(|item| {
            item.state == crate::ModalityState::Blocked(crate::ModalityBlock::NoSession)
        }));
        assert_eq!(
            crate::clipboard_decision(&ended, "周五"),
            crate::ClipboardDecision::Refused(crate::ModalityBlock::NoSession)
        );
    }

    #[test]
    fn clipboard_text_is_one_candidate_and_engines_do_not_see_it() {
        let hub = ImeHub::with_l2(false);
        let session = hub.begin_session("s", Purpose::Chat, vec!["zh-Hans".into()]);
        let mut engine = CountingEngine {
            calls: 0,
            l2: false,
            proposals: vec![Proposal::candidate("no", "不应出现", 1.0, "L0")],
        };
        let intent = InputIntent::text(Modality::Clipboard, "  周五的评审  ", &["zh-Hans"]);
        let proposals = hub.handle_intent(&session, &intent, &mut [&mut engine]);
        assert_eq!(engine.calls, 0);
        assert_eq!(proposals.len(), 1);
        match &proposals[0] {
            Proposal::Candidate {
                id, text, source, ..
            } => {
                assert_eq!(id, "clip");
                assert_eq!(text, "周五的评审");
                assert_eq!(source, "clipboard");
            }
            other => panic!("expected a clipboard candidate, got {other:?}"),
        }
        assert_eq!(
            crate::clipboard_decision(&session, "   "),
            crate::ClipboardDecision::Empty
        );
        let huge = "字".repeat(crate::CLIPBOARD_LIMIT + 1);
        assert_eq!(
            crate::clipboard_decision(&session, &huge),
            crate::ClipboardDecision::TooLong
        );
        let refused = hub.handle_intent(
            &session,
            &InputIntent::text(Modality::Clipboard, &huge, &["zh-Hans"]),
            &mut [&mut engine],
        );
        assert!(refused.is_empty());
        assert_eq!(engine.calls, 0);
    }

    #[test]
    fn clipboard_on_a_command_field_still_needs_confirm() {
        let hub = ImeHub::with_l2(false);
        let session = hub.begin_session("intent", Purpose::Command, vec!["zh-Hans".into()]);
        let mut engine = CountingEngine {
            calls: 0,
            l2: false,
            proposals: vec![],
        };
        let intent = InputIntent::text(Modality::Clipboard, "打开文件", &["zh-Hans"]);
        let proposals = hub.handle_intent(&session, &intent, &mut [&mut engine]);
        assert_eq!(engine.calls, 0);
        assert!(proposals.iter().any(|proposal| {
            matches!(proposal, Proposal::Candidate { text, source, .. } if text == "打开文件" && source == "clipboard")
        }));
        match proposals
            .iter()
            .find(|proposal| matches!(proposal, Proposal::Action { .. }))
        {
            Some(Proposal::Action {
                kind,
                requires_confirm,
                ..
            }) => {
                assert_eq!(kind, "open files");
                assert!(*requires_confirm);
            }
            other => panic!("expected a confirmable action, got {other:?}"),
        }
    }

    #[test]
    fn audio_ink_and_camera_never_become_text() {
        let hub = ImeHub::with_l2(false);
        let session = hub.begin_session("s", Purpose::Text, vec!["zh-Hans".into()]);
        let mut engine = CountingEngine {
            calls: 0,
            l2: false,
            proposals: vec![Proposal::candidate("fake", "识别结果", 1.0, "L2")],
        };
        let audio = InputIntent {
            modality: Modality::Voice,
            payload: IntentPayload::Audio {
                reference: "mic://session".into(),
            },
            interim: true,
            locale: vec!["zh-Hans".into()],
            hint: None,
        };
        let ink = InputIntent {
            modality: Modality::Handwriting,
            payload: IntentPayload::Ink { strokes: 4 },
            interim: false,
            locale: vec!["zh-Hans".into()],
            hint: None,
        };
        let ink_text = InputIntent::text(Modality::Handwriting, "永", &["zh-Hans"]);
        let camera = InputIntent::text(Modality::Camera, "屏幕上的字", &["zh-Hans"]);
        for intent in [&audio, &ink, &ink_text, &camera] {
            let proposals = hub.handle_intent(&session, intent, &mut [&mut engine]);
            assert!(proposals.is_empty(), "{intent:?} -> {proposals:?}");
        }
        assert_eq!(engine.calls, 0);

        let transcript = InputIntent {
            modality: Modality::Voice,
            payload: IntentPayload::Text {
                text: "改到下周一上午".into(),
            },
            interim: true,
            locale: vec!["zh-Hans".into()],
            hint: None,
        };
        let proposals = hub.handle_intent(&session, &transcript, &mut [&mut engine]);
        assert_eq!(engine.calls, 1);
        assert_eq!(proposals.len(), 1);
    }
}
