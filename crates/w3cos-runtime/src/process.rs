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
