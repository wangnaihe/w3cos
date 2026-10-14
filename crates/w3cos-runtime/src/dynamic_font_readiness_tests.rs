use super::*;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

fn request(socket: &mut TcpStream) -> String {
    socket.set_nonblocking(false).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        socket.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
    }
    String::from_utf8(bytes).unwrap()
}

#[test]
fn ready_waits_for_deferred_second_empty_inline_metrics_face() {
    crate::dom::reset_document();
    crate::jsdom::reset_bridge();
    w3cos_dom::stylesheet::clear_rules();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        assert!(request(&mut socket).starts_with("GET /index.html "));
        let html = r#"<!doctype html><style>
        @font-face {font-family:'Ready First';src:url('/one.ttf')}
        @font-face {font-family:'Ready Second';src:url('/two.ttf')}
        div {display:inline-block} span {border-top:1px solid;padding-left:1em}
        div:first-of-type {font-family:'Ready First'}
        div:nth-of-type(2) {font-family:'Ready Second'}
        </style><div><span></span></div><div><span></span></div>"#;
        write!(socket,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n{html}",html.len()).unwrap();
        drop(socket);
        listener.set_nonblocking(true).unwrap();
        let mut requests = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while requests.len() < 2 && Instant::now() < deadline {
            let Ok((mut socket, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            };
            let url = request(&mut socket);
            requests.push(url);
            if requests.len() == 2 {
                std::thread::sleep(Duration::from_millis(100));
            }
            let font = include_bytes!("../assets/Inter-Regular.ttf");
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: font/ttf\r\nConnection: close\r\n\r\n",font.len()).unwrap();
            // A failed client assertion may close an in-flight response.
            let _ = socket.write_all(font);
        }
        requests
    });
    let mut loader = DocumentLoader::new(ScriptPolicy::default(), DocumentLoaderOptions::default());
    loader
        .navigate(&format!("http://{address}/index.html"))
        .unwrap();
    while !matches!(loader.poll(), DocumentLoadProgress::Complete) {
        std::thread::sleep(Duration::from_millis(1));
    }
    let ready = crate::jsdom::document_value()
        .get_property("fonts")
        .get_property("ready");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !matches!(
        w3cos_core::promise::status(&ready),
        Some(w3cos_core::promise::PromiseStatus::Fulfilled(_))
    ) {
        poll_script_fetches();
        crate::jsdom::drain_microtasks();
        assert!(Instant::now() < deadline, "font readiness timeout");
        std::thread::sleep(Duration::from_millis(1));
    }
    let registry = crate::font_face::FontRegistry::global();
    let second = registry
        .resolve(
            "Ready Second",
            crate::font_face::FontWeight::NORMAL,
            crate::font_face::FontFaceStyle::Normal,
        )
        .is_some();
    // Drain the server even on RED so no fixture process/thread is left behind.
    let drain = Instant::now() + Duration::from_millis(250);
    while Instant::now() < drain {
        poll_script_fetches();
        std::thread::sleep(Duration::from_millis(1));
    }
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        second,
        "ready resolved before second empty-inline metrics face loaded"
    );
}
