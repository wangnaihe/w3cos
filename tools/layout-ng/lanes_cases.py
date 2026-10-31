#!/usr/bin/env python3
"""Chrome oracle for display: grid-lanes (testdata/chrome-grid-lanes.json).

Renders every case in headless Chrome (`--dump-dom`, Ahem 10px/1) and
writes the spec plus Chrome's border boxes. The Rust test
`grid_lanes_matches_chrome` rebuilds each tree from the same spec.

usage: lanes_cases.py OUT.json
"""
import base64, json, os, subprocess, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
AHEM = os.path.join(HERE, "../../crates/w3cos-runtime/src/layout_ng/testdata/Ahem.ttf")
CHROME = os.environ.get("W3COS_CHROME", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")


def case(name, template, items, **container):
    return {"name": name, "container": {"template": template, **container}, "items": items}


def it(id, h=None, w=None, text=None, place=None, order=None, margin=None):
    d = {"id": id}
    for k, v in (("h", h), ("w", w), ("text", text), ("place", place), ("order", order), ("margin", margin)):
        if v is not None:
            d[k] = v
    return d


H = [30, 10, 20, 40, 10, 10, 25]
IDS = "abcdefg"
def stack(n=7):
    return [it(IDS[i], h=H[i]) for i in range(n)]


CASES = [
    case("cols-basic", "50px 50px 50px", stack()),
    case("cols-gap", "60px 60px 60px", stack(), gap=[8, 5]),
    case("cols-tol0", "50px 50px 50px", [it(x, h=h) for x, h in zip("abcdef", [30, 25, 28, 5, 5, 5])], tolerance="0"),
    case("cols-tol-default", "50px 50px 50px", [it(x, h=h) for x, h in zip("abcdef", [30, 25, 28, 5, 5, 5])]),
    case("cols-tol-20", "50px 50px 50px", [it(x, h=h) for x, h in zip("abcdef", [30, 25, 28, 5, 5, 5])], tolerance="20px"),
    case("cols-tol-inf", "50px 50px 50px", [it(x, h=h) for x, h in zip("abcdef", [30, 25, 28, 5, 5, 5])], tolerance="infinite"),
    case("cols-span", "40px 40px 40px", [it("a", h=20), it("b", h=30, place="span 2"), it("c", h=10), it("d", h=10), it("e", h=15, place="span 2"), it("f", h=5)]),
    case("cols-explicit", "40px 40px 40px", [it("a", h=20, place="3"), it("b", h=30), it("c", h=10), it("d", h=15, place="1"), it("e", h=10)]),
    case("cols-order", "50px 50px", [it("a", h=20, order=2), it("b", h=30, order=1), it("c", h=10), it("d", h=10, order=-1)]),
    case("cols-auto", "auto auto", [it("a", text="XX"), it("b", text="XXXX"), it("c", text="XXX"), it("d", text="X")]),
    case("cols-fr", "1fr 2fr", [it("a", h=10), it("b", h=20), it("c", h=30), it("d", h=5)], width=210),
    case("cols-box", "60px 60px", [it("a", h=20, margin=5), it("b", h=10), it("c", h=15, margin=3), it("d", h=10)], gap=[4, 4]),
    case("cols-repeat", "repeat(auto-fill, 45px)", stack(5)),
    case("cols-justify", "70px 70px", [it("a", h=10, w=30), it("b", h=10, w=20), it("c", h=10, w=40)]),
    case("cols-height", "50px 50px", [it("a", h=10), it("b", h=20)], height=100),
    case("rows-basic", "30px 30px", [it(x, w=w, h=h) for x, w, h in zip("abcde", [60, 40, 50, 30, 70], [10, 10, 10, 10, 10])], rows=True),
    case("rows-auto", "auto auto", [it("a", text="XX"), it("b", text="XXXX"), it("c", text="XXX"), it("d", text="X"), it("e", text="XX")], axis="row"),
    case("rows-gap", "20px 20px 20px", [it(x, w=w, h=10) for x, w in zip("abcdef", [50, 80, 30, 60, 40, 20])], gap=[6, 4], rows=True),
    case("rows-span", "20px 20px 20px", [it("a", w=50, h=10), it("b", w=40, h=10, place="span 2"), it("c", w=30, h=10), it("d", w=60, h=10)], rows=True),
    case("rows-explicit", "20px 20px", [it("a", w=50, h=10, place="2"), it("b", w=40, h=10), it("c", w=30, h=10)], rows=True),
    case("rows-tol0", "20px 20px", [it("a", w=50, h=10), it("b", w=45, h=10), it("c", w=20, h=10), it("d", w=20, h=10)], tolerance="0", rows=True),
    case("rows-stretch", "auto auto", [it("a", w=50), it("b", w=30, h=15), it("c", w=40)], height=80, rows=True),
    case("rows-wrap-width", "30px 30px", [it("a", text="XX XX XX"), it("b", text="XX"), it("c", text="XXXXXXXXXXXXXXXXXXXXXXXX")], axis="row", width=100),
    case("axis-row-cols", "50px 50px", [it("a", w=30, h=10), it("b", w=40, h=10)], axis="row"),
]


def html(c):
    k = c["container"]
    # The axis follows the template that is set, or the direction keyword.
    rows_axis = k.get("axis") == "row" or (k.get("axis") is None and k.get("rows", False))
    style = ["display:grid-lanes", "font:10px/1 Ahem"]
    prop = "grid-template-rows" if rows_axis else "grid-template-columns"
    style.append(f"{prop}:{k['template']}")
    if k.get("axis"):
        style.append(f"grid-lanes-direction:{k['axis']}")
    if k.get("tolerance"):
        style.append(f"flow-tolerance:{k['tolerance']}")
    if k.get("gap"):
        style.append(f"row-gap:{k['gap'][0]}px;column-gap:{k['gap'][1]}px")
    style.append(f"width:{k.get('width', 200)}px")
    if k.get("height"):
        style.append(f"height:{k['height']}px")
    kids = []
    for i in c["items"]:
        s = []
        if "w" in i:
            s.append(f"width:{i['w']}px")
        if "h" in i:
            s.append(f"height:{i['h']}px")
        if "place" in i:
            s.append(f"{'grid-row' if rows_axis else 'grid-column'}:{i['place']}")
        if "order" in i:
            s.append(f"order:{i['order']}")
        if "margin" in i:
            s.append(f"margin:{i['margin']}px;padding:2px;border:1px solid")
        kids.append(f"<div id={c['name']}-{i['id']} style='{';'.join(s)}'>{i.get('text', '')}</div>")
    return f"<div id={c['name']} class=case style='position:relative;{';'.join(style)}'>{''.join(kids)}</div>"


def main():
    out = sys.argv[1]
    ahem = base64.b64encode(open(AHEM, "rb").read()).decode()
    # One page per case: a failing case cannot hide the others.
    result = {}
    version = subprocess.run([CHROME, "--version"], capture_output=True, text=True).stdout.strip()
    for c in CASES:
        body = html(c)
        page = (
            "<!doctype html><html><head><style>@font-face{font-family:Ahem;src:url(data:font/ttf;base64,"
            + ahem
            + ")}body{margin:0}</style></head><body>"
            + body
            + "<pre id=out></pre><script>document.fonts.load('10px Ahem').then(()=>{"
            "const r={};for(const e of document.querySelectorAll('[id]')){if(e.id=='out')continue;"
            "const b=e.getBoundingClientRect();r[e.id]=[b.x,b.y,b.width,b.height];}"
            "document.getElementById('out').textContent='@@'+JSON.stringify(r)+'@@';});</script></body></html>"
        )
        with tempfile.NamedTemporaryFile("w", suffix=".html", delete=False) as f:
            f.write(page)
        dom = subprocess.run(
            [CHROME, "--headless=new", "--disable-gpu", "--force-device-scale-factor=1",
             "--window-size=800,600", "--enable-experimental-web-platform-features", "--virtual-time-budget=3000", "--dump-dom", "file://" + f.name],
            capture_output=True, text=True,
        ).stdout
        os.unlink(f.name)
        raw = dom.split("@@")[1]
        rects = json.loads(raw.replace("&quot;", '"'))
        result[c["name"]] = {"spec": c, "html": body, "rects": rects}
    json.dump({"browser": version, "cases": result}, open(out, "w"), indent=1)
    for n, v in result.items():
        print(n, {k: [round(x, 2) for x in r] for k, r in v["rects"].items()})


main()
