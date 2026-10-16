//! Multi-step shell agent.
//!
//! The UI thread owns [`AgentLoop`]. A worker thread only calls the planner.
//! Every tool, including a read, stops for confirm. Nothing touches the view
//! until [`AgentLoop::confirm`]. The caller checks `capability_gate` there.
//! L2 stays off unless `W3COS_L2` is set.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Instant;

use crate::intent::{Decision, IntentTier, ShellApp};
use crate::planner::{
    self, ChatMessage, L2Mode, OpenAiPlanner, PlannedCall, Planner, PlannerCompletion, PlannerStep,
    PlannerTurn, ScriptedPlanner,
};
use crate::shell_tools::{self, PreparedTool, ShellEffect, ShellView};

const MAX_ROUNDS: u8 = 8;
const MAX_STEPS: usize = 12;

#[derive(Clone, Debug)]
pub struct ReceiptDraft {
    pub command: String,
    pub decision: Decision,
    pub confirmed: bool,
    pub outcome: String,
    pub tier: IntentTier,
}

#[derive(Clone, Debug)]
pub enum DriveKind {
    Plan(PlannerTurn),
    AwaitConfirm,
    Finished(String),
    Failed(String),
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct Drive {
    pub kind: DriveKind,
    pub utterance: String,
    pub effects: Vec<ShellEffect>,
    pub receipts: Vec<ReceiptDraft>,
    pub steps: String,
    pub summary: String,
    pub impact: String,
    pub notice: String,
}

#[derive(Clone, Debug, Default)]
pub struct SessionTrace {
    pub tools: Vec<String>,
    pub bodies: Vec<String>,
    pub apps: Vec<ShellApp>,
    pub night: bool,
    pub night_changes: u32,
    pub terminal_line: String,
    pub terminal_output: String,
    pub final_text: String,
    pub latencies_ms: Vec<u128>,
    pub ttft_ms: Vec<u128>,
    pub ok: bool,
}

struct Pending {
    id: String,
    tool: PreparedTool,
}

#[derive(Default)]
pub struct AgentLoop {
    utterance: String,
    messages: Vec<ChatMessage>,
    queue: VecDeque<PlannedCall>,
    pending: Option<Pending>,
    steps: Vec<String>,
    rounds: u8,
    acted: usize,
    timing: String,
}

impl AgentLoop {
    pub fn start(&mut self, utterance: &str) -> Drive {
        *self = Self {
            utterance: utterance.to_string(),
            messages: vec![ChatMessage::user(utterance)],
            ..Self::default()
        };
        self.utterance = utterance.to_string();
        self.plan_drive(Vec::new(), Vec::new())
    }

    pub fn on_completion(
        &mut self,
        completion: PlannerCompletion,
        view: &mut ShellView,
        gate: &dyn Fn(&str) -> bool,
    ) -> Drive {
        self.rounds = self.rounds.saturating_add(1);
        self.timing = match completion.first_token_ms {
            Some(ttft) => format!("ttft {ttft}ms · {}ms", completion.total_ms),
            None => format!("{}ms", completion.total_ms),
        };
        if self.rounds > MAX_ROUNDS {
            return self.fail("stopped: planner round limit");
        }
        match completion.step {
            PlannerStep::Final(text) => {
                if self.rounds < MAX_ROUNDS && (self.last_tool_failed() || hands_work_back(&text)) {
                    self.messages.push(ChatMessage::user(
                        "The request is not done. Call the next tool yourself. Do not tell the user to click or type. If the last tool failed, use the exact command from its result.",
                    ));
                    self.note("retry");
                    return self.plan_drive(Vec::new(), Vec::new());
                }
                self.finish(text)
            }
            PlannerStep::Calls(calls) => {
                if calls.is_empty() {
                    return self.fail("planner returned an empty tool list");
                }
                self.messages.push(ChatMessage::assistant(calls.clone()));
                self.queue.extend(calls);
                self.drain(view, gate)
            }
        }
    }

