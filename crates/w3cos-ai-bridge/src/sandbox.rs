//! A separate process for a program that is not `echo`, `pwd`, or `help`.
//!
//! The child starts with an empty environment, so a device secret in the
//! parent environment is not visible. Files and the network stay closed
//! unless the permission sheet granted that one capability. The wallet is
//! not in this module.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use std::os::unix::process::CommandExt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SandboxCaps {
    pub filesystem: bool,
    pub network: bool,
}

const MAX_OUTPUT: usize = 8 * 1024;
const MAX_ARGS: usize = 16;

/// Run `program` once. `Err("missing")` means it was not found. `Err("timeout")`
/// means it ran too long. Other errors mean it did not finish cleanly.
pub fn run(program: &str, args: &[String], caps: SandboxCaps) -> Result<String, String> {
    if args.len() > MAX_ARGS
        || args
            .iter()
            .any(|arg| arg.len() > 1024 || arg.contains('\0'))
    {
        return Err("failed".into());
    }
    let exe = resolve_program(program).ok_or_else(|| "missing".to_string())?;
    let staged = if caps.filesystem {
        None
    } else {
        Some(stage_root(&exe)?)
    };
    let result = spawn(&exe, args, caps, staged.as_deref());
    if let Some(dir) = staged {
        let _ = fs::remove_dir_all(dir);
    }
    result
}

fn spawn(
    exe: &Path,
    args: &[String],
    caps: SandboxCaps,
    root: Option<&Path>,
) -> Result<String, String> {
    let unshare = Path::new("/usr/bin/unshare");
    if !unshare.is_file() {
        return Err("failed".into());
    }
    let mut cmd = Command::new(unshare);
    cmd.arg("--user")
        .arg("--map-root-user")
        .arg("--mount")
        .arg("--propagation")
        .arg("private");
    if !caps.network {
        cmd.arg("--net");
    }
    if let Some(root) = root {
        cmd.arg("--root")
            .arg(root)
            .arg("--wd")
            .arg("/")
            .arg("/tool");
    } else {
        cmd.arg(exe);
    }
    cmd.args(args);
    cmd.env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = cmd.spawn().map_err(|_| "failed".to_string())?;
    let pid = child.id() as i32;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_task = thread::spawn(move || read_capped(stdout, MAX_OUTPUT));
    let err_task = thread::spawn(move || read_capped(stderr, 1024));
    let status = wait_child(&mut child, pid)?;
    let output = out_task.join().unwrap_or_default();
    let errors = err_task.join().unwrap_or_default();
    if status {
        return Ok(output.trim_end().to_string());
    }
    if !caps.filesystem && errors.contains("No such file") {
        return Err("no-file".into());
    }
    Err("failed".into())
}

fn wait_child(child: &mut std::process::Child, pid: i32) -> Result<bool, String> {
    let start = Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(5) {
            unsafe {
                kill(-pid, 9);
            }
            let _ = child.wait();
            return Err("timeout".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.success()),
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(_) => return Err("failed".into()),
        }
    }
}

fn read_capped(pipe: Option<impl Read>, cap: usize) -> String {
    let Some(pipe) = pipe else {
        return String::new();
    };
    let mut buf = Vec::new();
    let _ = pipe.take(cap as u64).read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).to_string()
}

fn resolve_program(program: &str) -> Option<PathBuf> {
    if program.is_empty() || program.len() > 256 || program.contains('\0') {
        return None;
    }
    if program.contains('/') {
        if !program.starts_with('/') || program.split('/').any(|part| part == "..") {
            return None;
        }
        let path = PathBuf::from(program);
        return path.is_file().then_some(path);
    }
    for dir in ["/usr/bin", "/bin"] {
        let path = Path::new(dir).join(program);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

fn stage_root(exe: &Path) -> Result<PathBuf, String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let root = std::env::temp_dir().join(format!("w3cos-sb-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&root).map_err(|_| "failed".to_string())?;
    copy_file(exe, &root.join("tool"))?;
    let mut pending = vec![exe.to_path_buf()];
    let mut seen = std::collections::BTreeSet::new();
    while let Some(path) = pending.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        let (interp, needed) = elf_deps(&path).map_err(|_| "failed".to_string())?;
        if let Some(interp) = interp {
            let interp_path = PathBuf::from(&interp);
            copy_into(&root, &interp_path)?;
            pending.push(interp_path);
        }
        for name in needed {
            let lib = resolve_lib(&name).ok_or_else(|| "failed".to_string())?;
            copy_into(&root, &lib)?;
            pending.push(lib);
        }
    }
    Ok(root)
}

fn copy_into(root: &Path, source: &Path) -> Result<(), String> {
    let rel = source.strip_prefix("/").map_err(|_| "failed".to_string())?;
    let dest = root.join(rel);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|_| "failed".to_string())?;
    }
    copy_file(source, &dest)
}

fn copy_file(source: &Path, dest: &Path) -> Result<(), String> {
    fs::copy(source, dest)
        .map(|_| ())
        .map_err(|_| "failed".to_string())
}

