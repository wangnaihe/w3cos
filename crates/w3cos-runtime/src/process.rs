use std::collections::HashMap;
use std::io::Read as IoRead;
use std::process::{Command, Stdio};

/// Result of running a command to completion.
#[derive(Debug, Clone)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub ok: bool,
}

/// A handle to a spawned child process.
pub struct ChildProcess {
    child: std::process::Child,
}

impl ChildProcess {
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// Wait for the process to finish and collect output.
    pub fn wait_with_output(self) -> ExecResult {
        match self.child.wait_with_output() {
            Ok(output) => {
                let code = output.status.code().unwrap_or(-1);
                ExecResult {
                    stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                    stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                    exit_code: code,
                    ok: output.status.success(),
                }
            }
            Err(e) => ExecResult {
                stdout: String::new(),
                stderr: e.to_string(),
                exit_code: -1,
                ok: false,
            },
        }
    }

    /// Kill the child process.
    pub fn kill(&mut self) -> Result<(), String> {
        self.child.kill().map_err(|e| e.to_string())
    }

    /// Check if still running (non-blocking).
    pub fn try_wait(&mut self) -> Option<i32> {
        self.child
            .try_wait()
            .ok()
            .flatten()
            .map(|s| s.code().unwrap_or(-1))
    }

    /// Read available stdout (non-blocking, returns what's available).
    pub fn read_stdout(&mut self) -> String {
        read_available(&mut self.child.stdout)
    }

    /// Read available stderr (non-blocking).
    pub fn read_stderr(&mut self) -> String {
        read_available(&mut self.child.stderr)
    }

    /// Write to stdin.
    pub fn write_stdin(&mut self, data: &str) -> Result<(), String> {
        use std::io::Write;
        if let Some(ref mut stdin) = self.child.stdin {
            stdin
                .write_all(data.as_bytes())
                .map_err(|e| e.to_string())?;
            stdin.flush().map_err(|e| e.to_string())
        } else {
            Err("stdin not piped".to_string())
        }
    }
}

fn read_available<R: IoRead>(reader: &mut Option<R>) -> String {
    if let Some(r) = reader {
        let mut buf = String::new();
        let _ = r.read_to_string(&mut buf);
        buf
    } else {
        String::new()
    }
}

#[derive(Debug, Clone, Default)]
pub struct SpawnOptions {
    pub cwd: Option<String>,
    pub env: HashMap<String, String>,
    pub pipe_stdin: bool,
    pub pipe_stdout: bool,
    pub pipe_stderr: bool,
}

