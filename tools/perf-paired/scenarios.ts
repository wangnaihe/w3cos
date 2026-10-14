// Shared JavaScript-compatible TypeScript. Execute these exact bytes on both engines.
// Timed work includes validation and any synchronous DOM invalidation/GC work.
// No forced layout, frame wait, paint, startup, or display-latency measurement.
export {};
const NODES = 256;
const ROUNDS = 32;
const WARMUP = 3;
const SAMPLES = 7;
const METRIC = "synchronous-dom-script-wall-ms-v1";
function check(ok) {
    if (!ok) throw new Error("DOM correctness check failed");
}
function pool() {
    const root = document.createElement("div");
    document.body.appendChild(root);
    const nodes = [];
    for (let i = 0; i < NODES; i++) nodes.push(document.createElement("span"));
    return {root: root, nodes: nodes};
}
function appendRemove(p) {
    let checks = 0;
    for (let r = 0; r < ROUNDS; r++) {
        for (let i = 0; i < NODES; i++) {
            check(p.root.appendChild(p.nodes[i]) === p.nodes[i]);
            check(p.nodes[i].parentNode === p.root);
            checks += 2;
        }
        const children = p.root.childNodes;
        check(children.length === NODES);
        checks++;
        for (let i = 0; i < NODES; i++) {
            check(children[i] === p.nodes[i]);
            checks++;
        }
        for (let i = NODES - 1; i >= 0; i--) {
            check(p.root.removeChild(p.nodes[i]) === p.nodes[i]);
            check(p.nodes[i].parentNode === null);
            checks += 2;
        }
        check(p.root.childNodes.length === 0);
        checks++;
    }
    return {checks: checks, final_children: p.root.childNodes.length};
}
function classMatches(p) {
    let checks = 0;
    let trueMatches = 0;
    let falseMatches = 0;
    for (let r = 0; r < ROUNDS; r++) {
        const active = r % 2 === 0;
        const value = active ? "bench active" : "bench inactive";
        for (let i = 0; i < NODES; i++) {
            const node = p.nodes[i];
            node.className = value;
            check(node.className === value);
            const a = node.matches(".active");
            const b = node.matches(".inactive");
            check(a === active);
            check(b === !active);
            checks += 3;
            trueMatches += (a ? 1 : 0) + (b ? 1 : 0);
            falseMatches += (a ? 0 : 1) + (b ? 0 : 1);
        }
    }
    return {checks: checks, true_matches: trueMatches, false_matches: falseMatches,
            final_class: p.nodes[0].className};
}
function measure(id, run, p) {
    const samples = [];
    for (let i = 0; i < WARMUP + SAMPLES; i++) {
        const start = performance.now();
        const evidence = run(p);
        const elapsed = performance.now() - start;
        if (i >= WARMUP) samples.push({phase: "measurement", index: i - WARMUP, elapsed_ms: elapsed, evidence: evidence});
    }
    return {id: id, samples: samples};
}
try {
    const a = pool();
    const b = pool();
    for (let i = 0; i < NODES; i++) {
        b.nodes[i].className = "bench inactive";
        b.root.appendChild(b.nodes[i]);
    }
    const results = [measure("append-remove-pool", appendRemove, a),
                     measure("class-matches", classMatches, b)];
    check(a.root.childNodes.length === 0);
    check(b.root.childNodes.length === NODES);
    for (let i = 0; i < NODES; i++) check(b.nodes[i].className === "bench inactive");
    const payload = {schema: "w3cos-dom-api-v1", status: "ok", metric: METRIC,
        reset: "fresh-process-fixed-node-pools-v1",
        parameters: {nodes: NODES, rounds: ROUNDS, warmup: WARMUP, samples: SAMPLES},
        viewport: {width: window.innerWidth, height: window.innerHeight, dpr: window.devicePixelRatio},
        scenarios: results};
    console.log("W3COS_PERF_RESULT " + JSON.stringify(payload));
} catch (error) {
    console.log("W3COS_PERF_RESULT " + JSON.stringify({schema: "w3cos-dom-api-v1", status: "error", error: String(error)}));
}