    pub fn confirm(&mut self, view: &mut ShellView, gate: &dyn Fn(&str) -> bool) -> Drive {
        let Some(pending) = self.pending.take() else {
            return self.fail("nothing is waiting for confirm");
        };
        if !pending.tool.capability.is_empty() && !gate(pending.tool.capability) {
            let outcome = format!(
                "Denied: {} is not granted. Nothing was changed.",
                pending.tool.capability
            );
            self.messages
                .push(ChatMessage::tool(pending.id, outcome.clone()));
            let receipt = self.receipt(&pending.tool.name, Decision::Denied, true, &outcome);
            self.note(&format!(
                "{} · denied {}",
                pending.tool.name, pending.tool.capability
            ));
            self.queue.clear();
            return self.drive(
                DriveKind::Failed(outcome.clone()),
                Vec::new(),
                vec![receipt],
                String::new(),
                String::new(),
                outcome,
            );
        }
        match shell_tools::perform(&pending.tool, view) {
            Ok((body, effects)) => {
                shell_tools::apply_effects(view, &effects);
                self.messages
                    .push(ChatMessage::tool(pending.id, truncate(&body)));
                let receipt = self.receipt(&pending.tool.name, Decision::Allowed, true, &body);
                self.note(&format!("{} · allowed", pending.tool.name));
                let mut drive = self.drain(view, gate);
                drive.effects.splice(0..0, effects);
                drive.receipts.insert(0, receipt);
                drive
            }
            Err(err) => {
                let outcome = format!("Denied: {err}. Nothing was changed.");
                self.messages
                    .push(ChatMessage::tool(pending.id, outcome.clone()));
                let receipt = self.receipt(&pending.tool.name, Decision::Denied, true, &outcome);
                self.note(&format!("{} · {}", pending.tool.name, err));
                let mut drive = self.drain(view, gate);
                drive.receipts.insert(0, receipt);
                drive
            }
        }
    }

    pub fn reject(&mut self, reason: &str) -> Drive {
        let Some(pending) = self.pending.take() else {
            return self.fail(reason);
        };
        let outcome = format!("Denied: {reason}. Nothing was changed.");
        self.messages
            .push(ChatMessage::tool(pending.id, outcome.clone()));
        let receipt = self.receipt(&pending.tool.name, Decision::Denied, true, &outcome);
        self.queue.clear();
        self.note(&format!("{} · {reason}", pending.tool.name));
        self.drive(
            DriveKind::Failed(outcome.clone()),
            Vec::new(),
            vec![receipt],
            String::new(),
            String::new(),
            outcome,
        )
    }

    pub fn cancel(&mut self) -> Drive {
        let receipts = if let Some(pending) = self.pending.take() {
            let outcome = "Cancelled. Nothing was changed.";
            self.messages.push(ChatMessage::tool(pending.id, outcome));
            self.note(&format!("{} · cancelled", pending.tool.name));
            vec![self.receipt(&pending.tool.name, Decision::Cancelled, false, outcome)]
        } else {
            Vec::new()
        };
        self.queue.clear();
        self.drive(
            DriveKind::Cancelled,
            Vec::new(),
            receipts,
            String::new(),
            String::new(),
            "Cancelled. Nothing was changed.".into(),
        )
    }

    pub fn pending_capability(&self) -> &'static str {
        self.pending
            .as_ref()
            .map(|pending| pending.tool.capability)
            .unwrap_or("")
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    fn drain(&mut self, view: &mut ShellView, _gate: &dyn Fn(&str) -> bool) -> Drive {
        let effects = Vec::new();
        let mut receipts = Vec::new();
        while let Some(call) = self.queue.pop_front() {
            if self.acted >= MAX_STEPS {
                return self.fail("stopped: step limit");
            }
            self.acted = self.acted.saturating_add(1);
            let prepared = match shell_tools::prepare(&call.name, &call.arguments, view) {
                Ok(tool) => tool,
                Err(err) => {
                    let outcome = format!("tool error: {err}");
                    self.messages
                        .push(ChatMessage::tool(call.id, outcome.clone()));
                    receipts.push(self.receipt(&call.name, Decision::Denied, false, &outcome));
                    self.note(&format!("{} · {err}", call.name));
                    continue;
                }
            };
            self.note(&format!(
                "{} · awaiting confirm {}",
                prepared.name, self.timing
            ));
            let summary = prepared.summary.clone();
            let impact = prepared.impact.clone();
            self.pending = Some(Pending {
                id: call.id,
                tool: prepared,
            });
            return self.drive(
                DriveKind::AwaitConfirm,
                effects,
                receipts,
                summary,
                impact,
                "确认后才会执行 · Confirm to run".into(),
            );
        }
        self.plan_drive(effects, receipts)
    }

