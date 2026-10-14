// Shared page script. These exact bytes run in headless Chromium and through
// the w3cos ESM/AOT native path. The harness selects a workload by setting
// globalThis.W3COS_PERF_WORKLOAD before this module runs.
export {};
var DOM_NODES = 1000;
var DOM_UPDATES = 40;
var FLEX_ITEMS = 240;
var GRID_ITEMS = 240;
var REFLOWS = 12;
var SCROLL_ROWS = 100;
var BOXES = 80;
var FRAME_WARMUP = 2;
var FRAME_SAMPLES = 20;
var COMPUTE_N = 400000;
var MEMORY_NODES = 4000;
var WARMUP = 2;
var SAMPLES = 5;

function check(ok, message) {
    if (!ok) throw new Error(message || "correctness check failed");
}
function resetPage() {
    var bootstrap = document.getElementById("root");
    if (bootstrap && bootstrap.parentNode) bootstrap.parentNode.removeChild(bootstrap);
    document.body.style.margin = "0";
    document.body.style.background = "#ffffff";
    if (document.documentElement && document.documentElement.style) {
        document.documentElement.style.margin = "0";
        document.documentElement.style.background = "#ffffff";
    }
}
function emit(payload) {
    payload.schema = "w3cos-native-browser-v1";
    if (!payload.status) payload.status = "ok";
    payload.viewport = {
        width: window.innerWidth,
        height: window.innerHeight,
        dpr: window.devicePixelRatio
    };
    console.log("W3COS_PERF_RESULT " + JSON.stringify(payload));
}
function measure(run) {
    var samples = [];
    for (var i = 0; i < WARMUP + SAMPLES; i++) {
        var evidence = run();
        if (i >= WARMUP) samples.push(evidence);
    }
    return samples;
}
function domOnce() {
    resetPage();
    var root = document.createElement("div");
    document.body.appendChild(root);
    var nodes = [];
    var t0 = performance.now();
    for (var i = 0; i < DOM_NODES; i++) {
        var el = document.createElement("div");
        el.id = "row-" + i;
        el.className = "row";
        el.textContent = "item " + i;
        root.appendChild(el);
        nodes.push(el);
    }
    var createMs = performance.now() - t0;
    var t1 = performance.now();
    var textSum = 0;
    for (var k = 0; k < DOM_UPDATES; k++) {
        var index = (k * 17) % DOM_NODES;
        nodes[index].textContent = "upd " + k;
        root.appendChild(nodes[index]);
        textSum = textSum + nodes[index].textContent.length;
    }
    var updateMs = performance.now() - t1;
    var count = root.childNodes.length;
    check(count === DOM_NODES, "dom count");
    var first = root.childNodes[0].textContent;
    var last = root.childNodes[count - 1].textContent;
    document.body.removeChild(root);
    return {
        elapsed_ms: createMs + updateMs,
        create_ms: createMs,
        update_ms: updateMs,
        checksum: textSum,
        count: count,
        first: first,
        last: last
    };
}
function layoutOnce() {
    resetPage();
    var flex = document.createElement("div");
    flex.style.display = "flex";
    flex.style.flexWrap = "wrap";
    flex.style.width = "800px";
    document.body.appendChild(flex);
    for (var i = 0; i < FLEX_ITEMS; i++) {
        var item = document.createElement("div");
        item.style.width = "80px";
        item.style.height = "24px";
        item.textContent = "" + (i % 10);
        flex.appendChild(item);
    }
    var grid = document.createElement("div");
    grid.style.display = "grid";
    grid.style.gridTemplateColumns = "repeat(8, 1fr)";
    grid.style.width = "800px";
    document.body.appendChild(grid);
    for (var j = 0; j < GRID_ITEMS; j++) {
        var cell = document.createElement("div");
        cell.style.height = "20px";
        cell.textContent = "" + (j % 10);
        grid.appendChild(cell);
    }
    var t0 = performance.now();
    var checksum = 0;
    for (var r = 0; r < REFLOWS; r++) {
        var wide = r % 2 === 0;
        flex.style.width = wide ? "800px" : "520px";
        grid.style.width = wide ? "800px" : "520px";
        grid.style.gridTemplateColumns = wide ? "repeat(8, 1fr)" : "repeat(4, 1fr)";
        checksum = checksum + flex.offsetHeight + grid.offsetHeight;
    }
    var elapsed = performance.now() - t0;
    check(checksum > 0, "layout produced no height");
    document.body.removeChild(flex);
    document.body.removeChild(grid);
    return {elapsed_ms: elapsed, checksum: checksum};
}
function layoutCssOnce() {
    resetPage();
    var style = document.createElement("style");
    style.textContent = "@font-face{font-family:w3perf;src:local(\"Noto Sans\"),url(\"/NotoSans-Regular.ttf\")}.wrap{width:100%}.card{display:block;width:50%}.copy{display:block;margin:0;font-family:w3perf,sans-serif;font-size:16px;line-height:20px}";
    document.head.appendChild(style);
    var wrap = document.createElement("div");
    wrap.className = "wrap";
    document.body.appendChild(wrap);
    var cards = 40;
    for (var i = 0; i < cards; i++) {
        var card = document.createElement("div");
        card.className = "card";
        var copy = document.createElement("div");
        copy.className = "copy";
        var words = (i % 5) + 2;
        var text = "";
        for (var w = 0; w < words; w++) text = text + "measure ";
        copy.textContent = text + i;
        card.appendChild(copy);
        wrap.appendChild(card);
    }
    var t0 = performance.now();
    var checksum = 0;
    for (var r = 0; r < 6; r++) {
        wrap.style.width = r % 2 === 0 ? "800px" : "420px";
        checksum = checksum + wrap.offsetHeight;
    }
    var elapsed = performance.now() - t0;
    check(checksum > 0, "css layout produced no height");
    document.head.removeChild(style);
    document.body.removeChild(wrap);
    return {elapsed_ms: elapsed, checksum: checksum};
}
function computeOnce() {
    var n = COMPUTE_N;
    var t0 = performance.now();
    var a = 1.1;
    var b = 2.2;
    var s = 0;
    for (var i = 1; i <= n; i++) {
        a = a + b * 0.000001;
        b = b + a * 0.000001;
        s = s + (a - b);
    }
    var elapsed = performance.now() - t0;
    check(isFinite(s), "compute was not finite");
    return {elapsed_ms: elapsed, checksum: s, n: n};
}
function runStartup() {
    resetPage();
    var root = document.createElement("div");
    root.id = "paint";
    root.textContent = "w3cos-perf-ready";
    root.style.background = "#00aa00";
    root.style.width = "240px";
    root.style.height = "80px";
    document.body.appendChild(root);
    for (var i = 0; i < 20; i++) {
        var box = document.createElement("div");
        box.style.width = "20px";
        box.style.height = "20px";
        box.style.background = i % 2 === 0 ? "#3366ff" : "#ff6633";
        box.textContent = "" + i;
        document.body.appendChild(box);
    }
    requestAnimationFrame(function () {
        console.log("W3COS_FIRST_CONTENT nodes=" + document.body.childNodes.length + " marker=" + root.textContent);
    });
}
function runFrames() {
    resetPage();
    var scroller = document.createElement("div");
    scroller.style.width = "400px";
    scroller.style.height = "300px";
    scroller.style.overflow = "auto";
    document.body.appendChild(scroller);
    for (var i = 0; i < SCROLL_ROWS; i++) {
        var row = document.createElement("div");
        row.style.height = "28px";
        row.textContent = "row " + i;
        scroller.appendChild(row);
    }
    var layer = document.createElement("div");
    layer.style.position = "relative";
    layer.style.width = "400px";
    layer.style.height = "200px";
    document.body.appendChild(layer);
    var boxes = [];
    for (var b = 0; b < BOXES; b++) {
        var box = document.createElement("div");
        box.style.position = "absolute";
        box.style.width = "16px";
        box.style.height = "16px";
        box.style.left = ((b % 20) * 20) + "px";
        box.style.top = "0px";
        box.style.background = "#3366ff";
        layer.appendChild(box);
        boxes.push(box);
    }
    var works = [];
    var gaps = [];
    var lastTs = 0;
    var frameIndex = 0;
    var heightSum = 0;
    var scrollSum = 0;
    function frame(ts) {
        var gap = lastTs === 0 ? 0 : ts - lastTs;
        lastTs = ts;
        var t0 = performance.now();
        scroller.scrollTop = (frameIndex * 17) % 2000;
        for (var n = 0; n < BOXES; n++) {
            boxes[n].style.top = ((frameIndex * 3 + n) % 180) + "px";
        }
        var scrollerHeight = scroller.offsetHeight;
        var layerHeight = layer.offsetHeight;
        heightSum = heightSum + scrollerHeight + layerHeight;
        scrollSum = scrollSum + scroller.scrollTop;
        var work = performance.now() - t0;
        if (frameIndex >= FRAME_WARMUP) {
            works.push(work);
            gaps.push(gap);
        }
        frameIndex = frameIndex + 1;
        if (frameIndex < FRAME_WARMUP + FRAME_SAMPLES) requestAnimationFrame(frame);
        else {
            var elapsed = 0;
            for (var s = 0; s < works.length; s++) elapsed = elapsed + works[s];
            emit({
                workload: "frames",
                samples: works,
                gaps: gaps,
                elapsed_ms: elapsed,
                checksum: heightSum,
                scroll_sum: scrollSum,
                frames: works.length
            });
        }
    }
    requestAnimationFrame(frame);
}
function runMemory() {
    resetPage();
    var root = document.createElement("div");
    document.body.appendChild(root);
    var t0 = performance.now();
    for (var i = 0; i < MEMORY_NODES; i++) {
        var el = document.createElement("div");
        el.textContent = "m " + (i % 50);
        el.style.height = "18px";
        root.appendChild(el);
    }
    var height = root.offsetHeight;
    var elapsed = performance.now() - t0;
    check(root.childNodes.length === MEMORY_NODES, "memory count");
    emit({
        workload: "memory",
        elapsed_ms: elapsed,
        checksum: height,
        count: root.childNodes.length
    });
}
try {
    var workload = globalThis.W3COS_PERF_WORKLOAD;
    if (workload === "startup") runStartup();
    else if (workload === "dom-list") emit({workload: "dom-list", samples: measure(domOnce)});
    else if (workload === "layout") emit({workload: "layout", samples: measure(layoutOnce)});
    else if (workload === "layout-css") emit({workload: "layout-css", samples: measure(layoutCssOnce)});
    else if (workload === "frames") runFrames();
    else if (workload === "compute") emit({workload: "compute", samples: measure(computeOnce)});
    else if (workload === "memory") runMemory();
    else emit({workload: String(workload), status: "error", error: "unknown workload"});
} catch (error) {
    console.log("W3COS_PERF_RESULT " + JSON.stringify({
        schema: "w3cos-native-browser-v1",
        status: "error",
        error: String(error)
    }));
}
