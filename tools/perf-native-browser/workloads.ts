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
// ---- Shared helpers for the 2026-10 workloads -------------------------------
var MD_HISTORY = 200;
var MD_LONG_HISTORY = 2000;
var MD_TOKENS = 5000;
var MD_LONG_SCROLLS = 2000;
var OPS_LIST = 1000;
var OPS_CHURN = 20000;
var OPS_SHUFFLES = 20;
var OPS_PARSES = 20;
var OPS_CLONES = 10;
var OPS_QUERIES = 5000;
var OPS_ATTRS = 50000;
var OPS_FRAGMENTS = 20;
var CSS_RULES = 300;
var CSS_NODES = 2000;
var CSS_TOGGLES = 20;
var CSS_READS = 5000;
var ANIM_NODES = 500;
var ANIM_FRAMES = 60;
var EVT_LISTENERS = 10000;
var EVT_DEPTH = 50;
var EVT_DISPATCHES = 100000;
var EVT_CHURN = 20000;
var EVT_FLOOD = 20000;

function now() { return performance.now(); }
function fold(sum, value) { return (sum * 31 + value) % 1000000007; }
function strFold(sum, text) {
    for (var i = 0; i < text.length; i++) sum = (sum * 31 + text.charCodeAt(i)) % 1000000007;
    return sum;
}
function sortedStats(values) {
    var copy = values.slice();
    copy.sort(function (a, b) { return a - b; });
    function at(fraction) {
        if (copy.length === 0) return 0;
        var index = Math.min(copy.length - 1, Math.floor((copy.length - 1) * fraction + 0.5));
        return copy[index];
    }
    return {p50: at(0.5), p95: at(0.95), max: copy.length ? copy[copy.length - 1] : 0};
}
// Deterministic LCG so both engines see the same content.
function makeRng(seed) {
    var state = seed;
    return function () {
        state = (state * 1103515245 + 12345) % 2147483648;
        return state / 2147483648;
    };
}

