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


def it(id, h=None, w=None, text=None, place=None, order=None, margin=None, self_=None, abs=None, min_w=None):
    d = {"id": id}
    for k, v in (("min_w", min_w), ("abs", abs), ("h", h), ("w", w), ("text", text), ("place", place), ("order", order), ("margin", margin), ("self", self_)):
        if v is not None:
            d[k] = v
    return d


H = [30, 10, 20, 40, 10, 10, 25]
IDS = "abcdefghi"
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
    case("rev-cols-fill", "50px 50px 50px", stack(), axis="column fill-reverse"),
    case("rev-cols-fill-gap", "60px 60px 60px", stack(), axis="column fill-reverse", gap=[8, 5]),
    case("rev-cols-fill-h", "50px 50px 50px", stack(5), axis="column fill-reverse", height=120),
    case("rev-cols-track", "40px 50px 60px", stack(), axis="column track-reverse"),
    case("rev-cols-both", "40px 50px 60px", stack(), axis="column fill-reverse track-reverse", gap=[3, 7]),
    case("rev-cols-span", "40px 40px 40px", [it("a", h=20), it("b", h=30, place="span 2"), it("c", h=10), it("d", h=10), it("e", h=15, place="span 2"), it("f", h=5)], axis="column fill-reverse track-reverse"),
    case("rev-cols-explicit", "40px 40px 40px", [it("a", h=20, place="3"), it("b", h=30), it("c", h=10), it("d", h=15, place="1"), it("e", h=10)], axis="column track-reverse"),
    case("rev-cols-justify", "70px 70px", [it("a", h=10, w=30), it("b", h=10, w=20), it("c", h=10, w=40)], axis="column track-reverse"),
    case("rev-cols-box", "60px 60px", [it("a", h=20, margin=5), it("b", h=10), it("c", h=15, margin=3), it("d", h=10)], gap=[4, 4], axis="column fill-reverse"),
    case("rev-rows-fill", "30px 30px", [it(x, w=w, h=10) for x, w in zip("abcde", [60, 40, 50, 30, 70])], axis="row fill-reverse"),
    case("rev-rows-fill-gap", "20px 20px 20px", [it(x, w=w, h=10) for x, w in zip("abcdef", [50, 80, 30, 60, 40, 20])], gap=[6, 4], axis="row fill-reverse"),
    case("rev-rows-track", "20px 30px", [it(x, w=w, h=10) for x, w in zip("abcde", [60, 40, 50, 30, 70])], axis="row track-reverse"),
    case("rev-rows-both", "20px 30px", [it(x, w=w, h=10) for x, w in zip("abcde", [60, 40, 50, 30, 70])], axis="row fill-reverse track-reverse", gap=[3, 7]),
    case("rev-rows-auto", "auto auto", [it("a", text="XX"), it("b", text="XXXX"), it("c", text="XXX"), it("d", text="X"), it("e", text="XX")], axis="row track-reverse"),
    case("rev-rows-fill-h", "20px 20px", [it("a", w=50, h=10), it("b", w=40, h=10), it("c", w=20, h=10)], axis="row fill-reverse", height=80),
    case("rev-rows-span", "20px 20px 20px", [it("a", w=50, h=10), it("b", w=40, h=10, place="span 2"), it("c", w=30, h=10), it("d", w=60, h=10)], axis="row fill-reverse"),]


def al_items():
    hs = [70, 40, 120, 60, 50, 80, None, None, None]
    return [it(IDS[i], h=h) for i, h in enumerate(hs)]


def al_rows():
    ws = [60, None, 40, None, 50, None, 20]
    texts = [None, "XX", None, "XXXX", None, "X", None]
    return [it(IDS[i], w=w, h=10, text=t) for i, (w, t) in enumerate(zip(ws, texts))]


