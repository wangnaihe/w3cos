use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

/// A pseudo-terminal session for interactive shell access.
pub struct PseudoTerminal {
    master_fd: std::os::fd::OwnedFd,
    child_pid: u32,
    cols: u16,
    rows: u16,
}

impl PseudoTerminal {
    /// Write data to the PTY (sends to the shell's stdin).
    pub fn write(&mut self, data: &str) -> Result<(), String> {
        use std::os::fd::AsFd;
        let mut f = std::fs::File::from(
            self.master_fd.as_fd().try_clone_to_owned().map_err(|e| e.to_string())?
        );
        f.write_all(data.as_bytes()).map_err(|e| e.to_string())?;
        f.flush().map_err(|e| e.to_string())
    }

    /// Read available output from the PTY (non-blocking).
    pub fn read(&mut self) -> Result<String, String> {
        use std::os::fd::AsRawFd;
        let fd = self.master_fd.as_raw_fd();

        // Set non-blocking
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }

        let mut buf = [0u8; 4096];
        let mut output = String::new();

        loop {
            let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n <= 0 {
                break;
            }
            output.push_str(&String::from_utf8_lossy(&buf[..n as usize]));
        }

        // Restore blocking
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK);
        }

        Ok(output)
    }

    /// Resize the terminal.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), String> {
        use std::os::fd::AsRawFd;
        self.cols = cols;
        self.rows = rows;
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let ret =
            unsafe { libc::ioctl(self.master_fd.as_raw_fd(), libc::TIOCSWINSZ, &ws as *const _) };
        if ret < 0 {
            Err("ioctl TIOCSWINSZ failed".to_string())
        } else {
            Ok(())
        }
    }

    /// Kill the shell process.
    pub fn kill(&self) -> Result<(), String> {
        let ret = unsafe { libc::kill(self.child_pid as i32, libc::SIGTERM) };
        if ret < 0 {
            Err("kill failed".to_string())
        } else {
            Ok(())
        }
    }

    pub fn pid(&self) -> u32 {
        self.child_pid
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }
}

impl Drop for PseudoTerminal {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}

#[derive(Debug, Clone)]
pub struct PtyOptions {
    pub shell: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
}

impl Default for PtyOptions {
    fn default() -> Self {
        Self {
            shell: None,
            cols: 80,
            rows: 24,
            cwd: None,
            env: Vec::new(),
        }
    }
}

/// Create a new pseudo-terminal with an interactive shell.
pub fn create(options: PtyOptions) -> Result<PseudoTerminal, String> {
    use std::os::fd::FromRawFd;

    let mut master_fd: libc::c_int = 0;
    let mut slave_fd: libc::c_int = 0;

    let ret = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ret < 0 {
        return Err("openpty failed".to_string());
    }

    // Set initial size
    let ws = libc::winsize {
        ws_row: options.rows,
        ws_col: options.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe {
        libc::ioctl(master_fd, libc::TIOCSWINSZ, &ws as *const _);
    }

    let shell = options
        .shell
        .or_else(|| std::env::var("SHELL").ok())
        .unwrap_or_else(|| "/bin/sh".to_string());

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err("fork failed".to_string());
    }

    if pid == 0 {
        // Child process
        unsafe {
            libc::close(master_fd);
            libc::setsid();
            libc::ioctl(slave_fd, libc::TIOCSCTTY as libc::c_ulong, 0);
            libc::dup2(slave_fd, 0);
            libc::dup2(slave_fd, 1);
            libc::dup2(slave_fd, 2);
            if slave_fd > 2 {
                libc::close(slave_fd);
            }
        }

        let mut cmd = Command::new(&shell);
        cmd.arg("-l"); // login shell

        if let Some(ref cwd) = options.cwd {
            cmd.current_dir(cwd);
        }
        for (k, v) in &options.env {
            cmd.env(k, v);
        }
        cmd.env("TERM", "xterm-256color");

        cmd.stdin(Stdio::inherit());
        cmd.stdout(Stdio::inherit());
        cmd.stderr(Stdio::inherit());

        let _ = cmd.exec();
        std::process::exit(1);
    }

    // Parent
    unsafe {
        libc::close(slave_fd);
    }

    let master = unsafe { std::os::fd::OwnedFd::from_raw_fd(master_fd) };

    Ok(PseudoTerminal {
        master_fd: master,
        child_pid: pid as u32,
        cols: options.cols,
        rows: options.rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_pty_and_read() {
        let mut pty = create(PtyOptions::default()).expect("failed to create PTY");
        assert!(pty.pid() > 0);
        assert_eq!(pty.cols(), 80);
        assert_eq!(pty.rows(), 24);

        // Send a command
        pty.write("echo hello_pty\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));

        let output = pty.read().unwrap();
        assert!(
            output.contains("hello_pty"),
            "expected output to contain 'hello_pty', got: {output}"
        );

        pty.kill().unwrap();
    }

    #[test]
    fn resize_pty() {
        let mut pty = create(PtyOptions::default()).unwrap();
        assert!(pty.resize(120, 40).is_ok());
        assert_eq!(pty.cols(), 120);
        assert_eq!(pty.rows(), 40);
        pty.kill().unwrap();
    }
}