// ---- Minimal Markdown -> DOM renderer (vendored, plain JS, no regex) --------
var MD_KEYWORDS = ["function", "return", "var", "let", "const", "if", "else", "for", "while", "new", "class", "import", "export"];
function mdIsKeyword(word) {
    for (var i = 0; i < MD_KEYWORDS.length; i++) if (MD_KEYWORDS[i] === word) return true;
    return false;
}
function mdIsIdent(code) {
    return (code >= 97 && code <= 122) || (code >= 65 && code <= 90) || code === 95 || code === 36 || (code >= 48 && code <= 57);
}
function mdSpan(parent, cls, text) {
    var span = document.createElement("span");
    span.className = cls;
    span.textContent = text;
    parent.appendChild(span);
}
// Syntax highlight a single code line into spans.
function mdHighlight(parent, line) {
    var i = 0;
    var plain = "";
    function flush() {
        if (plain.length) { parent.appendChild(document.createTextNode(plain)); plain = ""; }
    }
    while (i < line.length) {
        var ch = line.charAt(i);
        var code = line.charCodeAt(i);
        if (ch === "/" && line.charAt(i + 1) === "/") {
            flush(); mdSpan(parent, "tok-comment", line.slice(i)); return;
        }
        if (ch === "\"" || ch === "'") {
            var end = line.indexOf(ch, i + 1);
            if (end < 0) end = line.length - 1;
            flush(); mdSpan(parent, "tok-string", line.slice(i, end + 1)); i = end + 1; continue;
        }
        if (code >= 48 && code <= 57) {
            var j = i;
            while (j < line.length && line.charCodeAt(j) >= 48 && line.charCodeAt(j) <= 57) j++;
            flush(); mdSpan(parent, "tok-number", line.slice(i, j)); i = j; continue;
        }
        if (mdIsIdent(code)) {
            var k = i;
            while (k < line.length && mdIsIdent(line.charCodeAt(k))) k++;
            var word = line.slice(i, k);
            if (mdIsKeyword(word)) { flush(); mdSpan(parent, "tok-keyword", word); }
            else plain = plain + word;
            i = k; continue;
        }
        plain = plain + ch;
        i++;
    }
    flush();
}
// Inline: `code`, **bold**, [text](href).
function mdInline(parent, text) {
    var i = 0;
    var plain = "";
    function flush() {
        if (plain.length) { parent.appendChild(document.createTextNode(plain)); plain = ""; }
    }
    while (i < text.length) {
        var ch = text.charAt(i);
        if (ch === "`") {
            var end = text.indexOf("`", i + 1);
            if (end > i) {
                flush();
                var code = document.createElement("code");
                code.textContent = text.slice(i + 1, end);
                parent.appendChild(code);
                i = end + 1; continue;
            }
        }
        if (ch === "*" && text.charAt(i + 1) === "*") {
            var close = text.indexOf("**", i + 2);
            if (close > i) {
                flush();
                var strong = document.createElement("strong");
                strong.textContent = text.slice(i + 2, close);
                parent.appendChild(strong);
                i = close + 2; continue;
            }
        }
        if (ch === "[") {
            var mid = text.indexOf("](", i);
            var stop = mid > 0 ? text.indexOf(")", mid) : -1;
            if (mid > i && stop > mid) {
                flush();
                var a = document.createElement("a");
                a.setAttribute("href", text.slice(mid + 2, stop));
                a.textContent = text.slice(i + 1, mid);
                parent.appendChild(a);
                i = stop + 1; continue;
            }
        }
        plain = plain + ch;
        i++;
    }
    flush();
}
function mdTrimLeft(text) {
    var i = 0;
    while (i < text.length && text.charAt(i) === " ") i++;
    return text.slice(i);
}
function mdStartsWith(text, prefix) { return text.slice(0, prefix.length) === prefix; }
function mdTableRow(tr, line, tag) {
    var cells = line.split("|");
    for (var c = 1; c < cells.length - 1; c++) {
        var cell = document.createElement(tag);
        mdInline(cell, mdTrimLeft(cells[c]));
        tr.appendChild(cell);
    }
}
// Render markdown source into `container`. Returns the number of blocks.
function mdRender(container, source) {
    var lines = source.split("\n");
    var blocks = 0;
    var i = 0;
    while (i < lines.length) {
        var line = lines[i];
        if (line.length === 0) { i++; continue; }
        blocks++;
        if (mdStartsWith(line, "```")) {
            var pre = document.createElement("pre");
            var code = document.createElement("code");
            code.className = "lang-" + line.slice(3);
            pre.appendChild(code);
            i++;
            while (i < lines.length && !mdStartsWith(lines[i], "```")) {
                var row = document.createElement("div");
                row.className = "code-line";
                mdHighlight(row, lines[i]);
                code.appendChild(row);
                i++;
            }
            i++;
            container.appendChild(pre);
            continue;
        }
        if (line.charAt(0) === "#") {
            var level = 0;
            while (line.charAt(level) === "#") level++;
            var h = document.createElement("h" + Math.min(level, 6));
            mdInline(h, line.slice(level + 1));
            container.appendChild(h);
            i++; continue;
        }
        if (line.charAt(0) === "|") {
            var table = document.createElement("table");
            var head = document.createElement("tr");
            mdTableRow(head, line, "th");
            table.appendChild(head);
            i += 2;
            while (i < lines.length && lines[i].charAt(0) === "|") {
                var tr = document.createElement("tr");
                mdTableRow(tr, lines[i], "td");
                table.appendChild(tr);
                i++;
            }
            container.appendChild(table);
            continue;
        }
        if (mdStartsWith(line, "- ") || mdStartsWith(line, "1. ")) {
            var ordered = line.charAt(0) === "1";
            var list = document.createElement(ordered ? "ol" : "ul");
            while (i < lines.length && (mdStartsWith(lines[i], "- ") || mdStartsWith(lines[i], "1. "))) {
                var li = document.createElement("li");
                mdInline(li, lines[i].slice(lines[i].indexOf(" ") + 1));
                list.appendChild(li);
                i++;
            }
            container.appendChild(list);
            continue;
        }
        if (mdStartsWith(line, "> ")) {
            var quote = document.createElement("blockquote");
            var qp = document.createElement("p");
            mdInline(qp, line.slice(2));
            quote.appendChild(qp);
            container.appendChild(quote);
            i++; continue;
        }
        var p = document.createElement("p");
        mdInline(p, line);
        container.appendChild(p);
        i++;
    }
    return blocks;
}
var MD_WORDS = ["the", "layout", "engine", "streams", "tokens", "into", "a", "chat", "view", "while", "keeping", "scroll", "anchored", "native", "code", "path", "runs", "fast", "and", "text"];
function mdSentence(rng, words) {
    var out = "";
    for (var w = 0; w < words; w++) {
        if (w) out = out + " ";
        out = out + MD_WORDS[Math.floor(rng() * MD_WORDS.length)];
    }
    return out;
}
function mdCodeBlock(rng, lines) {
    var out = "```js\n";
    for (var l = 0; l < lines; l++) {
        var n = Math.floor(rng() * 1000);
        var kind = l % 5;
        if (kind === 0) out = out + "function step" + n + "(input) {\n";
        else if (kind === 1) out = out + "    var total = input * " + n + "; // scale\n";
        else if (kind === 2) out = out + "    if (total > " + n + ") return \"big\";\n";
        else if (kind === 3) out = out + "    for (let i = 0; i < " + n + "; i++) total = total + i;\n";
        else out = out + "    return total;\n}\n";
    }
    return out + "```\n";
}
function mdMessage(rng, index) {
    var kind = index % 4;
    var out = "## Message " + index + "\n";
    out = out + mdSentence(rng, 14) + " with `inline code` and **bold " + index + "** plus [a link](https://example.com/" + index + ").\n";
    if (kind === 0) {
        out = out + "- " + mdSentence(rng, 5) + "\n- " + mdSentence(rng, 6) + "\n- " + mdSentence(rng, 4) + "\n";
    } else if (kind === 1) {
        out = out + "| name | value | note |\n| --- | --- | --- |\n";
        for (var r = 0; r < 4; r++) out = out + "| row" + r + " | " + Math.floor(rng() * 100) + " | " + mdSentence(rng, 3) + " |\n";
    } else if (kind === 2) {
        out = out + mdCodeBlock(rng, 50);
    } else {
        out = out + "> " + mdSentence(rng, 10) + "\n1. " + mdSentence(rng, 4) + "\n1. " + mdSentence(rng, 5) + "\n";
    }
    return out + mdSentence(rng, 20) + ".\n";
}
var MD_STYLE = "#chat{width:760px;height:700px;overflow:auto;font-size:14px;line-height:20px}" +
    "#chat .msg{padding:8px 12px;margin:0 0 8px 0;border-bottom:1px solid #ddd}" +
    "#chat .msg.user{background:#f2f6ff}" +
    "#chat h2{font-size:18px;line-height:24px;margin:4px 0}" +
    "#chat p{margin:4px 0}" +
    "#chat pre{background:#f6f8fa;margin:4px 0;padding:6px;overflow:auto}" +
    "#chat .code-line{white-space:pre;font-family:monospace;line-height:18px;height:18px}" +
    "#chat .tok-keyword{color:#cf222e}#chat .tok-string{color:#0a3069}#chat .tok-number{color:#0550ae}#chat .tok-comment{color:#6e7781}" +
    "#chat table{border-collapse:collapse}#chat td,#chat th{border:1px solid #ccc;padding:2px 6px}" +
    "#chat blockquote{margin:4px 0;padding-left:8px;border-left:3px solid #ccc}" +
    "#chat code{background:#eee}";