for rev in ("", " fill-reverse"):
    tag = "rev" if rev else "fwd"
    for al in ("start", "center", "end", "stretch"):
        CASES.append(case(f"al-{tag}-cols-{al}", "80px 80px 80px", al_items(), gap=[5, 5], align=al, axis="column" + rev))
        CASES.append(case(f"al-{tag}-cols-{al}-h", "80px 80px 80px", al_items(), gap=[5, 5], align=al, axis="column" + rev, height=300))
    CASES.append(case(f"al-{tag}-cols-span", "80px 80px 80px", [it("a", h=40, place="span 2"), it("b", h=60), it("c", h=50, place="span 3"), it("d", h=30), it("e", h=60), it("f", place="span 2"), it("g")], gap=[5, 5], align="center", axis="column" + rev, height=300))
    CASES.append(case(f"al-{tag}-cols-self", "80px 80px 80px", [it("a", h=40), it("b", h=60, self_="end"), it("c", h=20), it("d", h=30), it("e", h=10, self_="center"), it("f", h=25), it("g", h=15)], gap=[5, 5], axis="column" + rev, height=200))
    CASES.append(case(f"al-{tag}-cols-self-auto", "80px 80px 80px", [it("a", h=40), it("b", h=60, self_="end"), it("c", h=20), it("d", h=30), it("e", h=10, self_="center"), it("f", h=25), it("g", h=15)], gap=[5, 5], axis="column" + rev))
    for al in ("center", "end", "stretch"):
        CASES.append(case(f"al-{tag}-rows-{al}", "20px 20px 20px", al_rows(), gap=[5, 5], align=al, axis="row" + rev))