/// Spawn a child process (non-blocking).
pub fn spawn(program: &str, args: &[&str], options: SpawnOptions) -> Result<ChildProcess, String> {
    let mut cmd = Command::new(program);
    cmd.args(args);

    if let Some(ref cwd) = options.cwd {
        cmd.current_dir(cwd);
    }
    for (k, v) in &options.env {
        cmd.env(k, v);
    }

    cmd.stdin(if options.pipe_stdin {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.stdout(if options.pipe_stdout {
        Stdio::piped()
    } else {
        Stdio::inherit()
    });
    cmd.stderr(if options.pipe_stderr {
        Stdio::piped()
    } else {
        Stdio::inherit()
    });

    let child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;
    Ok(ChildProcess { child })
}

/// Execute a command and wait for completion, capturing all output.
pub fn exec(program: &str, args: &[&str]) -> ExecResult {
    exec_with_options(program, args, SpawnOptions::default())
}

/// Execute with custom options.
pub fn exec_with_options(program: &str, args: &[&str], options: SpawnOptions) -> ExecResult {
    let mut cmd = Command::new(program);
    cmd.args(args);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    if let Some(ref cwd) = options.cwd {
        cmd.current_dir(cwd);
    }
    for (k, v) in &options.env {
        cmd.env(k, v);
    }

    match cmd.output() {
        Ok(output) => {
            let code = output.status.code().unwrap_or(-1);
            ExecResult {
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                exit_code: code,
                ok: output.status.success(),
            }
        }
        Err(e) => ExecResult {
            stdout: String::new(),
            stderr: e.to_string(),
            exit_code: -1,
            ok: false,
        },
    }
}

/// Execute a shell command string via `sh -c`.
pub fn exec_shell(command: &str) -> ExecResult {
    exec("sh", &["-c", command])
}

// ---------------------------------------------------------------------------
// Process info (w3cos.process.list / getpid / kill)
// ---------------------------------------------------------------------------

/// Information about a running process.
#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub cpu_usage: f32,
    pub memory_bytes: u64,
    pub status: String,
}

/// List all running processes on the system.
pub fn list_processes() -> Vec<ProcessInfo> {
    use sysinfo::System;
    let mut sys = System::new_all();
    sys.refresh_all();
    sys.processes()
        .values()
        .map(|p| ProcessInfo {
            pid: p.pid().as_u32(),
            name: p.name().to_string_lossy().to_string(),
            cpu_usage: p.cpu_usage(),
            memory_bytes: p.memory(),
            status: format!("{:?}", p.status()),
        })
        .collect()
}

/// Get the current process PID.
pub fn getpid() -> u32 {
    std::process::id()
}

/// Get CPU and memory usage for a specific PID.
pub fn process_usage(pid: u32) -> Option<(f32, u64)> {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let mut sys = System::new();
    let spid = Pid::from_u32(pid);
    sys.refresh_processes(ProcessesToUpdate::Some(&[spid]), false);
    sys.process(spid).map(|p| (p.cpu_usage(), p.memory()))
}

/// Send a signal to a process by PID.
///
/// Common signals: 9 = SIGKILL, 15 = SIGTERM, 2 = SIGINT
pub fn kill(pid: u32, signal: i32) -> Result<(), String> {
    #[cfg(unix)]
    {
        use nix::sys::signal::{kill as nix_kill, Signal};
        use nix::unistd::Pid;
        let sig = Signal::try_from(signal).map_err(|e| e.to_string())?;
        nix_kill(Pid::from_raw(pid as i32), sig).map_err(|e| e.to_string())
    }
    #[cfg(not(unix))]
    {
        // On non-Unix, fall back to TerminateProcess via std
        let _ = signal;
        let mut child = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .output();
        child.map(|_| ()).map_err(|e| e.to_string())
    }
}

/// Set an environment variable for the current process.
pub fn setenv(key: &str, value: &str) {
    unsafe { std::env::set_var(key, value) };
}

/// Remove an environment variable from the current process.
pub fn unsetenv(key: &str) {
    unsafe { std::env::remove_var(key) };
}

/// Get an environment variable.
pub fn getenv(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

// ---------------------------------------------------------------------------
// Pipe chain (w3cos.process.pipe)
// ---------------------------------------------------------------------------

/// Run a pipeline of shell commands connected by pipes.
///
/// Each element is a `(program, args)` pair. The stdout of each command
/// is piped into the stdin of the next. Returns the final output.
///
/// # Example
/// ```ignore
/// let result = pipe_commands(&[
///     ("echo", vec!["hello world"]),
///     ("tr", vec!["a-z", "A-Z"]),
/// ]);
/// assert_eq!(result.stdout.trim(), "HELLO WORLD");
/// ```
pub fn pipe_commands(commands: &[(&str, Vec<&str>)]) -> ExecResult {
    if commands.is_empty() {
        return ExecResult { stdout: String::new(), stderr: String::new(), exit_code: 0, ok: true };
    }

    // Build shell pipeline string: cmd1 args | cmd2 args | ...
    let pipeline: String = commands
        .iter()
        .map(|(prog, args)| {
            let mut parts = vec![shell_escape(prog)];
            parts.extend(args.iter().map(|a| shell_escape(a)));
            parts.join(" ")
        })
        .collect::<Vec<_>>()
        .join(" | ");

    exec_shell(&pipeline)
}

fn shell_escape(s: &str) -> String {
    // Simple single-quote escaping for shell safety
    format!("'{}'", s.replace('\'', "'\\''"))
}

// ---------------------------------------------------------------------------
// System info (w3cos.process.sysinfo)
// ---------------------------------------------------------------------------

/// Basic system information.
#[derive(Debug, Clone)]
pub struct SysInfo {
    pub total_memory_bytes: u64,
    pub used_memory_bytes: u64,
    pub cpu_count: usize,
    pub os_name: String,
    pub kernel_version: String,
    pub hostname: String,
}

/// Get system-level information.
pub fn sysinfo() -> SysInfo {
    use sysinfo::System;
    let mut sys = System::new_all();
    sys.refresh_all();
    SysInfo {
        total_memory_bytes: sys.total_memory(),
        used_memory_bytes: sys.used_memory(),
        cpu_count: sys.cpus().len(),
        os_name: System::name().unwrap_or_default(),
        kernel_version: System::kernel_version().unwrap_or_default(),
        hostname: System::host_name().unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_echo() {
        let result = exec("echo", &["hello", "w3cos"]);
        assert!(result.ok);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "hello w3cos");
    }

    #[test]
    fn exec_shell_pipe() {
        let result = exec_shell("echo 'test' | tr 'a-z' 'A-Z'");
        assert!(result.ok);
        assert_eq!(result.stdout.trim(), "TEST");
    }

    #[test]
    fn exec_nonexistent() {
        let result = exec("nonexistent_program_w3cos", &[]);
        assert!(!result.ok);
    }

    #[test]
    fn exec_with_cwd() {
        let result = exec_with_options(
            "pwd",
            &[],
            SpawnOptions {
                cwd: Some("/tmp".to_string()),
                ..Default::default()
            },
        );
        assert!(result.ok);
        assert!(result.stdout.trim().contains("tmp"));
    }

    #[test]
    fn spawn_and_wait() {
        let child = spawn(
            "echo",
            &["spawn_test"],
            SpawnOptions {
                pipe_stdout: true,
                ..Default::default()
            },
        )
        .unwrap();
        let result = child.wait_with_output();
        assert!(result.ok);
        assert_eq!(result.stdout.trim(), "spawn_test");
    }
}