function mdSetup(count, seed) {
    resetPage();
    var style = document.createElement("style");
    style.textContent = MD_STYLE;
    document.head.appendChild(style);
    var chat = document.createElement("div");
    chat.id = "chat";
    document.body.appendChild(chat);
    var rng = makeRng(seed);
    var blocks = 0;
    for (var m = 0; m < count; m++) {
        var msg = document.createElement("div");
        msg.className = m % 2 === 0 ? "msg user" : "msg";
        blocks += mdRender(msg, mdMessage(rng, m));
        chat.appendChild(msg);
    }
    return {chat: chat, blocks: blocks, rng: rng};
}
function mdTree(sum, node) {
    // Structural checksum: tag names, classes and text, in document order.
    if (node.nodeType === 3) return strFold(sum, node.data);
    sum = strFold(sum, node.tagName);
    if (node.className) sum = strFold(sum, node.className);
    for (var c = node.firstChild; c; c = c.nextSibling) sum = mdTree(sum, c);
    return sum;
}
function mdReplyTokens(rng) {
    // ~MD_TOKENS tokens of 2-8 characters forming a markdown reply.
    var text = "";
    var line = 0;
    while (text.length < MD_TOKENS * 5) {
        if (line % 12 === 5) text = text + mdCodeBlock(rng, 10);
        else if (line % 12 === 9) text = text + "- " + mdSentence(rng, 6) + "\n";
        else text = text + mdSentence(rng, 18) + " `x" + line + "` done.\n";
        line++;
    }
    var tokens = [];
    var pos = 0;
    while (pos < text.length && tokens.length < MD_TOKENS) {
        var size = 2 + Math.floor(rng() * 7);
        tokens.push(text.slice(pos, pos + size));
        pos += size;
    }
    return tokens;
}
function runMdStream(strategy) {
    var setup = mdSetup(MD_HISTORY, 7);
    var chat = setup.chat;
    var warm = chat.scrollHeight;
    check(warm > 0, "md history layout");
    var tokens = mdReplyTokens(makeRng(99));
    var reply = document.createElement("div");
    reply.className = "msg";
    chat.appendChild(reply);
    var source = "";
    var blockStart = 0;       // offset in source where the open block starts
    var tail = null;          // text node receiving appended characters (append strategy)
    var tailBlock = null;     // element rendered from source.slice(blockStart)
    var inCode = false;
    var samples = [];
    var scrollSum = 0;
    var heightSum = 0;
    var t0 = now();
    for (var t = 0; t < tokens.length; t++) {
        var c0 = now();
        var token = tokens[t];
        source = source + token;
        if (strategy === "append") {
            // Append characters to the open block; finish a line at "\n".
            var parts = token.split("\n");
            var off = source.length - token.length;
            for (var p = 0; p < parts.length; p++) {
                if (p > 0) {
                    // Line break: close the open line and re-render it with full markdown.
                    var lineEnd = off;
                    off = off + 1;
                    var finished = source.slice(blockStart, lineEnd);
                    if (mdStartsWith(finished, "```")) inCode = !inCode;
                    if (tailBlock) reply.removeChild(tailBlock);
                    var holder = document.createElement("div");
                    if (inCode && !mdStartsWith(finished, "```")) {
                        holder.className = "code-line";
                        mdHighlight(holder, finished);
                    } else {
                        mdRender(holder, finished);
                    }
                    reply.appendChild(holder);
                    blockStart = off;
                    tailBlock = null;
                    tail = null;
                }
                var piece = parts[p];
                off = off + piece.length;
                if (piece.length) {
                    if (!tail) {
                        tailBlock = document.createElement("p");
                        tail = document.createTextNode("");
                        tailBlock.appendChild(tail);
                        reply.appendChild(tailBlock);
                    }
                    tail.appendData(piece);
                }
            }
        } else {
            // Re-render the last block (from the last blank line / newline) on every chunk.
            var nl = source.lastIndexOf("\n");
            var start = nl < 0 ? 0 : nl + 1;
            if (start !== blockStart) {
                // A line finished: freeze it as rendered output.
                var done = source.slice(blockStart, start - 1);
                if (mdStartsWith(done, "```")) inCode = !inCode;
                if (tailBlock) reply.removeChild(tailBlock);
                var frozen = document.createElement("div");
                if (inCode && !mdStartsWith(done, "```")) { frozen.className = "code-line"; mdHighlight(frozen, done); }
                else mdRender(frozen, done);
                reply.appendChild(frozen);
                blockStart = start;
                tailBlock = null;
            }
            if (tailBlock) reply.removeChild(tailBlock);
            tailBlock = document.createElement("div");
            var open = source.slice(blockStart);
            if (inCode) { tailBlock.className = "code-line"; mdHighlight(tailBlock, open); }
            else mdRender(tailBlock, open);
            reply.appendChild(tailBlock);
        }
        chat.scrollTop = chat.scrollHeight;
        scrollSum += chat.scrollTop;
        samples.push(now() - c0);
    }
    var elapsed = now() - t0;
    heightSum = chat.scrollHeight;
    check(scrollSum > 0, "md auto-scroll");
    var stats = sortedStats(samples);
    emit({
        workload: "md-stream-" + strategy,
        elapsed_ms: elapsed,
        chunk_p50_ms: stats.p50,
        chunk_p95_ms: stats.p95,
        chunk_max_ms: stats.max,
        chunks: tokens.length,
        count: chat.childNodes.length,
        checksum: fold(fold(mdTree(0, reply), source.length), setup.blocks),
        layout_height: heightSum
    });
}
function runMdLongHistory() {
    var t0 = now();
    var setup = mdSetup(MD_LONG_HISTORY, 11);
    var chat = setup.chat;
    var height = chat.scrollHeight;
    var buildMs = now() - t0;
    check(height > 0, "md long layout");
    var t1 = now();
    var probe = 0;
    for (var s = 0; s < MD_LONG_SCROLLS; s++) {
        chat.scrollTop = Math.floor(height * ((s * 37) % MD_LONG_SCROLLS) / MD_LONG_SCROLLS);
        var msg = chat.childNodes[(s * 37) % MD_LONG_HISTORY];
        probe += msg.offsetTop > 0 ? 1 : 0;
    }
    var scrollMs = now() - t1;
    check(probe > 0, "md long probe");
    emit({
        workload: "md-long-history",
        elapsed_ms: buildMs + scrollMs,
        build_ms: buildMs,
        scroll_ms: scrollMs,
        count: chat.childNodes.length,
        checksum: fold(mdTree(0, chat.childNodes[MD_LONG_HISTORY - 1]), setup.blocks),
        layout_height: height
    });
}