def dense_cases():
    S = "grid-lanes-pack:dense"
    out = []
    sp = [it("a", h=20), it("b", h=30, place="span 2"), it("c", h=10), it("d", h=10), it("e", h=15, place="span 2"), it("f", h=5), it("g", h=20), it("h", h=12)]
    out.append(case("d-cols-span", "40px 40px 40px", sp, pack=S))
    out.append(case("d-cols-span-none", "40px 40px 40px", sp))
    out.append(case("d-cols-span-both", "40px 40px 40px", sp, pack="grid-auto-flow:dense;grid-lanes-pack:dense"))
    out.append(case("d-cols-span-tol0", "40px 40px 40px", sp, pack=S, tolerance="0"))
    out.append(case("d-cols-span-gap", "40px 40px 40px", sp, pack=S, gap=[8, 5]))
    out.append(case("d-cols-single", "50px 50px 50px", stack(), pack=S))
    out.append(case("d-cols-single-tol0", "50px 50px 50px", stack(), pack=S, tolerance="0"))
    out.append(case("d-cols-single-tol-inf", "50px 50px 50px", stack(), pack=S, tolerance="infinite"))
    out.append(case("d-cols-single-gap", "50px 50px 50px", stack(), pack=S, gap=[6, 4]))
    out.append(case("d-cols-explicit", "40px 40px 40px", [it("a", h=30), it("b", h=10, place="3"), it("c", h=10, place="3"), it("d", h=15, place="1"), it("e", h=10, place="span 2"), it("f", h=10)], pack=S))
    out.append(case("d-cols-four", "30px 30px 30px 30px", [it("a", h=40), it("b", h=20, place="span 3"), it("c", h=10), it("d", h=10), it("e", h=10, place="span 2"), it("f", h=20), it("g", h=5), it("h", h=10, place="span 4"), it("i", h=7)], pack=S, width=150))
    out.append(case("d-cols-uneven", "40px 40px 60px 60px", [it("a", h=30), it("b", h=10, place="span 2"), it("c", h=10), it("d", h=10), it("e", h=15, place="span 2"), it("f", h=5), it("g", h=20)], pack=S, width=220))
    out.append(case("d-cols-order", "40px 40px 40px", [it("a", h=20, order=2), it("b", h=30, place="span 2", order=1), it("c", h=10), it("d", h=10, order=-1), it("e", h=15, place="span 2"), it("f", h=5)], pack=S))
    out.append(case("d-cols-box", "50px 50px 50px", [it("a", h=20, margin=3), it("b", h=30, place="span 2", margin=2), it("c", h=10), it("d", h=10, margin=4), it("e", h=15, place="span 2"), it("f", h=5)], pack=S, gap=[3, 4]))
    out.append(case("d-cols-fill-rev", "40px 40px 40px", sp, pack=S, axis="column fill-reverse", height=150))
    out.append(case("d-cols-track-rev", "40px 40px 40px", sp, pack=S, axis="column track-reverse"))
    out.append(case("d-cols-both-rev", "40px 40px 40px", sp, pack=S, axis="column fill-reverse track-reverse", height=150))
    out.append(case("d-cols-align-center", "40px 40px 40px", sp, pack=S, align="center", height=150))
    out.append(case("d-cols-align-end", "40px 40px 40px", sp, pack=S, align="end", height=150))
    out.append(case("d-cols-align-stretch", "40px 40px 40px", sp, pack=S, align="stretch", height=150))
    out.append(case("d-cols-align-self", "40px 40px 40px", [it("a", h=20), it("b", h=30, place="span 2", self_="end"), it("c", h=10), it("d", h=10, self_="center"), it("e", h=15, place="span 2"), it("f", h=5)], pack=S, height=150))
    rw = [it("a", w=40, h=10), it("b", w=60, h=10, place="span 2"), it("c", w=30, h=10), it("d", w=30, h=10), it("e", w=50, h=10, place="span 2"), it("f", w=20, h=10), it("g", w=40, h=10), it("h", w=25, h=10)]
    out.append(case("d-rows-span", "20px 20px 20px", rw, pack=S, axis="row"))
    out.append(case("d-rows-span-tol0", "20px 20px 20px", rw, pack=S, axis="row", tolerance="0"))
    out.append(case("d-rows-span-gap", "20px 20px 20px", rw, pack=S, axis="row", gap=[6, 4]))
    out.append(case("d-rows-single", "20px 20px 20px", [it(x, w=w, h=10) for x, w in zip("abcdefg", [60, 20, 40, 80, 20, 20, 50])], pack=S, axis="row"))
    out.append(case("d-rows-explicit", "20px 20px 20px", [it("a", w=60, h=10), it("b", w=20, h=10, place="3"), it("c", w=30, h=10, place="3"), it("d", w=25, h=10, place="1"), it("e", w=20, h=10, place="span 2"), it("f", w=20, h=10)], pack=S, axis="row"))
    out.append(case("d-rows-track-rev", "20px 20px 20px", rw, pack=S, axis="row track-reverse"))
    out.append(case("d-rows-fill-rev", "20px 20px 20px", rw, pack=S, axis="row fill-reverse"))
    out.append(case("d-rows-align-center", "20px 20px 20px", rw, pack=S, axis="row", align="center"))
    out.append(case("d-rows-align-stretch", "20px 20px 20px", rw, pack=S, axis="row", align="stretch"))
    out.append(case("d-rows-auto", "auto auto auto", [it("a", text="XXXX"), it("b", text="XXXXXX", place="span 2"), it("c", text="XX"), it("d", text="XXX"), it("e", text="XXXXX", place="span 2"), it("f", text="X")], pack=S, axis="row"))
    # Cases lifted from the WPT dense-packing tests (explicit and split openings).
    out.append(case("d-wpt-row-004", "50px 50px 50px 50px", [it("a", w=20), it("b", w=40), it("c", w=60, place="4"), it("d", w=20, place="span 2"), it("e", w=20, place="span 4"), it("f", w=20, place="span 3"), it("g", w=20, place="span 4"), it("h", w=20, place="4"), it("i", w=20)], pack=S, axis="row", tolerance="0"))
    out.append(case("d-wpt-row-ms-004", "50px 50px 50px 50px", [it("a", w=20, place="span 2"), it("b", w=20, place="span 4"), it("c", w=40, place="3 / span 2"), it("d", w=20, place="span 4"), it("e", w=20, place="1 / span 2"), it("f", w=20, place="span 2")], pack=S, axis="row", tolerance="0"))
    out.append(case("d-wpt-row-rev-ms-005", "50px 50px 50px 50px 50px", [it("a", w=80), it("b", w=20, place="span 2"), it("c", w=40), it("d", w=100), it("e", w=10, place="span 5"), it("f", w=10, place="span 4"), it("g", w=10, place="span 2")], pack=S, axis="row track-reverse", tolerance="0"))
    return out


CASES += dense_cases()


def intrinsic_repeat_cases():
    t = [it("a", text="XXXX"), it("b", text="XX", place="span 2"), it("c", text="XXX"), it("d", text="XXXXXX", place="span 2"), it("e", text="X")]
    return [
        case("ir-fill-auto", "repeat(auto-fill, auto)", t),
        case("ir-fill-auto-fixed", "repeat(auto-fill, auto 20px)", t),
        case("ir-fill-minmax-auto", "repeat(auto-fill, minmax(auto, 40px))", t),
        case("ir-fit-auto", "repeat(auto-fit, auto)", t, width=300),
        case("ir-fit-fixed", "repeat(auto-fit, minmax(30px, 1fr))", [it("a", h=10), it("b", h=20)]),
        case("ir-fit-fixed-fill", "repeat(auto-fill, minmax(30px, 1fr))", [it("a", h=10), it("b", h=20)]),
        case("ir-fit-span", "repeat(auto-fit, minmax(30px, 1fr))", [it("a", h=10), it("b", h=20, place="span 2"), it("c", h=15)]),
        case("ir-fit-rows", "repeat(auto-fit, auto)", [it("a", text="XX", w=20), it("b", text="XXXX", w=30), it("c", w=10, h=20)], axis="row", height=100),
        case("ir-fill-rows-indef", "repeat(auto-fill, 20px)", [it("a", w=10, h=10), it("b", w=20, h=10)], axis="row", height=90),
    ]


