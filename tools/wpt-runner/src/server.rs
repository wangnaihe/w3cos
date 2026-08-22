use anyhow::{Context, Result, bail};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const TESTHARNESS_REPORTER: &str = r#"(function () {
  var collected = [];
  window.__w3cos_wpt_results_json = "";
  add_result_callback(function (test) {
    collected.push({
      name: test.name,
      status: test.status,
      message: test.message
    });
  });
  add_completion_callback(function (_tests, status) {
    window.__w3cos_wpt_results_json = JSON.stringify({
      harness_status: status.status,
      harness_message: status.message,
      tests: collected
    });
  });
}());
"#;

pub struct StaticServer {
    address: SocketAddr,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl StaticServer {
    pub fn start(root: PathBuf) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").context("failed to bind WPT server")?;
        listener
            .set_nonblocking(true)
            .context("failed to configure WPT server")?;
        let address = listener
            .local_addr()
            .context("missing WPT server address")?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = Arc::clone(&shutdown);
        let thread = thread::spawn(move || {
            while !thread_shutdown.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if thread_shutdown.load(Ordering::Acquire) {
                            break;
                        }
                        if let Err(error) = serve_connection(stream, &root) {
                            eprintln!("W3COS_WPT_SERVER_ERROR {error:#}");
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => {
                        eprintln!("W3COS_WPT_SERVER_ERROR accept failed: {error}");
                        break;
                    }
                }
            }
        });
        Ok(Self {
            address,
            shutdown,
            thread: Some(thread),
        })
    }

    pub fn url_for(&self, path: &str) -> String {
        format!("http://{}/{}", self.address, path)
    }
}

impl Drop for StaticServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve_connection(mut stream: TcpStream, root: &Path) -> Result<()> {
    stream
        .set_nonblocking(false)
        .context("failed to configure WPT connection")?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .context("failed to set WPT request timeout")?;
    let mut request = Vec::new();
    let mut chunk = [0_u8; 4096];
    while request.len() < 64 * 1024 {
        let read = stream
            .read(&mut chunk)
            .context("failed to read WPT request")?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let request = String::from_utf8_lossy(&request);
    let mut request_line = request
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace();
    let method = request_line.next().unwrap_or_default();
    let target = request_line.next().unwrap_or_default();
    if !matches!(method, "GET" | "HEAD") {
        return write_response(
            &mut stream,
            method,
            405,
            "text/plain",
            b"method not allowed",
        );
    }
    let path = target.split('?').next().unwrap_or_default();
    let relative = match safe_relative_path(path) {
        Ok(path) => path,
        Err(_) => return write_response(&mut stream, method, 400, "text/plain", b"bad path"),
    };
    if relative == Path::new("resources/testharnessreport.js") {
        return write_response(
            &mut stream,
            method,
            200,
            "text/javascript; charset=utf-8",
            TESTHARNESS_REPORTER.as_bytes(),
        );
    }

    let file = root.join(&relative);
    if !file.is_file() {
        return write_response(&mut stream, method, 404, "text/plain", b"not found");
    }
    let body = std::fs::read(&file)
        .with_context(|| format!("failed to read WPT resource {}", file.display()))?;
    write_response(&mut stream, method, 200, content_type(&file), &body)
}

fn write_response(
    stream: &mut TcpStream,
    method: &str,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .context("failed to write WPT response headers")?;
    if method != "HEAD" {
        stream
            .write_all(body)
            .context("failed to write WPT response body")?;
    }
    Ok(())
}

fn safe_relative_path(target: &str) -> Result<PathBuf> {
    let decoded = percent_decode(target)?;
    let trimmed = decoded.strip_prefix('/').unwrap_or(&decoded);
    let path = Path::new(trimmed);
    if trimmed.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("unsafe WPT request path");
    }
    Ok(path.to_path_buf())
}

fn percent_decode(input: &str) -> Result<String> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            bail!("truncated percent escape");
        }
        let high = hex(bytes[index + 1])?;
        let low = hex(bytes[index + 2])?;
        decoded.push((high << 4) | low);
        index += 3;
    }
    String::from_utf8(decoded).context("WPT request path is not UTF-8")
}

fn hex(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!("invalid percent escape"),
    }
}

fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "xht" | "xhtml" => "application/xhtml+xml; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_normal_paths_and_rejects_traversal() {
        assert_eq!(
            safe_relative_path("/dom/a%20b.html").unwrap(),
            Path::new("dom/a b.html")
        );
        assert!(safe_relative_path("/../secret").is_err());
        assert!(safe_relative_path("/%2e%2e/secret").is_err());
        assert!(safe_relative_path("/%zz").is_err());
    }

    #[test]
    fn serves_files_and_overrides_only_the_reporter() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("resources")).unwrap();
        std::fs::write(
            directory.path().join("resources/testharness.js"),
            "upstream-harness",
        )
        .unwrap();
        let server = StaticServer::start(directory.path().to_path_buf()).unwrap();

        let harness = http_get(&server.url_for("resources/testharness.js"));
        assert!(harness.ends_with("upstream-harness"));
        let reporter = http_get(&server.url_for("resources/testharnessreport.js"));
        assert!(reporter.contains("__w3cos_wpt_results_json"));
    }

    fn http_get(url: &str) -> String {
        let suffix = url.strip_prefix("http://").unwrap();
        let separator = suffix.find('/').unwrap();
        let address = &suffix[..separator];
        let path = &suffix[separator..];
        let mut stream = TcpStream::connect(address).unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response.split("\r\n\r\n").nth(1).unwrap().to_string()
    }
}