    fn plan_drive(&mut self, effects: Vec<ShellEffect>, receipts: Vec<ReceiptDraft>) -> Drive {
        self.drive(
            DriveKind::Plan(PlannerTurn {
                messages: self.messages.clone(),
            }),
            effects,
            receipts,
            String::new(),
            String::new(),
            "规划中 Planning. The model is off the UI thread.".into(),
        )
    }

    fn finish(&mut self, text: String) -> Drive {
        self.note("done");
        self.drive(
            DriveKind::Finished(text.clone()),
            Vec::new(),
            Vec::new(),
            String::new(),
            String::new(),
            text,
        )
    }

    fn fail(&mut self, reason: &str) -> Drive {
        self.queue.clear();
        self.pending = None;
        self.note(reason);
        self.drive(
            DriveKind::Failed(reason.to_string()),
            Vec::new(),
            Vec::new(),
            String::new(),
            String::new(),
            reason.to_string(),
        )
    }

    fn note(&mut self, line: &str) {
        self.steps.push(format!("{}. {line}", self.steps.len() + 1));
    }

    fn last_tool_failed(&self) -> bool {
        let Some(message) = self
            .messages
            .iter()
            .rev()
            .find(|message| message.role == "tool")
        else {
            return false;
        };
        let text = message.content.to_lowercase();
        text.contains("no l0 command")
            || text.contains("tool error")
            || text.contains("not both")
            || text.contains("missing")
            || text.contains("denied")
            || text.contains("not the terminal")
            || text.contains("not interactive")
    }

    fn receipt(
        &self,
        name: &str,
        decision: Decision,
        confirmed: bool,
        outcome: &str,
    ) -> ReceiptDraft {
        ReceiptDraft {
            command: format!("l2:{name}"),
            decision,
            confirmed,
            outcome: truncate(outcome),
            tier: IntentTier::L2,
        }
    }

    fn drive(
        &self,
        kind: DriveKind,
        effects: Vec<ShellEffect>,
        receipts: Vec<ReceiptDraft>,
        summary: String,
        impact: String,
        notice: String,
    ) -> Drive {
        Drive {
            kind,
            utterance: self.utterance.clone(),
            effects,
            receipts,
            steps: self.steps.join("\n"),
            summary,
            impact,
            notice,
        }
    }
}

fn hands_work_back(text: &str) -> bool {
    let text = text.to_lowercase();
    // Imperative hand-off, not a past-tense receipt such as "clicked".
    text.contains("click ")
        || text.contains("click.")
        || text.contains("what would you like")
        || text.contains("now, let's")
        || text.contains("let's run")
}

fn truncate(text: &str) -> String {
    let mut out: String = text.chars().take(800).collect();
    if text.chars().count() > 800 {
        out.push('…');
    }
    out
}

fn generation() -> &'static AtomicU64 {
    static GEN: OnceLock<AtomicU64> = OnceLock::new();
    GEN.get_or_init(|| AtomicU64::new(1))
}

fn mailbox() -> &'static Mutex<Option<Result<PlannerCompletion, String>>> {
    static MAIL: OnceLock<Mutex<Option<Result<PlannerCompletion, String>>>> = OnceLock::new();
    MAIL.get_or_init(|| Mutex::new(None))
}

