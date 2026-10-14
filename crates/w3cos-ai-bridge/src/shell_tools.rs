//! In-process execution of the MCP tool catalog against shell state.
//!
//! Tool names stay the ones `w3cos-ai-mcp` already exposes. This module does
//! not open a socket: the UI thread runs a call only after the capability
//! gate and, for mutations, a confirm. Process spawn is an allowlist
//! (`echo`, `pwd`, `help`) behind `w3cos-process`, not a shell.

use serde_json::{Value, json};

use crate::dom_access::ActionType;
use crate::intent::{self, ShellApp, ShellContext};
use crate::mcp;
use crate::permissions::BridgeOp;

pub const CAP_A11Y: &str = "w3cos-accessibility";
pub const CAP_CAPTURE: &str = "display-capture";
pub const CAP_ACT: &str = "w3cos-agent-act";
pub const CAP_PROCESS: &str = "w3cos-process";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellView {
    pub app: ShellApp,
    pub night: bool,
    pub terminal_line: String,
    pub terminal_output: String,
}

impl Default for ShellView {
    fn default() -> Self {
        Self {
            app: ShellApp::Desktop,
            night: false,
            terminal_line: String::new(),
            terminal_output: String::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellEffect {
    Navigate(ShellApp),
    SetNight(bool),
    SetTerminalLine(String),
    SetTerminalOutput(String),
}

pub fn apply_effects(view: &mut ShellView, effects: &[ShellEffect]) {
    for effect in effects {
        match effect {
            ShellEffect::Navigate(app) => view.app = *app,
            ShellEffect::SetNight(night) => view.night = *night,
            ShellEffect::SetTerminalLine(line) => view.terminal_line = line.clone(),
            ShellEffect::SetTerminalOutput(output) => view.terminal_output = output.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum ControlKind {
    Propose(&'static str),
    TerminalInput,
    RunTerminal,
    Info,
}

#[derive(Clone, Debug)]
pub struct Control {
    pub index: u32,
    pub role: &'static str,
    pub name: String,
    pub kind: ControlKind,
}

#[derive(Clone, Debug)]
pub struct PreparedTool {
    pub name: String,
    pub arguments: Value,
    pub summary: String,
    pub impact: String,
    pub mutating: bool,
    pub capability: &'static str,
}

pub fn controls(view: &ShellView) -> Vec<Control> {
    let mut rows = vec![
        propose(1, "打开文件 Files", "open files"),
        propose(2, "打开终端 Terminal", "open terminal"),
        propose(3, "打开设置 Settings", "open settings"),
        propose(4, "夜间模式 Night", "night mode"),
        propose(5, "应用列表 Apps", "list apps"),
        propose(6, "我在哪 Where", "where am i"),
    ];
    match view.app {
        ShellApp::Terminal => {
            rows.push(Control {
                index: 7,
                role: "textbox",
                name: "terminal command".into(),
                kind: ControlKind::TerminalInput,
            });
            rows.push(Control {
                index: 8,
                role: "button",
                name: "运行 Run".into(),
                kind: ControlKind::RunTerminal,
            });
        }
        ShellApp::Settings => {
            rows.push(Control {
                index: 7,
                role: "text",
                name: format!(
                    "夜间模式 Night {}",
                    if view.night { "开 On" } else { "关 Off" }
                ),
                kind: ControlKind::Info,
            });
            rows.push(propose(8, "切换夜间 Toggle night", "night mode"));
        }
        ShellApp::Files => {
            for (offset, name) in ["README.md", "Cargo.toml", "app.tsx"].iter().enumerate() {
                rows.push(Control {
                    index: 7 + offset as u32,
                    role: "text",
                    name: (*name).into(),
                    kind: ControlKind::Info,
                });
            }
        }
        _ => {}
    }
    rows
}

fn propose(index: u32, name: &str, utterance: &'static str) -> Control {
    Control {
        index,
        role: "button",
        name: name.into(),
        kind: ControlKind::Propose(utterance),
    }
}

pub fn index_json(view: &ShellView) -> String {
    let entries: Vec<_> = controls(view)
        .iter()
        .map(|control| {
            json!({
                "index": control.index,
                "role": control.role,
                "name": control.name,
                "interactive": !matches!(control.kind, ControlKind::Info),
            })
        })
        .collect();
    serde_json::to_string(&entries).unwrap_or_else(|_| "[]".into())
}

pub fn observe_text(view: &ShellView) -> String {
    let mut text = format!(
        "Active app: {}. Night mode: {}.\n",
        view.app.label(),
        if view.night { "on" } else { "off" }
    );
    if !view.terminal_line.is_empty() {
        text.push_str(&format!("Terminal line: {}\n", view.terminal_line));
    }
    if !view.terminal_output.is_empty() {
        text.push_str(&format!("Terminal output: {}\n", view.terminal_output));
    }
    text.push_str("Controls:\n");
    for control in controls(view) {
        text.push_str(&format!(
            "[{}] {}: {}\n",
            control.index, control.role, control.name
        ));
    }
    text
}

/// Validate against the MCP catalog, then describe the shell effect.
pub fn prepare(name: &str, arguments: &Value, view: &ShellView) -> Result<PreparedTool, String> {
    let arguments = prefer_index(name, arguments);
    mcp::tool_to_bridge(name, &arguments)?;
    let (summary, impact, mutating, capability) = match name {
        "observe" | "a11y_index" | "query" => (
            format!("读取 {name} · Read {name}"),
            "只读无障碍索引，不改桌面。Reads the shell index.".into(),
            false,
            CAP_A11Y,
        ),
        "screenshot" | "screenshot_annotated" => (
            format!("截图 {name} · Screenshot"),
            "规划器不接收像素，只记下截图请求。The planner does not receive pixels.".into(),
            false,
            CAP_CAPTURE,
        ),
        "confirm_intent" => (
            "确认由用户按钮完成 · User confirms in the shell".into(),
            "模型不能替用户确认。The model cannot confirm for the user.".into(),
            false,
            CAP_A11Y,
        ),
        "propose_intent" => {
            let utterance = arguments
                .get("utterance")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            (
                format!("提议 {utterance} · Propose {utterance}"),
                "确认后才切换应用或改会话。Nothing changes until confirm.".into(),
                true,
                CAP_ACT,
            )
        }
        "type" => {
            let text = arguments
                .get("text")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            (
                format!("终端输入 {text} · Type into the terminal"),
                "只写入终端输入行，不启动进程。Sets the terminal line only.".into(),
                true,
                CAP_ACT,
            )
        }
        "click" => describe_click(&arguments, view)?,
        other => return Err(format!("unknown tool '{other}'")),
    };
    Ok(PreparedTool {
        name: name.to_string(),
        arguments,
        summary,
        impact,
        mutating,
        capability,
    })
}

fn describe_click(
    arguments: &Value,
    view: &ShellView,
) -> Result<(String, String, bool, &'static str), String> {
    let control = find_control(arguments, view)?;
    match control.kind {
        ControlKind::Info => Err(format!("index {} is not interactive", control.index)),
        ControlKind::TerminalInput => {
            Err("the terminal field takes the type tool, not click".into())
        }
        ControlKind::RunTerminal => Ok((
            "运行终端命令 · Run the terminal line".into(),
            "只允许 echo、pwd、help，且需要一次性的 w3cos-process。Allowlisted commands only."
                .into(),
            true,
            CAP_PROCESS,
        )),
        ControlKind::Propose(utterance) => Ok((
            format!("点击 {} · Click {}", control.name, utterance),
            "确认后才执行这个控件。The control runs after confirm.".into(),
            true,
            CAP_ACT,
        )),
    }
}

pub fn bridge_op(tool: &PreparedTool) -> BridgeOp {
    match tool.name.as_str() {
        "screenshot" | "screenshot_annotated" => BridgeOp::Screenshot,
        "click" | "type" | "propose_intent" => BridgeOp::Mutate {
            action: ActionType::Click,
            selector: format!("shell:l2:{}", tool.name),
        },
        _ => BridgeOp::ReadDom,
    }
}

pub fn perform(
    tool: &PreparedTool,
    view: &ShellView,
) -> Result<(String, Vec<ShellEffect>), String> {
    match tool.name.as_str() {
        "observe" => Ok((observe_text(view), Vec::new())),
        "a11y_index" => Ok((index_json(view), Vec::new())),
        "query" => {
            let selector = tool
                .arguments
                .get("selector")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let hits: Vec<_> = controls(view)
                .into_iter()
                .filter(|control| {
                    control
                        .name
                        .to_lowercase()
                        .contains(&selector.to_lowercase())
                })
                .map(|control| json!({"index": control.index, "name": control.name}))
                .collect();
            Ok((json!({"matches": hits}).to_string(), Vec::new()))
        }
        "screenshot" | "screenshot_annotated" => Ok((
            json!({"note":"pixels stay on the bridge; the planner does not receive image bytes"})
                .to_string(),
            Vec::new(),
        )),
        "confirm_intent" => Ok((
            "confirmation is the user's button; this tool did not run a command".into(),
            Vec::new(),
        )),
        "propose_intent" => {
            let utterance = tool
                .arguments
                .get("utterance")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            run_l0(utterance, view)
        }
        "type" => type_terminal(tool, view),
        "click" => click_control(tool, view),
        other => Err(format!("unknown tool '{other}'")),
    }
}

fn run_l0(utterance: &str, view: &ShellView) -> Result<(String, Vec<ShellEffect>), String> {
    let Some(command) = intent::resolve_l0_command(utterance) else {
        return Ok((
            format!(
                "no L0 command for {utterance:?}. Call propose_intent again with exactly {:?}.",
                suggest_command(utterance)
            ),
            Vec::new(),
        ));
    };
    let (outcome, navigate, night) = intent::command_effect(
        &command,
        &ShellContext {
            app: view.app,
            night: view.night,
        },
    );
    let mut effects = Vec::new();
    if let Some(app) = navigate {
        effects.push(ShellEffect::Navigate(app));
    }
    if let Some(night) = night {
        effects.push(ShellEffect::SetNight(night));
    }
    Ok((outcome, effects))
}

fn type_terminal(
    tool: &PreparedTool,
    view: &ShellView,
) -> Result<(String, Vec<ShellEffect>), String> {
    let text = tool
        .arguments
        .get("text")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if text.is_empty() {
        return Err("missing text".into());
    }
    if text.chars().count() > 200 {
        return Err("terminal text is longer than 200 characters".into());
    }
    if let Some(index) = tool.arguments.get("index").and_then(|value| value.as_u64()) {
        let control = controls(view)
            .into_iter()
            .find(|control| control.index == index as u32)
            .ok_or_else(|| format!("no control at index {index}"))?;
        if !matches!(control.kind, ControlKind::TerminalInput) {
            return Err(format!("index {index} is not the terminal input"));
        }
    } else {
        let selector = tool
            .arguments
            .get("selector")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_lowercase();
        let terminal_field = (selector.is_empty() && view.app == ShellApp::Terminal)
            || selector.contains("terminal")
            || selector.contains("命令");
        if !terminal_field {
            return Err("type target is not the terminal".into());
        }
    }
    Ok((
        format!("Typed {text:?} into the terminal."),
        vec![ShellEffect::SetTerminalLine(text)],
    ))
}

fn click_control(
    tool: &PreparedTool,
    view: &ShellView,
) -> Result<(String, Vec<ShellEffect>), String> {
    let control = find_control(&tool.arguments, view)?;
    match control.kind {
        ControlKind::Propose(utterance) => run_l0(utterance, view),
        ControlKind::RunTerminal => {
            let output = run_allowlisted(&view.terminal_line)?;
            Ok((output.clone(), vec![ShellEffect::SetTerminalOutput(output)]))
        }
        ControlKind::TerminalInput => Err("use the type tool for the terminal field".into()),
        ControlKind::Info => Err(format!("index {} is not interactive", control.index)),
    }
}

fn prefer_index(name: &str, arguments: &Value) -> Value {
    if !matches!(name, "click" | "type" | "query") {
        return arguments.clone();
    }
    let has_index = arguments
        .get("index")
        .and_then(|value| value.as_u64())
        .is_some();
    let has_selector = arguments
        .get("selector")
        .and_then(|value| value.as_str())
        .is_some_and(|selector| !selector.trim().is_empty());
    if !(has_index && has_selector) {
        return arguments.clone();
    }
    let mut arguments = arguments.clone();
    if let Some(object) = arguments.as_object_mut() {
        object.remove("selector");
    }
    arguments
}

fn suggest_command(utterance: &str) -> &'static str {
    let utterance = utterance.to_lowercase();
    if utterance.contains("editor") || utterance.contains("编辑") {
        "open editor"
    } else if utterance.contains("browser") || utterance.contains("浏览器") {
        "open browser"
    } else if utterance.contains("terminal") || utterance.contains("终端") {
        "open terminal"
    } else if utterance.contains("setting") || utterance.contains("设置") {
        "open settings"
    } else if utterance.contains("file") || utterance.contains("文件") {
        "open files"
    } else if utterance.contains("night")
        || utterance.contains("夜间")
        || utterance.contains("dark")
    {
        "night mode"
    } else if utterance.contains("help") {
        "help"
    } else if utterance.contains("where") || utterance.contains("front") || utterance.contains("哪")
    {
        "where am i"
    } else if utterance.contains("app") {
        "list apps"
    } else {
        "open files, open terminal, open settings, open editor, open browser, night mode, list apps, where am i, help"
    }
}

fn find_control(arguments: &Value, view: &ShellView) -> Result<Control, String> {
    if let Some(index) = arguments.get("index").and_then(|value| value.as_u64()) {
        return controls(view)
            .into_iter()
            .find(|control| control.index == index as u32)
            .ok_or_else(|| format!("no control at index {index}"));
    }
    let selector = arguments
        .get("selector")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_lowercase();
    if selector.is_empty() {
        return Err("missing selector or index".into());
    }
    controls(view)
        .into_iter()
        .find(|control| control.name.to_lowercase().contains(&selector))
        .ok_or_else(|| format!("no control matches {selector:?}"))
}

/// `echo`, `pwd`, and `help` only. Metacharacters are refused. There is no shell.
pub fn run_allowlisted(line: &str) -> Result<String, String> {
    let line = line.trim();
    if line.is_empty() {
        return Err("terminal line is empty".into());
    }
    if line.chars().any(|ch| {
        matches!(
            ch,
            ';' | '|' | '&' | '`' | '$' | '\n' | '>' | '<' | '\\' | '(' | ')' | '\'' | '"'
        )
    }) {
        return Err("refused: the command has shell metacharacters".into());
    }
    let mut parts = line.split_whitespace();
    let command = parts.next().unwrap_or("");
    let args: Vec<&str> = parts.collect();
    match command {
        "help" if args.is_empty() => {
            Ok("help: echo <text>, pwd, help. Other programs are refused.".into())
        }
        "pwd" if args.is_empty() => command_output("pwd", &[]),
        "echo" => command_output("echo", &args),
        other => Err(format!(
            "refused: '{other}' is not allowlisted (echo, pwd, help)"
        )),
    }
}

fn command_output(program: &str, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(format!("{program} exited {:?}", output.status.code()));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::{AgentPermissions, authorize};

    #[test]
    fn terminal_run_is_allowlisted_and_observer_cannot_click() {
        assert_eq!(run_allowlisted("echo hello").unwrap(), "hello");
        assert!(run_allowlisted("echo hello; rm -rf /").is_err());
        assert!(run_allowlisted("bash -c echo").is_err());
        let tool = prepare("click", &json!({"index": 4}), &ShellView::default()).unwrap();
        assert!(tool.mutating);
        assert_eq!(tool.capability, CAP_ACT);
        assert!(authorize(&AgentPermissions::observer(), &bridge_op(&tool)).is_err());
    }

    #[test]
    fn night_click_and_terminal_type_follow_the_catalog() {
        let view = ShellView::default();
        let click = prepare("click", &json!({"index": 4}), &view).unwrap();
        let (outcome, effects) = perform(&click, &view).unwrap();
        assert!(outcome.contains("Night"));
        assert_eq!(effects, vec![ShellEffect::SetNight(true)]);

        let mut terminal = view;
        terminal.app = ShellApp::Terminal;
        let typed = prepare(
            "type",
            &json!({"index": 7, "text": "echo hello"}),
            &terminal,
        )
        .unwrap();
        let (_, effects) = perform(&typed, &terminal).unwrap();
        apply_effects(&mut terminal, &effects);
        assert_eq!(terminal.terminal_line, "echo hello");
        let run = prepare("click", &json!({"index": 8}), &terminal).unwrap();
        assert_eq!(run.capability, CAP_PROCESS);
        let (output, effects) = perform(&run, &terminal).unwrap();
        assert_eq!(output, "hello");
        apply_effects(&mut terminal, &effects);
        assert_eq!(terminal.terminal_output, "hello");
    }
}