CASES += intrinsic_repeat_cases()


def oof_cases():
    base = [it("a", h=20), it("b", h=30), it("c", h=10)]
    out = []
    for pos in ("relative", "static"):
        tag = pos[:3]
        out += [
            case(f"oof-{tag}-col-auto", "50px 50px", base + [it("o", w=10, h=10, abs={})], position=pos),
            case(f"oof-{tag}-col-place", "50px 50px", base + [it("o", w=10, h=10, place="2", abs={})], position=pos),
            case(f"oof-{tag}-col-inset", "50px 50px", base + [it("o", place="2", abs={"left": 5, "top": 5, "right": 5, "bottom": 5})], position=pos),
            case(f"oof-{tag}-col-span", "50px 50px 50px", base + [it("o", w=10, h=10, place="2 / span 2", abs={"left": 0, "top": 0})], position=pos),
            case(f"oof-{tag}-col-fillrev", "50px 50px", base + [it("o", w=10, h=10, abs={})], axis="column fill-reverse", position=pos),
            case(f"oof-{tag}-col-fillrev-h", "50px 50px", base + [it("o", w=10, h=10, abs={})], axis="column fill-reverse", height=100, position=pos),
            case(f"oof-{tag}-col-only", "50px 50px", [it("o", w=10, h=10, abs={})], position=pos),
            case(f"oof-{tag}-col-gap", "50px 50px", base + [it("o", w=10, h=10, place="2", abs={"left": 2, "top": 2})], gap=[4, 6], position=pos),
            case(f"oof-{tag}-col-order", "40px 40px", [it("o", w=10, h=10, abs={}), it("a", h=20), it("b", h=15)], position=pos),
            case(f"oof-{tag}-row-auto", "30px 30px", [it(x, w=w, h=10) for x, w in zip("abc", [60, 40, 50])] + [it("o", w=10, h=10, abs={})], axis="row", position=pos),
            case(f"oof-{tag}-row-place", "30px 30px", [it(x, w=w, h=10) for x, w in zip("abc", [60, 40, 50])] + [it("o", w=10, h=10, place="2", abs={})], axis="row", position=pos),
            case(f"oof-{tag}-row-inset", "30px 30px", [it(x, w=w, h=10) for x, w in zip("abc", [60, 40, 50])] + [it("o", place="2", abs={"left": 3, "top": 3, "right": 3, "bottom": 3})], axis="row", position=pos),
            case(f"oof-{tag}-row-fillrev", "30px 30px", [it(x, w=w, h=10) for x, w in zip("abc", [60, 40, 50])] + [it("o", w=10, h=10, abs={})], axis="row fill-reverse", position=pos),
            case(f"oof-{tag}-row-h", "30px 30px", [it(x, w=w, h=10) for x, w in zip("abc", [60, 40, 50])] + [it("o", w=10, h=10, place="2", abs={})], axis="row", height=100, position=pos),
        ]
    for name, tmpl in (("mc", "minmax(0px, min-content) 5px"), ("xc", "5px minmax(0px, max-content)"), ("mm", "minmax(0px, min-content) 5% minmax(0px, max-content)")):
        n = tmpl.count(" ") - tmpl.count(", ") + 1
        out.append(case(f"oof-rel-span-{name}", tmpl, [it("a", text="XXXX XXXX", place="1 / -1")] + [it(f"o{i}", w=5, h=5, place=str(i), abs={}) for i in range(1, n + 1)]))
    for mw in (15, 55):
        out.append(case(f"oof-rel-span-minw{mw}", "minmax(0px, min-content) 5px", [it("a", text="XXXX XXXX", place="1 / -1", min_w=mw)] + [it(f"o{i}", w=5, h=5, place=str(i), abs={}) for i in (1, 2)], justify="start", width=100))
    out.append(case("oof-rel-span-area", "minmax(0px, min-content) 5px", [it("a", text="XXXX XXXX", place="1 / -1", min_w=15)] + [it(f"o{i}", place=f"{i} / {i + 1}", abs={"left": 0, "right": 0, "top": 0, "bottom": 0}) for i in (1, 2)], justify="start", width=100, pad=10))
    out.append(case("oof-rel-span-pct", "minmax(0px, min-content) 5px", [it("a", text="XXXX XXXX", place="1 / -1", min_w=15)] + [it(f"o{i}", place=f"{i} / {i + 1}", abs={"width": "100%", "height": "5px"}) for i in (1, 2)], justify="start", width=100, pad=10))
    out.append(case("oof-rel-span-pcth", "minmax(0px, min-content) 5px", [it("a", text="XXXX XXXX", h=20, place="1 / -1", min_w=15)] + [it(f"o{i}", place=f"{i} / {i + 1}", abs={"width": "100%", "height": "5px"}) for i in (1, 2)], justify="start", width=100, pad=10, height=20))
    for nm, mw in (("15", 15), ("55", 55)):  # fit-content tracks bail (oof)
        out.append(case("oof-rel-span-fitautonoabs" + nm, "fit-content(100px) 5px minmax(0px, auto)", [it("a", text="XXXX XXXX", h=20, place="1 / -1", min_w=mw)], justify="start", width=100, pad=10, height=20))
    # Insets against the initial containing block depend on the headless
    # window size, so only the relative container has them.
    return [c for c in out if not (c['name'].startswith('oof-sta-') and c['name'].endswith('-inset'))]