fn shared_loop() -> &'static Mutex<AgentLoop> {
    static LOOP: OnceLock<Mutex<AgentLoop>> = OnceLock::new();
    LOOP.get_or_init(|| Mutex::new(AgentLoop::default()))
}

type WakeHook = fn();

fn wake_slot() -> &'static Mutex<Option<WakeHook>> {
    static WAKE: OnceLock<Mutex<Option<WakeHook>>> = OnceLock::new();
    WAKE.get_or_init(|| Mutex::new(None))
}

/// The shell registers a nudge for the window event loop. The worker calls it
/// after a completion so the UI thread can drain the mailbox.
pub fn set_wake(hook: fn()) {
    if let Ok(mut slot) = wake_slot().lock() {
        *slot = Some(hook);
    }
}

fn wake() {
    if let Ok(slot) = wake_slot().lock()
        && let Some(hook) = *slot
    {
        hook();
    }
}

pub fn begin_run(utterance: &str) -> Drive {
    generation().fetch_add(1, Ordering::SeqCst);
    if let Ok(mut mail) = mailbox().lock() {
        *mail = None;
    }
    shared_loop()
        .lock()
        .map(|mut agent| agent.start(utterance))
        .unwrap_or_else(|_| AgentLoop::default().start(utterance))
}

pub fn spawn_plan(turn: PlannerTurn) {
    let seen = generation().load(Ordering::SeqCst);
    let spawned = thread::Builder::new()
        .name("w3cos-l2".into())
        .spawn(move || {
            let body = plan_with_env(&turn);
            if generation().load(Ordering::SeqCst) == seen
                && let Ok(mut slot) = mailbox().lock()
            {
                *slot = Some(body);
            }
            wake();
        });
    if spawned.is_err()
        && let Ok(mut slot) = mailbox().lock()
    {
        *slot = Some(Err("failed to start the planner thread".into()));
    }
}

pub fn take_mail() -> Option<Result<PlannerCompletion, String>> {
    mailbox().lock().ok()?.take()
}

pub fn pending_capability() -> &'static str {
    shared_loop()
        .lock()
        .map(|agent| agent.pending_capability())
        .unwrap_or("")
}

pub fn has_pending() -> bool {
    shared_loop()
        .lock()
        .map(|agent| agent.has_pending())
        .unwrap_or(false)
}

pub fn on_model(
    completion: PlannerCompletion,
    view: &mut ShellView,
    gate: &dyn Fn(&str) -> bool,
) -> Drive {
    match shared_loop().lock() {
        Ok(mut agent) => agent.on_completion(completion, view, gate),
        Err(_) => AgentLoop::default().start(""),
    }
}

pub fn confirm_run(view: &mut ShellView, gate: &dyn Fn(&str) -> bool) -> Drive {
    match shared_loop().lock() {
        Ok(mut agent) => agent.confirm(view, gate),
        Err(_) => AgentLoop::default().start(""),
    }
}

pub fn reject_run(reason: &str) -> Drive {
    match shared_loop().lock() {
        Ok(mut agent) => agent.reject(reason),
        Err(_) => AgentLoop::default().start(""),
    }
}

pub fn cancel_run() -> Drive {
    generation().fetch_add(1, Ordering::SeqCst);
    if let Ok(mut mail) = mailbox().lock() {
        *mail = None;
    }
    match shared_loop().lock() {
        Ok(mut agent) => agent.cancel(),
        Err(_) => AgentLoop::default().start(""),
    }
}

fn plan_with_env(turn: &PlannerTurn) -> Result<PlannerCompletion, String> {
    match planner::mode_from_env()? {
        L2Mode::Off => Err("L2 is off".into()),
        L2Mode::Script => ScriptedPlanner.complete(turn),
        L2Mode::Remote(config) => OpenAiPlanner { config }.complete(turn),
    }
}

pub struct Task {
    pub id: &'static str,
    pub utterance: &'static str,
}