// ---- DOM operations ---------------------------------------------------------
function runDomOps() {
    resetPage();
    var root = document.createElement("div");
    document.body.appendChild(root);
    var sum = 0;
    var phases = {};
    var rng = makeRng(5);
    // 1. insertBefore / removeChild / replaceChildren churn.
    var t = now();
    var list = document.createElement("ul");
    root.appendChild(list);
    for (var i = 0; i < OPS_LIST; i++) {
        var li = document.createElement("li");
        li.textContent = "n" + i;
        list.appendChild(li);
    }
    for (var c = 0; c < OPS_CHURN; c++) {
        var kids = list.childNodes;
        var at = Math.floor(rng() * kids.length);
        if (c % 3 === 2) {
            list.removeChild(kids[at]);
        } else {
            var node = document.createElement("li");
            node.textContent = "c" + c;
            list.insertBefore(node, kids[at]);
        }
        if (c % 5000 === 4999) {
            var keep = [];
            for (var k = 0; k < list.childNodes.length; k += 2) keep.push(list.childNodes[k]);
            list.replaceChildren.apply(list, keep);
        }
    }
    sum = fold(sum, list.childNodes.length);
    sum = strFold(sum, list.firstChild.textContent + list.lastChild.textContent);
    phases.churn_ms = now() - t;
    // 2. Keyed shuffle of OPS_LIST rows.
    t = now();
    var keyed = document.createElement("div");
    root.appendChild(keyed);
    var rows = [];
    for (var r = 0; r < OPS_LIST; r++) {
        var row = document.createElement("div");
        row.setAttribute("data-key", "" + r);
        row.textContent = "row " + r;
        keyed.appendChild(row);
        rows.push(row);
    }
    for (var s = 0; s < OPS_SHUFFLES; s++) {
        for (var x = rows.length - 1; x > 0; x--) {
            var y = Math.floor(rng() * (x + 1));
            var tmp = rows[x]; rows[x] = rows[y]; rows[y] = tmp;
        }
        // Keyed reconcile: move each row to its new position.
        var cursor = keyed.firstChild;
        for (var q = 0; q < rows.length; q++) {
            if (rows[q] === cursor) cursor = cursor.nextSibling;
            else keyed.insertBefore(rows[q], cursor);
        }
    }
    sum = strFold(sum, keyed.firstChild.getAttribute("data-key") + "/" + keyed.lastChild.getAttribute("data-key"));
    phases.shuffle_ms = now() - t;
    // 3. innerHTML parse.
    t = now();
    var html = "";
    for (var h = 0; h < 200; h++) html += "<div class=\"item k" + (h % 7) + "\" data-i=\"" + h + "\"><span>t" + h + "</span><b>x</b></div>";
    var host = document.createElement("div");
    root.appendChild(host);
    for (var pz = 0; pz < OPS_PARSES; pz++) {
        host.innerHTML = html;
        sum = fold(sum, host.querySelectorAll("span").length);
    }
    phases.parse_ms = now() - t;
    // 4. Deep cloneNode.
    t = now();
    function build(depth) {
        var el = document.createElement("section");
        el.className = "d" + depth;
        el.setAttribute("title", "t" + depth);
        if (depth === 0) { el.textContent = "leaf"; return el; }
        for (var b = 0; b < 4; b++) el.appendChild(build(depth - 1));
        return el;
    }
    var tree = build(5);
    for (var cl = 0; cl < OPS_CLONES; cl++) {
        var copy = tree.cloneNode(true);
        sum = fold(sum, copy.getElementsByTagName("section").length);
    }
    phases.clone_ms = now() - t;
    // 5. querySelector / querySelectorAll.
    t = now();
    for (var qq = 0; qq < OPS_QUERIES; qq++) {
        var m = qq % 3;
        if (m === 0) sum = fold(sum, host.querySelector("[data-i=\"" + (qq % 200) + "\"]") ? 1 : 0);
        else if (m === 1) sum = fold(sum, host.querySelectorAll(".k" + (qq % 7)).length);
        else sum = fold(sum, host.querySelector("div.item > b") ? 2 : 0);
    }
    phases.query_ms = now() - t;
    // 6. classList toggles on 2000 nodes.
    t = now();
    var togg = [];
    for (var tg = 0; tg < 2000; tg++) {
        var tn = document.createElement("i");
        root.appendChild(tn);
        togg.push(tn);
    }
    for (var round = 0; round < 10; round++) {
        for (var tz = 0; tz < togg.length; tz++) {
            togg[tz].classList.toggle("on");
            if ((tz + round) % 3 === 0) togg[tz].classList.add("third");
            else togg[tz].classList.remove("third");
        }
    }
    var onCount = 0;
    for (var tc = 0; tc < togg.length; tc++) onCount += togg[tc].classList.contains("third") ? 1 : 0;
    sum = fold(sum, onCount);
    phases.classlist_ms = now() - t;
    // 7. Attribute set/get.
    t = now();
    for (var at2 = 0; at2 < OPS_ATTRS; at2++) {
        var target = togg[at2 % togg.length];
        target.setAttribute("data-v", "" + at2);
        sum = fold(sum, target.getAttribute("data-v").length);
    }
    phases.attr_ms = now() - t;
    // 8. DocumentFragment batch insert.
    t = now();
    var batchHost = document.createElement("div");
    root.appendChild(batchHost);
    for (var fr = 0; fr < OPS_FRAGMENTS; fr++) {
        var frag = document.createDocumentFragment();
        for (var fi = 0; fi < 1000; fi++) {
            var fe = document.createElement("p");
            fe.textContent = "f" + fi;
            frag.appendChild(fe);
        }
        batchHost.replaceChildren();
        batchHost.appendChild(frag);
        sum = fold(sum, batchHost.childNodes.length + frag.childNodes.length);
    }
    phases.fragment_ms = now() - t;
    var height = root.offsetHeight;
    check(height > 0, "dom ops layout");
    var total = 0;
    for (var key in phases) total += phases[key];
    phases.workload = "dom-ops";
    phases.elapsed_ms = total;
    phases.checksum = sum;
    phases.count = root.childNodes.length;
    emit(phases);
}

