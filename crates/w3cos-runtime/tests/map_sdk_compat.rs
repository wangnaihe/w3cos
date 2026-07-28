#![cfg(feature = "dynamic-js")]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use w3cos_core::Value;
use w3cos_runtime::dynamic_script::{ScriptLoader, ScriptPolicy, has_pending_script_fetches};

fn javascript_response(stream: &mut impl Write, source: &str) {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\n\
         Cache-Control: no-store\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{}",
        source.len(),
        source
    )
    .expect("write JavaScript fixture response");
}

fn request_path(request: &[u8]) -> String {
    String::from_utf8_lossy(request)
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .expect("HTTP request path")
        .to_string()
}

#[test]
fn map_style_bootstrap_loads_jsonp_and_secondary_chunk_through_one_runtime() {
    w3cos_runtime::dom::reset_document();
    w3cos_runtime::jsdom::reset_bridge();

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind map SDK fixture");
    let address = listener.local_addr().expect("map SDK fixture address");
    let server = thread::spawn(move || {
        let mut paths = Vec::new();
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().expect("accept map SDK request");
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).expect("read map SDK request");
            let path = request_path(&request[..read]);
            paths.push(path.clone());
            match path.as_str() {
                "/maps/bootstrap.js" => javascript_response(
                    &mut stream,
                    r#"
                        window.MapFixture = {};
                        window.MapFixture.callback = (payload) => {
                            document.body.setAttribute(
                                "data-map-metadata",
                                payload && payload.status
                                    ? payload.status
                                    : "missing"
                            );
                            const chunk = document.createElement("script");
                            chunk.src = "/maps/chunk.js";
                            chunk.onload = () => {
                                document.body.setAttribute(
                                    "data-map-sdk",
                                    window.MapFixture.version || "missing"
                                );
                            };
                            chunk.onerror = () => {
                                document.body.setAttribute(
                                    "data-map-error",
                                    "chunk"
                                );
                            };
                            document.head.appendChild(chunk);
                        };

                        const jsonp = document.createElement("script");
                        jsonp.src =
                            "/maps/jsonp?callback=MapFixture.callback";
                        jsonp.onerror = () => {
                            document.body.setAttribute(
                                "data-map-error",
                                "jsonp"
                            );
                        };
                        document.head.appendChild(jsonp);
                    "#,
                ),
                "/maps/jsonp?callback=MapFixture.callback" => javascript_response(
                    &mut stream,
                    r#"
                        MapFixture.callback({
                            status: "metadata-ready",
                            center: [116.397, 39.908],
                            zooms: [3, 20]
                        });
                    "#,
                ),
                "/maps/chunk.js" => javascript_response(
                    &mut stream,
                    r#"
                        debugger;
                        var checksum = 0;
                        for (var index = 0; index < 3; index++) {
                            checksum += index;
                        }
                        var iterationCallbacks = [];
                        for (let offset = 0; offset < 2; offset++) {
                            iterationCallbacks.push(() => offset);
                        }
                        window.MapFixture.iterations =
                            iterationCallbacks[0]() + ":" +
                            iterationCallbacks[1]();
                        window.MapFixture.methods =
                            [1, 2, 3].map(value => value * 2).join(":");
                        const {
                            tiles: [primaryTile, ...remainingTiles],
                            style = "road",
                            ...mapOptions
                        } = {
                            tiles: [8, 9],
                            zoom: 12
                        };
                        window.MapFixture.declarations =
                            primaryTile + ":" +
                            remainingTiles[0] + ":" +
                            style + ":" +
                            mapOptions.zoom;
                        var routeCallbacks = [];
                        for (
                            const [routeKind, routeWeight] of
                            [["road", 2], ["poi", 3]]
                        ) {
                            routeCallbacks.push(
                                () => routeKind + ":" + routeWeight
                            );
                        }
                        var glyphs = "";
                        for (const glyph of "A😀") {
                            glyphs += glyph;
                        }
                        window.MapFixture.forOf =
                            routeCallbacks[0]() + "|" +
                            routeCallbacks[1]() + "|" +
                            glyphs;
                        var blockCallbacks = [];
                        for (var blockIndex = 0; blockIndex < 2; blockIndex++) {
                            let snapshot = blockIndex;
                            blockCallbacks.push(() => snapshot);
                        }
                        window.MapFixture.blocks =
                            blockCallbacks[0]() + ":" +
                            blockCallbacks[1]();
                        {
                            window.MapFixture.blockFunction = readBlockMode();
                            function readBlockMode() {
                                return "hoisted";
                            }
                        }
                        var modeCode = buildModeCode({
                            value: checksum,
                            padding: 0
                        });
                        function buildModeCode(
                            { value, ...metadata },
                            [mask = 1, ...flags] = [],
                            ...ignored
                        ) {
                            return (value << 1) |
                                (
                                    mask +
                                    metadata.padding +
                                    flags.length +
                                    ignored.length
                                );
                        }
                        switch (modeCode) {
                            case 7:
                                window.MapFixture.mode = "vector";
                                break;
                            default:
                                window.MapFixture.mode = "unknown";
                        }
                        var templateOrder = "";
                        function templateRead(label, value) {
                            templateOrder += label;
                            return value;
                        }
                        window.MapFixture.template =
                            `mode\n${
                                templateRead("A", window.MapFixture.mode)
                            }:${
                                templateRead("B", null)
                            }:\`\u{1F600}`;
                        window.MapFixture.templateOrder = templateOrder;
                        var logicalTargetReads = 0;
                        function logicalKey() {
                            logicalTargetReads += 1;
                            return "logical";
                        }
                        window.MapFixture.logical = 0;
                        window.MapFixture[logicalKey()] ||= 2;
                        window.MapFixture[logicalKey()] ||= 3;
                        window.MapFixture.logical &&= 4;
                        window.MapFixture.missing ??= 5;
                        var power = 2;
                        power **= 3;
                        window.MapFixture.logicalAssignments =
                            window.MapFixture.logical + ":" +
                            window.MapFixture.missing + ":" +
                            power + ":" +
                            logicalTargetReads;
                        window.MapFixture.reassigned = {};
                        var reassignmentKeyCalls = 0;
                        function reassignmentKey() {
                            reassignmentKeyCalls += 1;
                            return "middle";
                        }
                        var reassignedHead = 0;
                        var reassignedTail = [];
                        var reassignmentSource = [12, 13, 14];
                        var reassignmentResult = (
                            [
                                reassignedHead,
                                window.MapFixture.reassigned[
                                    reassignmentKey()
                                ],
                                ...reassignedTail
                            ] = reassignmentSource
                        );
                        var reassignedZoom = 0;
                        var reassignedOptions = {};
                        var objectReassignmentSource = {
                            zoom: 15,
                            style: "dark"
                        };
                        var objectReassignmentResult = (
                            {
                                zoom: reassignedZoom,
                                ...reassignedOptions
                            } = objectReassignmentSource
                        );
                        window.MapFixture.reassignments =
                            reassignedHead + ":" +
                            window.MapFixture.reassigned.middle + ":" +
                            reassignedTail.join(":") + ":" +
                            reassignmentKeyCalls + ":" +
                            (reassignmentResult === reassignmentSource) + ":" +
                            reassignedZoom + ":" +
                            reassignedOptions.style + ":" +
                            (
                                objectReassignmentResult ===
                                objectReassignmentSource
                            );
                        class MapSuperBase {
                            get revision() {
                                return this._revision;
                            }
                            set revision(next) {
                                this._revision = next * 2;
                            }
                        }
                        class MapSuperChild extends MapSuperBase {
                            update() {
                                var keyReads = 0;
                                var key = () => {
                                    keyReads += 1;
                                    return "revision";
                                };
                                var assigned = super[key()] = 3;
                                var post = super[key()]++;
                                super[key()] += 2;
                                super[key()] ||= 99;
                                return (
                                    assigned + ":" +
                                    post + ":" +
                                    super[key()] + ":" +
                                    this._revision + ":" +
                                    keyReads
                                );
                            }
                        }
                        window.MapFixture.superWrites =
                            new MapSuperChild().update();
                        window.MapFixture.version = "fixture-1";
                        window.MapFixture.create = (container) => {
                            container.setAttribute("data-map-created", "yes");
                            return { ready: true };
                        };
                        (
                            window.MapFixture.kind =
                                typeof window.MapFixture.create,
                            window.MapFixture.checksum = checksum
                        );
                    "#,
                ),
                unexpected => panic!("unexpected map SDK request: {unexpected}"),
            }
        }
        paths
    });

    let loader = ScriptLoader::new(ScriptPolicy::default());
    loader
        .attach_to_document(&format!("http://{address}/app/index.html"))
        .expect("attach map SDK document loader");

    let document = w3cos_runtime::jsdom::document_value();
    let bootstrap = document.call_method("createElement", vec![Value::string("script")]);
    bootstrap.set_property(
        "src",
        Value::from(format!("http://{address}/maps/bootstrap.js")),
    );
    let bootstrap_loads = Arc::new(AtomicUsize::new(0));
    let observed_bootstrap_loads = Arc::clone(&bootstrap_loads);
    bootstrap.set_property(
        "onload",
        Value::function(move |_, _| {
            observed_bootstrap_loads.fetch_add(1, Ordering::SeqCst);
            Value::Undefined
        }),
    );
    document
        .get_property("head")
        .call_method("appendChild", vec![bootstrap]);

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        w3cos_runtime::jsdom::tick_timers();
        w3cos_runtime::jsdom::drain_microtasks();
        if document
            .get_property("body")
            .call_method("getAttribute", vec![Value::string("data-map-sdk")])
            .to_js_string()
            == "fixture-1"
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "map SDK fixture did not initialize before the deadline"
        );
        thread::sleep(Duration::from_millis(1));
    }

    let paths = server.join().expect("map SDK fixture completed");
    assert_eq!(
        paths,
        [
            "/maps/bootstrap.js",
            "/maps/jsonp?callback=MapFixture.callback",
            "/maps/chunk.js"
        ]
    );
    assert!(!has_pending_script_fetches());
    assert_eq!(bootstrap_loads.load(Ordering::SeqCst), 1);
    let map_fixture = w3cos_runtime::jsdom::window_value().get_property("MapFixture");
    assert_eq!(map_fixture.get_property("kind").to_js_string(), "function");
    assert_eq!(map_fixture.get_property("checksum"), Value::Number(3.0));
    assert_eq!(map_fixture.get_property("mode").to_js_string(), "vector");
    assert_eq!(
        map_fixture.get_property("template").to_js_string(),
        "mode\nvector:null:`😀"
    );
    assert_eq!(
        map_fixture.get_property("templateOrder").to_js_string(),
        "AB"
    );
    assert_eq!(map_fixture.get_property("iterations").to_js_string(), "0:1");
    assert_eq!(map_fixture.get_property("methods").to_js_string(), "2:4:6");
    assert_eq!(
        map_fixture.get_property("declarations").to_js_string(),
        "8:9:road:12"
    );
    assert_eq!(
        map_fixture.get_property("forOf").to_js_string(),
        "road:2|poi:3|A😀"
    );
    assert_eq!(map_fixture.get_property("blocks").to_js_string(), "0:1");
    assert_eq!(
        map_fixture.get_property("blockFunction").to_js_string(),
        "hoisted"
    );
    assert_eq!(
        map_fixture
            .get_property("logicalAssignments")
            .to_js_string(),
        "4:5:8:2"
    );
    assert_eq!(
        map_fixture.get_property("reassignments").to_js_string(),
        "12:13:14:1:true:15:dark:true"
    );
    assert_eq!(
        map_fixture.get_property("superWrites").to_js_string(),
        "3:6:32:32:5"
    );
    assert_eq!(
        document
            .get_property("body")
            .call_method("getAttribute", vec![Value::string("data-map-metadata")])
            .to_js_string(),
        "metadata-ready"
    );
    assert_eq!(
        document
            .get_property("body")
            .call_method("getAttribute", vec![Value::string("data-map-error")]),
        Value::Null
    );

    let map_container = document.call_method("createElement", vec![Value::string("div")]);
    let instance = map_fixture.call_method("create", vec![map_container.clone()]);
    assert!(instance.get_property("ready").to_bool());
    assert_eq!(
        map_container
            .call_method("getAttribute", vec![Value::string("data-map-created")])
            .to_js_string(),
        "yes"
    );
}