pub const TASKS: &[Task] = &[
    Task {
        id: "open-files",
        utterance: "please open the files application",
    },
    Task {
        id: "night-mode",
        utterance: "switch the display to night mode",
    },
    Task {
        id: "find-setting",
        utterance: "where is the night mode setting",
    },
    Task {
        id: "terminal-echo",
        utterance: "open the terminal and run echo hello",
    },
    Task {
        id: "a11y-click",
        utterance: "read the accessibility tree and click the night mode control",
    },
    Task {
        id: "open-editor",
        utterance: "open the text editor",
    },
    Task {
        id: "front-app",
        utterance: "what application is in front",
    },
    Task {
        id: "open-browser",
        utterance: "launch the web browser",
    },
    Task {
        id: "files-then-settings",
        utterance: "open files and then open settings",
    },
    Task {
        id: "terminal-help",
        utterance: "show help after opening the terminal",
    },
];

pub fn task_passed(id: &str, trace: &SessionTrace) -> bool {
    if !trace.ok {
        return false;
    }
    match id {
        "open-files" => trace.apps.contains(&ShellApp::Files),
        "night-mode" => trace.night_changes >= 1,
        "find-setting" => trace.apps.contains(&ShellApp::Settings),
        "terminal-echo" => trace.terminal_output.contains("hello"),
        "a11y-click" => {
            trace.tools.iter().any(|tool| tool.contains("a11y_index")) && trace.night_changes >= 1
        }
        "open-editor" => trace.apps.contains(&ShellApp::Editor),
        "front-app" => trace.bodies.iter().any(|body| body.contains("Active app")),
        "open-browser" => trace.apps.contains(&ShellApp::Browser),
        "files-then-settings" => {
            trace.apps.contains(&ShellApp::Files) && trace.apps.last() == Some(&ShellApp::Settings)
        }
        "terminal-help" => trace.terminal_line == "help" && trace.terminal_output.contains("echo"),
        _ => false,
    }
}

/// Run one utterance to completion. Confirm grants the one-shot capabilities
/// the shell grants when the user presses Confirm.
pub fn drive_until_done(planner: &dyn Planner, utterance: &str) -> SessionTrace {
    let mut agent = AgentLoop::default();
    let mut view = ShellView::default();
    let mut trace = SessionTrace::default();
    let mut drive = agent.start(utterance);
    for _ in 0..32 {
        note_trace(&mut trace, &drive, &view);
        match drive.kind {
            DriveKind::Plan(turn) => {
                let started = Instant::now();
                let completion = match planner.complete(&turn) {
                    Ok(mut completion) => {
                        if completion.total_ms == 0 {
                            completion.total_ms = started.elapsed().as_millis();
                        }
                        completion
                    }
                    Err(err) => {
                        trace.final_text = err;
                        trace.ok = false;
                        return trace;
                    }
                };
                trace.latencies_ms.push(completion.total_ms);
                if let Some(ttft) = completion.first_token_ms {
                    trace.ttft_ms.push(ttft);
                }
                drive = agent.on_completion(completion, &mut view, &read_gate);
            }
            DriveKind::AwaitConfirm => {
                drive = agent.confirm(&mut view, &confirm_gate);
            }
            DriveKind::Finished(text) => {
                trace.final_text = text;
                trace.ok = true;
                return trace;
            }
            DriveKind::Failed(text) => {
                trace.final_text = text;
                trace.ok = false;
                return trace;
            }
            DriveKind::Cancelled => {
                trace.final_text = "cancelled".into();
                trace.ok = false;
                return trace;
            }
        }
    }
    trace.final_text = "stopped: round limit".into();
    trace.ok = false;
    trace
}

fn note_trace(trace: &mut SessionTrace, drive: &Drive, view: &ShellView) {
    for receipt in &drive.receipts {
        trace.tools.push(receipt.command.clone());
        trace.bodies.push(receipt.outcome.clone());
    }
    if trace.apps.last() != Some(&view.app) && view.app != ShellApp::Desktop {
        trace.apps.push(view.app);
    }
    if view.night != trace.night {
        trace.night_changes = trace.night_changes.saturating_add(1);
        trace.night = view.night;
    }
    trace.terminal_line = view.terminal_line.clone();
    trace.terminal_output = view.terminal_output.clone();
}