// ---- CSS --------------------------------------------------------------------
function runCssOps() {
    resetPage();
    var css = ":root{--w:100px;--c:rgb(10, 20, 30)}" +
        "#css-ops .cell{height:4px;width:var(--w);color:var(--c)}" +
        "#css-ops.alt .cell{width:120px}" +
        "#css-ops .cell.hot{color:rgb(200, 0, 0)}" +
        "#css-ops div .cell.hot{color:rgb(0, 200, 0)}" +
        "#css-ops [data-kind=\"b\"]{height:6px}" +
        "#css-ops .cell:nth-child(3n){width:60px}" +
        "#css-ops.alt .cell:nth-child(3n){width:70px}" +
        "@media (min-width: 800px){#css-ops .wide{height:5px}}" +
        "@media (max-width: 600px){#css-ops .wide{height:50px}}";
    for (var r = 0; r < CSS_RULES; r++) {
        // Rules that must be matched and rejected for every node.
        var m = r % 4;
        if (m === 0) css += "#css-ops .g" + r + " .cell{width:" + (r % 50) + "px}";
        else if (m === 1) css += "#css-ops .cell.k" + r + "{height:" + (r % 9) + "px}";
        else if (m === 2) css += "#css-ops [data-x=\"" + r + "\"]{color:rgb(" + (r % 255) + ", 1, 1)}";
        else css += "#css-ops section:nth-child(" + (r + 7) + ") > .cell.z" + r + "{margin-left:1px}";
    }
    var style = document.createElement("style");
    style.textContent = css;
    document.head.appendChild(style);
    var root = document.createElement("div");
    root.id = "css-ops";
    document.body.appendChild(root);
    var cells = [];
    var t = now();
    for (var g = 0; g < CSS_NODES / 20; g++) {
        var group = document.createElement("section");
        group.className = "grp";
        root.appendChild(group);
        var inner = document.createElement("div");
        group.appendChild(inner);
        for (var i = 0; i < 20; i++) {
            var cell = document.createElement("div");
            cell.className = (i % 4 === 0 ? "cell hot" : "cell") + (i % 5 === 0 ? " wide" : "");
            if (i % 6 === 0) cell.setAttribute("data-kind", "b");
            inner.appendChild(cell);
            cells.push(cell);
        }
    }
    var sum = root.offsetHeight;
    var phases = {match_ms: now() - t};
    t = now();
    for (var k = 0; k < CSS_TOGGLES; k++) {
        if (k % 2 === 0) root.classList.add("alt");
        else root.classList.remove("alt");
        sum = fold(sum, cells[3].offsetWidth + cells[2].offsetWidth + root.offsetHeight);
    }
    phases.toggle_ms = now() - t;
    t = now();
    for (var q = 0; q < CSS_READS; q++) {
        var cs = getComputedStyle(cells[(q * 7) % cells.length]);
        sum = strFold(sum, cs.color);
        sum = strFold(sum, cs.width);
    }
    phases.computed_ms = now() - t;
    t = now();
    for (var v = 0; v < CSS_TOGGLES; v++) {
        document.documentElement.style.setProperty("--w", (80 + v) + "px");
        sum = fold(sum, cells[1].offsetWidth);
    }
    phases.vars_ms = now() - t;
    t = now();
    var mq = window.matchMedia("(min-width: 800px)").matches ? 1 : 0;
    for (var w = 0; w < cells.length; w += 5) sum = fold(sum, cells[w].offsetHeight);
    sum = fold(sum, mq);
    phases.media_ms = now() - t;
    var total = 0;
    for (var key in phases) total += phases[key];
    phases.workload = "css-ops";
    phases.elapsed_ms = total;
    phases.checksum = sum;
    phases.count = cells.length;
    emit(phases);
}

