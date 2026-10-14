//! UI-thread side of the L2 agent.
//!
//! `begin` returns immediately. The planner runs on `w3cos-l2` and wakes the
//! window. Mutating tools do not run until confirm grants `w3cos-agent-act`
//! (and `w3cos-process` for the terminal allowlist) for that call only.

use w3cos_ai_bridge::agent_loop::{self, Drive, DriveKind, ReceiptDraft};
use w3cos_ai_bridge::planner::{self, L2Mode};
use w3cos_ai_bridge::shell_tools::{ShellEffect, ShellView};

pub struct HostUpdate {
    pub phase: i64,
    pub notice: String,
    pub summary: String,
    pub impact: String,
    pub tier: &'static str,
    pub steps: String,
    pub open_tasks: bool,
    pub effects: Vec<ShellEffect>,
    pub receipts: Vec<ReceiptDraft>,
    pub utterance: String,
}

pub fn install_wake() {
    agent_loop::set_wake(poke_window);
}

fn poke_window() {
    w3cos_runtime::uitest::poke_event_loop();
}

pub fn begin(utterance: &str) -> Result<HostUpdate, String> {
    match planner::mode_from_env()? {
        L2Mode::Off => return Err("L2 is off".into()),
        L2Mode::Script | L2Mode::Remote(_) => {}
    }
    Ok(from_drive(agent_loop::begin_run(utterance)))
}

pub fn poll(view: &mut ShellView, gate: &dyn Fn(&str) -> bool) -> Option<HostUpdate> {
    let mail = agent_loop::take_mail()?;
    let drive = match mail {
        Ok(completion) => agent_loop::on_model(completion, view, gate),
        Err(err) => {
            return Some(failed(err));
        }
    };
    Some(from_drive(drive))
}

pub fn confirm(view: &mut ShellView, gate: &dyn Fn(&str) -> bool) -> HostUpdate {
    from_drive(agent_loop::confirm_run(view, gate))
}

pub fn reject(reason: &str) -> HostUpdate {
    from_drive(agent_loop::reject_run(reason))
}

pub fn cancel() -> HostUpdate {
    from_drive(agent_loop::cancel_run())
}

fn failed(notice: String) -> HostUpdate {
    HostUpdate {
        phase: 2,
        notice: notice.clone(),
        summary: String::new(),
        impact: String::new(),
        tier: "L2",
        steps: notice,
        open_tasks: true,
        effects: Vec::new(),
        receipts: Vec::new(),
        utterance: String::new(),
    }
}

fn from_drive(drive: Drive) -> HostUpdate {
    let utterance = drive.utterance;
    let effects = drive.effects;
    let receipts = drive.receipts;
    let steps = drive.steps;
    let summary = drive.summary;
    let impact = drive.impact;
    let notice = drive.notice;
    let (phase, tier, open_tasks) = match drive.kind {
        DriveKind::Plan(turn) => {
            agent_loop::spawn_plan(turn);
            (3, "L2", true)
        }
        DriveKind::AwaitConfirm => (1, "L2", true),
        DriveKind::Finished(_) | DriveKind::Failed(_) => (2, "L2", true),
        DriveKind::Cancelled => (0, "", true),
    };
    HostUpdate {
        phase,
        notice,
        summary,
        impact,
        tier,
        steps,
        open_tasks,
        effects,
        receipts,
        utterance,
    }
}
