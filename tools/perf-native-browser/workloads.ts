// Shared page script. These exact bytes run in headless Chromium and through
// the w3cos ESM/AOT native path. The harness selects a workload by setting
// globalThis.W3COS_PERF_WORKLOAD before this module runs.
export {};
var DOM_NODES = 1000;
var DOM_UPDATES = 8000;
var FLEX_ITEMS = 240;
var GRID_ITEMS = 240;
var REFLOWS = 12;
var SCROLL_ROWS = 100;
var BOXES = 80;
var FRAME_WARMUP = 2;
var FRAME_SAMPLES = 20;
var COMPUTE_N = 400000;
var MEMORY_NODES = 4000;
var CSS_LIST_NODES = 1000;
var FLEX_LIST_NODES = 400;
var EVENT_NODES = 400;
var TABLE_ROWS = 24;
var TABLE_COLS = 6;
var TABLE_PASSES = 4;
var STYLED_NODES = 800;
var SCROLL_STEPS = 30;
var TEXT_PARAS = 80;
var TEXT_PASSES = 6;
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
    style.textContent = "@font-face{font-family:w3perf;src:local(\"Noto Sans Regular\"),local(\"Noto Sans\"),url(\"/NotoSans-Regular.ttf\")}.wrap{width:100%}.card{display:block;width:50%}.copy{display:block;margin:0;font-family:w3perf,sans-serif;font-size:16px;line-height:20px}";
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
function runCssList() {
    resetPage();
    var style = document.createElement("style");
    style.textContent = "#css-list > div { height: 18px; font-size: 14px; line-height: 18px; }";
    document.head.appendChild(style);
    var root = document.createElement("div");
    root.id = "css-list";
    document.body.appendChild(root);
    var t0 = performance.now();
    for (var i = 0; i < CSS_LIST_NODES; i++) {
        var el = document.createElement("div");
        el.textContent = "row " + (i % 40);
        root.appendChild(el);
    }
    var height = root.offsetHeight;
    var elapsed = performance.now() - t0;
    check(root.childNodes.length === CSS_LIST_NODES, "css list count");
    emit({
        workload: "css-list",
        elapsed_ms: elapsed,
        checksum: height,
        count: root.childNodes.length
    });
}
function runFlexList() {
    resetPage();
    var root = document.createElement("div");
    root.style.display = "flex";
    root.style.flexDirection = "column";
    document.body.appendChild(root);
    var t0 = performance.now();
    for (var i = 0; i < FLEX_LIST_NODES; i++) {
        var el = document.createElement("div");
        el.textContent = "f " + (i % 20);
        el.style.height = "18px";
        el.style.flex = "0 0 auto";
        root.appendChild(el);
    }
    var height = root.offsetHeight;
    var elapsed = performance.now() - t0;
    check(root.childNodes.length === FLEX_LIST_NODES, "flex list count");
    emit({
        workload: "flex-list",
        elapsed_ms: elapsed,
        checksum: height,
        count: root.childNodes.length
    });
}
function runEvents() {
    resetPage();
    var root = document.createElement("div");
    document.body.appendChild(root);
    var hits = 0;
    for (var i = 0; i < EVENT_NODES; i++) {
        var el = document.createElement("button");
        el.textContent = "e " + i;
        el.addEventListener("click", function () { hits = hits + 1; });
        root.appendChild(el);
    }
    var nodes = root.childNodes;
    var t0 = performance.now();
    for (var n = 0; n < nodes.length; n++) {
        nodes[n].dispatchEvent(new Event("click"));
    }
    var elapsed = performance.now() - t0;
    check(hits === EVENT_NODES, "event hits");
    emit({
        workload: "events",
        elapsed_ms: elapsed,
        checksum: hits,
        count: nodes.length
    });
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
function runTableRepaint() {
    resetPage();
    var table = document.createElement("table");
    table.style.borderCollapse = "collapse";
    table.style.borderSpacing = "0";
    table.style.tableLayout = "fixed";
    table.style.width = "720px";
    document.body.appendChild(table);
    var cells = [];
    for (var r = 0; r < TABLE_ROWS; r++) {
        var row = document.createElement("tr");
        row.style.height = "22px";
        for (var c = 0; c < TABLE_COLS; c++) {
            var cell = document.createElement("td");
            cell.style.width = "120px";
            cell.style.height = "22px";
            cell.style.padding = "0";
            cell.style.border = "0";
            cell.style.overflow = "hidden";
            cell.style.whiteSpace = "nowrap";
            cell.textContent = "c " + r + "-" + c;
            row.appendChild(cell);
            cells.push(cell);
        }
        table.appendChild(row);
    }
    var warmHeight = table.offsetHeight;
    check(warmHeight > 0, "table warmup");
    var t0 = performance.now();
    var checksum = 0;
    var textSum = 0;
    for (var pass = 0; pass < TABLE_PASSES; pass++) {
        for (var i = 0; i < cells.length; i++) {
            var text = "v " + pass + "-" + i;
            cells[i].textContent = text;
            textSum = textSum + text.length;
        }
        checksum = checksum + table.offsetHeight;
    }
    var elapsed = performance.now() - t0;
    check(table.childNodes.length === TABLE_ROWS, "table rows");
    check(cells.length === TABLE_ROWS * TABLE_COLS, "table cells");
    check(checksum > 0, "table height");
    emit({
        workload: "table-repaint",
        elapsed_ms: elapsed,
        checksum: checksum + textSum,
        count: cells.length
    });
}
function runStyledScroll() {
    resetPage();
    var style = document.createElement("style");
    style.textContent = "#styled-scroll{width:420px;height:360px;overflow:auto}#styled-scroll div{height:24px;font-size:14px;line-height:24px;background:#eef3ff;margin:0 0 2px 0}";
    document.head.appendChild(style);
    var scroller = document.createElement("div");
    scroller.id = "styled-scroll";
    document.body.appendChild(scroller);
    for (var i = 0; i < STYLED_NODES; i++) {
        var row = document.createElement("div");
        row.textContent = "styled " + (i % 50);
        scroller.appendChild(row);
    }
    var probe = scroller.childNodes[STYLED_NODES - 1];
    var t0 = performance.now();
    var checksum = 0;
    var scrollSum = 0;
    for (var step = 0; step < SCROLL_STEPS; step++) {
        scroller.scrollTop = (step * 37) % 4000;
        checksum = checksum + scroller.offsetHeight + probe.offsetTop;
        scrollSum = scrollSum + scroller.scrollTop;
    }
    var elapsed = performance.now() - t0;
    check(scroller.childNodes.length === STYLED_NODES, "styled count");
    check(checksum > 0, "styled scroll height");
    emit({
        workload: "styled-scroll",
        elapsed_ms: elapsed,
        checksum: checksum + scrollSum,
        count: scroller.childNodes.length
    });
}
function runTextLayout() {
    resetPage();
    var style = document.createElement("style");
    style.textContent = "@font-face{font-family:w3perf;src:local(\"Noto Sans Regular\"),local(\"Noto Sans\"),url(\"/NotoSans-Regular.ttf\")}";
    document.head.appendChild(style);
    var wrap = document.createElement("div");
    document.body.appendChild(wrap);
    for (var i = 0; i < TEXT_PARAS; i++) {
        var para = document.createElement("p");
        para.style.margin = "0";
        para.style.fontFamily = "w3perf, sans-serif";
        para.style.fontSize = "16px";
        para.style.lineHeight = "22px";
        para.style.width = "100%";
        var sentence = "The quick brown fox jumps over the lazy dog near the river bank. ";
        var copies = (i % 4) + 1;
        var text = "";
        for (var c = 0; c < copies; c++) text = text + sentence;
        para.textContent = text + i;
        wrap.appendChild(para);
    }
    wrap.style.width = "640px";
    var warmWide = wrap.offsetHeight;
    wrap.style.width = "360px";
    var warmNarrow = wrap.offsetHeight;
    check(warmWide > 0 && warmNarrow > 0 && warmWide !== warmNarrow, "text warmup");
    var t0 = performance.now();
    var checksum = 0;
    for (var pass = 0; pass < TEXT_PASSES; pass++) {
        wrap.style.width = pass % 2 === 0 ? "640px" : "360px";
        checksum = checksum + wrap.offsetHeight;
    }
    var elapsed = performance.now() - t0;
    check(wrap.childNodes.length === TEXT_PARAS, "text paragraphs");
    check(checksum > 0, "text layout height");
    emit({
        workload: "text-layout",
        elapsed_ms: elapsed,
        checksum: checksum,
        count: wrap.childNodes.length
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
    else if (workload === "css-list") runCssList();
    else if (workload === "flex-list") runFlexList();
    else if (workload === "events") runEvents();
    else if (workload === "table-repaint") runTableRepaint();
    else if (workload === "styled-scroll") runStyledScroll();
    else if (workload === "text-layout") runTextLayout();
    else emit({workload: String(workload), status: "error", error: "unknown workload"});
} catch (error) {
    console.log("W3COS_PERF_RESULT " + JSON.stringify({
        schema: "w3cos-native-browser-v1",
        status: "error",
        error: String(error)
    }));
}