// ---- Animation --------------------------------------------------------------
function runAnimation() {
    resetPage();
    var style = document.createElement("style");
    style.textContent = "@keyframes w3slide{from{transform:translateX(0px);opacity:1}to{transform:translateX(100px);opacity:0}}" +
        "@keyframes w3grow{from{width:10px;top:0px}to{width:110px;top:100px}}" +
        ".anim{position:absolute;height:4px;width:10px;left:0px;top:0px}" +
        ".kf{animation:w3slide 1000ms linear 0s 1 normal both paused}" +
        ".kl{animation:w3grow 1000ms linear 0s 1 normal both paused}" +
        ".tr{transition:opacity 1000000ms linear, transform 1000000ms linear}";
    document.head.appendChild(style);
    var stage = document.createElement("div");
    stage.style.position = "relative";
    stage.style.height = "400px";
    document.body.appendChild(stage);
    var nodes = [];
    for (var i = 0; i < ANIM_NODES; i++) {
        var el = document.createElement("div");
        var kind = i % 5;
        el.className = "anim " + (kind === 0 ? "kf" : kind === 1 ? "kl" : kind === 2 ? "tr" : "js");
        el.style.top = (i % 100) * 4 + "px";
        stage.appendChild(el);
        nodes.push(el);
    }
    var waapi = [];
    for (var a = 4; a < ANIM_NODES; a += 5) {
        var anim = nodes[a].animate([{opacity: 1, transform: "translateY(0px)"}, {opacity: 0, transform: "translateY(50px)"}], {duration: 1000, fill: "both"});
        anim.pause();
        waapi.push(anim);
    }
    var works = [];
    var sum = 0;
    var frame = 0;
    function step() {
        var t0 = now();
        var progress = (frame % 10) / 10;
        for (var n = 0; n < nodes.length; n++) {
            var node = nodes[n];
            var kind = n % 5;
            if (kind === 0 || kind === 1) node.style.animationDelay = (-progress * 1000) + "ms";
            else if (kind === 2) { node.style.opacity = frame % 2 ? "0" : "1"; }
            else if (kind === 3) { node.style.transform = "translateX(" + (frame * 3 % 100) + "px)"; node.style.left = (frame % 50) + "px"; }
        }
        for (var wv = 0; wv < waapi.length; wv++) waapi[wv].currentTime = progress * 1000;
        for (var r = 0; r < nodes.length; r += 23) {
            var cs = getComputedStyle(nodes[r]);
            var kind2 = r % 5;
            if (kind2 === 1) sum = fold(sum, nodes[r].offsetWidth);
            else if (kind2 === 0 || kind2 === 4) sum = fold(sum, Math.round(parseFloat(cs.opacity) * 100));
            else if (kind2 === 3) sum = fold(sum, nodes[r].offsetLeft);
        }
        works.push(now() - t0);
        frame++;
        if (frame < ANIM_FRAMES + 2) requestAnimationFrame(step);
        else {
            var measured = works.slice(2);
            var total = 0;
            for (var s = 0; s < measured.length; s++) total += measured[s];
            var stats = sortedStats(measured);
            emit({
                workload: "animation",
                elapsed_ms: total,
                frame_p50_ms: stats.p50,
                frame_p95_ms: stats.p95,
                checksum: sum,
                count: nodes.length
            });
        }
    }
    requestAnimationFrame(step);
}