fn resolve_lib(name: &str) -> Option<PathBuf> {
    if name.starts_with('/') {
        let path = PathBuf::from(name);
        return path.is_file().then_some(path);
    }
    for dir in [
        "/lib/x86_64-linux-gnu",
        "/usr/lib/x86_64-linux-gnu",
        "/lib64",
        "/usr/lib64",
        "/lib",
        "/usr/lib",
    ] {
        let path = Path::new(dir).join(name);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

fn elf_deps(path: &Path) -> Result<(Option<String>, Vec<String>), String> {
    let bytes = fs::read(path).map_err(|_| "elf".to_string())?;
    if bytes.len() < 64 || &bytes[0..4] != b"\x7fELF" || bytes[4] != 2 {
        return Ok((None, Vec::new()));
    }
    let phoff = u64_at(&bytes, 32)?;
    let phentsize = u16_at(&bytes, 54)? as usize;
    let phnum = u16_at(&bytes, 56)? as usize;
    let mut interp = None;
    let mut dynamic = None;
    for index in 0..phnum {
        let start = phoff as usize + index * phentsize;
        let kind = u32_at(&bytes, start)?;
        let offset = u64_at(&bytes, start + 8)?;
        let filesz = u64_at(&bytes, start + 32)?;
        if kind == 3 {
            let slice = slice_at(&bytes, offset as usize, filesz as usize)?;
            interp = Some(c_string(slice));
        } else if kind == 2 {
            dynamic = Some((offset as usize, filesz as usize));
        }
    }
    let Some((offset, size)) = dynamic else {
        return Ok((interp, Vec::new()));
    };
    let dynamic_bytes = slice_at(&bytes, offset, size)?.to_vec();
    let mut strtab = 0u64;
    let mut needed_off = Vec::new();
    let mut cursor = 0;
    while cursor + 16 <= dynamic_bytes.len() {
        let tag = i64::from_le_bytes(dynamic_bytes[cursor..cursor + 8].try_into().unwrap());
        let val = u64::from_le_bytes(dynamic_bytes[cursor + 8..cursor + 16].try_into().unwrap());
        if tag == 0 {
            break;
        }
        if tag == 1 {
            needed_off.push(val);
        } else if tag == 5 {
            strtab = val;
        }
        cursor += 16;
    }
    let str_off = vaddr_offset(&bytes, phoff, phentsize, phnum, strtab)?;
    let mut needed = Vec::new();
    for off in needed_off {
        let start = str_off + off as usize;
        if start >= bytes.len() {
            continue;
        }
        let end = bytes[start..]
            .iter()
            .position(|byte| *byte == 0)
            .map(|len| start + len)
            .unwrap_or(bytes.len());
        needed.push(String::from_utf8_lossy(&bytes[start..end]).to_string());
    }
    Ok((interp, needed))
}

fn vaddr_offset(
    bytes: &[u8],
    phoff: u64,
    phentsize: usize,
    phnum: usize,
    vaddr: u64,
) -> Result<usize, String> {
    for index in 0..phnum {
        let start = phoff as usize + index * phentsize;
        let kind = u32_at(bytes, start)?;
        if kind != 1 {
            continue;
        }
        let offset = u64_at(bytes, start + 8)?;
        let virt = u64_at(bytes, start + 16)?;
        let filesz = u64_at(bytes, start + 32)?;
        if vaddr >= virt && vaddr < virt.saturating_add(filesz) {
            return Ok((offset + (vaddr - virt)) as usize);
        }
    }
    Err("elf".into())
}

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16, String> {
    let slice = slice_at(bytes, offset, 2)?;
    Ok(u16::from_le_bytes(slice.try_into().unwrap()))
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let slice = slice_at(bytes, offset, 4)?;
    Ok(u32::from_le_bytes(slice.try_into().unwrap()))
}

fn u64_at(bytes: &[u8], offset: usize) -> Result<u64, String> {
    let slice = slice_at(bytes, offset, 8)?;
    Ok(u64::from_le_bytes(slice.try_into().unwrap()))
}

fn slice_at(bytes: &[u8], offset: usize, len: usize) -> Result<&[u8], String> {
    bytes
        .get(offset..offset + len)
        .ok_or_else(|| "elf".to_string())
}

fn c_string(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).to_string()
}

unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_sandbox_hides_the_environment_files_and_network() {
        let secret = "w3cos-sandbox-secret-value";
        unsafe {
            std::env::set_var("W3COS_DEVICE_SEED", secret);
        }
        let closed = SandboxCaps {
            filesystem: false,
            network: false,
        };
        let printed = run("printenv", &[], closed).unwrap();
        assert!(!printed.contains(secret), "{printed}");
        assert!(!printed.contains("W3COS_DEVICE_SEED"), "{printed}");

        let marker =
            std::env::temp_dir().join(format!("w3cos-sandbox-marker-{}", std::process::id()));
        fs::write(&marker, b"marker-not-a-seed").unwrap();
        let missed = run("cat", &[marker.display().to_string()], closed).unwrap_err();
        assert_eq!(missed, "no-file");

        let opened = SandboxCaps {
            filesystem: true,
            network: false,
        };
        let seen = run("cat", &[marker.display().to_string()], opened).unwrap();
        assert_eq!(seen, "marker-not-a-seed");
        let _ = fs::remove_file(&marker);

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let code =
            format!("import socket; socket.create_connection(('127.0.0.1', {port}), 2).close()");
        let blocked = run("python3", &[String::from("-c"), code.clone()], opened);
        assert!(blocked.is_err(), "{blocked:?}");
        let connected = run(
            "python3",
            &[String::from("-c"), code],
            SandboxCaps {
                filesystem: true,
                network: true,
            },
        );
        assert!(connected.is_ok(), "{connected:?}");
        drop(listener);

        assert!(run("not-a-w3cos-program", &[], closed).unwrap_err() == "missing");
        unsafe {
            std::env::remove_var("W3COS_DEVICE_SEED");
        }
    }
}