fn read_gate(name: &str) -> bool {
    matches!(name, shell_tools::CAP_A11Y | shell_tools::CAP_CAPTURE)
}

fn confirm_gate(name: &str) -> bool {
    matches!(
        name,
        shell_tools::CAP_A11Y
            | shell_tools::CAP_CAPTURE
            | shell_tools::CAP_ACT
            | shell_tools::CAP_PROCESS
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::PlannerStep;
    use crate::shell_tools::ShellEffect;

    #[test]
    fn scripted_suite_passes_the_fixed_tasks() {
        let planner = ScriptedPlanner;
        for task in TASKS {
            let trace = drive_until_done(&planner, task.utterance);
            assert!(
                task_passed(task.id, &trace),
                "{} failed: apps={:?} night_changes={} line={} output={} tools={:?} final={}",
                task.id,
                trace.apps,
                trace.night_changes,
                trace.terminal_line,
                trace.terminal_output,
                trace.tools,
                trace.final_text
            );
        }
    }

    #[test]
    fn a_missing_capability_denies_the_mutation_and_writes_nothing() {
        let mut agent = AgentLoop::default();
        let mut view = ShellView::default();
        let drive = agent.start("switch the display to night mode");
        let DriveKind::Plan(turn) = drive.kind else {
            panic!("expected a plan");
        };
        let completion = ScriptedPlanner.complete(&turn).unwrap();
        let drive = agent.on_completion(completion, &mut view, &read_gate);
        assert!(matches!(drive.kind, DriveKind::AwaitConfirm));
        assert!(drive.effects.is_empty());
        let drive = agent.confirm(&mut view, &|_| false);
        assert!(matches!(drive.kind, DriveKind::Failed(_)));
        assert!(!view.night);
        assert_eq!(drive.receipts[0].decision, Decision::Denied);
        assert!(drive.receipts[0].outcome.contains("w3cos-agent-act"));
    }

    #[test]
    fn read_tools_wait_for_confirm_and_cancel_drops_the_pending_step() {
        let mut agent = AgentLoop::default();
        let mut view = ShellView::default();
        let completion = PlannerCompletion {
            step: PlannerStep::Calls(vec![PlannedCall {
                id: "r1".into(),
                name: "a11y_index".into(),
                arguments: serde_json::json!({}),
            }]),
            first_token_ms: Some(4),
            total_ms: 9,
        };
        let _ = agent.start("look");
        let drive = agent.on_completion(completion, &mut view, &read_gate);
        assert!(matches!(drive.kind, DriveKind::AwaitConfirm));
        assert!(drive.effects.is_empty());
        assert!(drive.receipts.is_empty());
        assert!(drive.steps.contains("a11y_index"));
        let drive = agent.confirm(&mut view, &read_gate);
        assert!(matches!(drive.kind, DriveKind::Plan(_)));
        assert_eq!(drive.receipts[0].decision, Decision::Allowed);
        assert!(drive.receipts[0].confirmed);
        assert!(drive.receipts[0].outcome.contains("index"));

        let completion = PlannerCompletion {
            step: PlannerStep::Calls(vec![PlannedCall {
                id: "c1".into(),
                name: "click".into(),
                arguments: serde_json::json!({"index": 4}),
            }]),
            first_token_ms: None,
            total_ms: 3,
        };
        let drive = agent.on_completion(completion, &mut view, &read_gate);
        assert!(matches!(drive.kind, DriveKind::AwaitConfirm));
        let drive = agent.cancel();
        assert!(matches!(drive.kind, DriveKind::Cancelled));
        assert_eq!(drive.receipts[0].decision, Decision::Cancelled);
        assert!(!view.night);
        assert!(!matches!(
            drive.effects.first(),
            Some(ShellEffect::SetNight(true))
        ));
    }
}