// ---- Events -----------------------------------------------------------------
function runEventFlood() {
    resetPage();
    var root = document.createElement("div");
    document.body.appendChild(root);
    var phases = {};
    var hits = 0;
    var t = now();
    var leaves = [];
    function onHit() { hits++; }
    for (var i = 0; i < EVT_LISTENERS; i++) {
        var el = document.createElement("span");
        el.addEventListener("ping", onHit);
        el.addEventListener("ping", function (e) { hits += e.eventPhase; });
        root.appendChild(el);
        leaves.push(el);
    }
    phases.listen_ms = now() - t;
    // Deep bubbling.
    t = now();
    var deep = root;
    var deepHits = 0;
    for (var d = 0; d < EVT_DEPTH; d++) {
        var next = document.createElement("div");
        next.addEventListener("bub", function () { deepHits++; });
        deep.appendChild(next);
        deep = next;
    }
    for (var b = 0; b < 2000; b++) deep.dispatchEvent(new Event("bub", {bubbles: true}));
    phases.bubble_ms = now() - t;
    // Delegation.
    t = now();
    var delegated = 0;
    root.addEventListener("tap", function (e) {
        if (e.target.tagName === "SPAN") delegated += 1;
    });
    for (var dl = 0; dl < leaves.length; dl++) leaves[dl].dispatchEvent(new Event("tap", {bubbles: true}));
    phases.delegate_ms = now() - t;
    // 100k synthetic dispatches (two listeners each).
    t = now();
    for (var x = 0; x < EVT_DISPATCHES; x++) leaves[x % leaves.length].dispatchEvent(new Event("ping"));
    phases.dispatch_ms = now() - t;
    // add/remove churn.
    t = now();
    var churnHits = 0;
    var churnTarget = leaves[0];
    function churn() { churnHits++; }
    for (var c = 0; c < EVT_CHURN; c++) {
        churnTarget.addEventListener("churn", churn);
        if (c % 4 === 0) churnTarget.dispatchEvent(new Event("churn"));
        churnTarget.removeEventListener("churn", churn);
    }
    phases.churn_ms = now() - t;
    // pointermove / scroll floods.
    t = now();
    var px = 0;
    var scroller = document.createElement("div");
    scroller.style.height = "100px";
    scroller.style.overflow = "auto";
    root.appendChild(scroller);
    var scrolls = 0;
    scroller.addEventListener("scroll", function () { scrolls++; });
    root.addEventListener("pointermove", function (e) { px += e.clientX; });
    for (var f = 0; f < EVT_FLOOD; f++) {
        leaves[f % leaves.length].dispatchEvent(new PointerEvent("pointermove", {bubbles: true, clientX: f % 640, clientY: 3}));
        scroller.dispatchEvent(new Event("scroll"));
    }
    phases.flood_ms = now() - t;
    check(hits > 0 && deepHits === EVT_DEPTH * 2000 && delegated === EVT_LISTENERS, "event counts");
    check(scrolls === EVT_FLOOD && churnHits === EVT_CHURN / 4, "flood counts");
    var total = 0;
    for (var key in phases) total += phases[key];
    phases.workload = "event-flood";
    phases.elapsed_ms = total;
    phases.checksum = fold(fold(fold(fold(hits, deepHits), delegated), churnHits), fold(px, scrolls));
    phases.count = leaves.length;
    emit(phases);
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
    else if (workload === "md-stream-append") runMdStream("append");
    else if (workload === "md-stream-rerender") runMdStream("rerender");
    else if (workload === "md-long-history") runMdLongHistory();
    else if (workload === "dom-ops") runDomOps();
    else if (workload === "css-ops") runCssOps();
    else if (workload === "animation") runAnimation();
    else if (workload === "event-flood") runEventFlood();
    else emit({workload: String(workload), status: "error", error: "unknown workload"});
} catch (error) {
    console.log("W3COS_PERF_RESULT " + JSON.stringify({
        schema: "w3cos-native-browser-v1",
        status: "error",
        error: String(error)
    }));
}