CASES += oof_cases()


def html(c):
    k = c["container"]
    # The axis follows the template that is set, or the direction keyword.
    rows_axis = (k.get("axis") or "").startswith("row") or (k.get("axis") is None and k.get("rows", False))
    style = ["display:grid-lanes", "font:10px/1 Ahem"]
    prop = "grid-template-rows" if rows_axis else "grid-template-columns"
    style.append(f"{prop}:{k['template']}")
    if k.get("axis"):
        style.append(f"grid-lanes-direction:{k['axis']}")
    if k.get("align"):
        style.append(f"{'justify-items' if rows_axis else 'align-items'}:{k['align']}")
    if k.get("justify"):
        style.append(f"{'align-items' if rows_axis else 'justify-items'}:{k['justify']}")
    if k.get("pad"):
        style.append(f"padding-top:{k['pad']}px;border:2px solid")
    if k.get("tolerance"):
        style.append(f"flow-tolerance:{k['tolerance']}")
    if k.get("pack"):
        style.append(f"{k['pack']}")
    if k.get("gap"):
        style.append(f"row-gap:{k['gap'][0]}px;column-gap:{k['gap'][1]}px")
    style.append(f"width:{k.get('width', 200)}px")
    pos = k.get('position', 'relative')
    if k.get("height"):
        style.append(f"height:{k['height']}px")
    kids = []
    for i in c["items"]:
        s = []
        if "w" in i:
            s.append(f"width:{i['w']}px")
        if "h" in i:
            s.append(f"height:{i['h']}px")
        if "min_w" in i:
            s.append(f"min-width:{i['min_w']}px")
        if "place" in i:
            s.append(f"{'grid-row' if rows_axis else 'grid-column'}:{i['place']}")
        if "order" in i:
            s.append(f"order:{i['order']}")
        if "self" in i:
            s.append(f"{'justify-self' if rows_axis else 'align-self'}:{i['self']}")
        if "abs" in i:
            s.append("position:absolute")
            s += [f"{k}:{v}" if isinstance(v, str) else f"{k}:{v}px" for k, v in i["abs"].items()]
        if "margin" in i:
            s.append(f"margin:{i['margin']}px;padding:2px;border:1px solid")
        kids.append(f"<div id={c['name']}-{i['id']} style='{';'.join(s)}'>{i.get('text', '')}</div>")
    return f"<div id={c['name']} class=case style='position:{pos};{';'.join(style)}'>{''.join(kids)}</div>"


def main():
    out = sys.argv[1]
    ahem = base64.b64encode(open(AHEM, "rb").read()).decode()
    # One page per case: a failing case cannot hide the others.
    result = {}
    version = subprocess.run([CHROME, "--version"], capture_output=True, text=True).stdout.strip()
    only = os.environ.get("W3COS_ONLY")
    for c in CASES:
        if only and not c["name"].startswith(only):
            continue
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
