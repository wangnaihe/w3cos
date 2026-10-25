#!/usr/bin/env python3
"""Chrome oracle for LayoutNG differential tests.

Reads the JSONL written by the layout_ng generated-tree test
(W3COS_NG_FUZZ_DUMP=path cargo test ... layout_ng::tests::generated),
renders each case in headless Chromium (800x600, body margin 0) and
compares every box's border-box rect with the NG and legacy rects.

usage: chrome_oracle.py DUMP.jsonl [--show N] [--write-golden OUT.json]

--write-golden stores Chrome's rects per case (the checked-in expectations
the Rust test compares NG against).
"""
import base64, json, os, sys
from playwright.sync_api import sync_playwright

HERE = os.path.dirname(os.path.abspath(__file__))
FONTS = {
    "Ahem": os.path.join(HERE, "../../crates/w3cos-runtime/src/layout_ng/testdata/Ahem.ttf"),
}


def font_faces(names):
    css = ""
    for name in names:
        with open(FONTS[name], "rb") as f:
            data = base64.b64encode(f.read()).decode()
        css += f"@font-face{{font-family:{name};src:url(data:font/ttf;base64,{data})}}"
    return f"<style>{css}</style>" if css else ""

EPS = 1.0 / 64.0


def same(a, b):
    return a is not None and b is not None and all(abs(x - y) <= EPS for x, y in zip(a, b))


def main():
    path = sys.argv[1]
    show = int(sys.argv[sys.argv.index("--show") + 1]) if "--show" in sys.argv else 5
    cases = [json.loads(line) for line in open(path)]
    stats = {"cases": 0, "ng_ok": 0, "legacy_ok": 0, "both_ok": 0}
    shown = 0
    golden = {}
    with sync_playwright() as pw:
        # Same browser as the perf harness (W3COS_CHROME, default system Chrome).
        chrome_path = os.environ.get(
            "W3COS_CHROME", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
        )
        browser = pw.chromium.launch(
            executable_path=chrome_path,
            headless=True,
            args=["--disable-gpu", "--force-device-scale-factor=1"],
        )
        page = browser.new_page(viewport={"width": 800, "height": 600})
        for case in cases:
            page.set_content(
                "<!doctype html><html><head>" + font_faces(case.get("fonts", []))
                + "</head><body style='margin:0'>" + case["html"] + "</body></html>"
            )
            page.evaluate("() => document.fonts.ready.then(() => true)")
            chrome = page.evaluate(
                """() => Object.fromEntries([...document.querySelectorAll('[id^=n]')].map(e => {
                    const r = e.getBoundingClientRect();
                    return [e.id.slice(1), [r.x, r.y, r.width, r.height]];
                }))"""
            )
            golden[str(case["case"])] = {k: [round(x * 64) for x in v] for k, v in chrome.items()}
            # A collapsible space removed at a line edge: no NG fragment,
            # zero-width Chrome rect.
            ok = lambda c, k: same(chrome[k], c.get(k)) or (k not in c and abs(chrome[k][2]) <= EPS)
            ng_ok = all(ok(case["ng"], k) for k in chrome)
            legacy_ok = all(ok(case["legacy"], k) for k in chrome)
            stats["cases"] += 1
            stats["ng_ok"] += ng_ok
            stats["legacy_ok"] += legacy_ok
            stats["both_ok"] += ng_ok and legacy_ok
            if not ng_ok and shown < show:
                shown += 1
                print(f"--- case {case['case']} (legacy {'ok' if legacy_ok else 'differs'})")
                for k in sorted(chrome, key=int):
                    if not ok(case["ng"], k):
                        print(f"  n{k}: chrome {chrome[k]} ng {case['ng'].get(k)} legacy {case['legacy'].get(k)}")
        browser.close()
    if "--write-golden" in sys.argv:
        out = sys.argv[sys.argv.index("--write-golden") + 1]
        with open(out, "w") as f:
            f.write("{\n" + ",\n".join(f'"{k}":{json.dumps(v, separators=(",", ":"))}' for k, v in golden.items()) + "\n}\n")
    print(json.dumps(stats))


if __name__ == "__main__":
    main()
