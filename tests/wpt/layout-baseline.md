# Layout WPT baseline (LayoutNG rewrite merge gate)

Suite: `tests/wpt/layout/suite-css2.json` (2025 cases, run with
`tests/wpt/profiles/css2-userstyle.css`) and `suite-css3.json` (7753 cases):
every runnable reftest/testharness case under css/CSS2/{floats,floats-clear,
linebox,margin-padding-clear,normal-flow} and css/css-{display,flexbox,grid,
inline,overflow,position,sizing,tables,text}, WPT revision
`fa5393bb9f5f7d41cc16d1aeede1809ccd378ac0`, viewport 800x600.

WPT checkout (sparse) needs: `common resources fonts images infrastructure
css/support css/reference css/CSS2 css/css-{flexbox,grid,tables,display,
position,sizing,overflow,text,inline} dom/nodes`.

Baseline pass set: `tests/wpt/layout-baseline-pass.txt` (origin/main 4a91b5a).
Gate: the candidate's pass set must be a superset of it (zero regressions).

```sh
cargo build --release -p w3cos-wpt
scripts/wpt-layout-gate.py run --bin target/release/w3cos-wpt --wpt-root ../wpt --out target/wpt-layout/run --jobs 8
scripts/wpt-layout-gate.py check --results target/wpt-layout/run      # exit 1 on any regression
scripts/wpt-layout-gate.py table --results target/wpt-layout/run      # per-dir counts
```

`run` executes 500-case `--case-start/--case-limit` chunks and skips finished
chunks, so an interrupted run resumes where it stopped.

## Baseline counts (2026-10-18, Apple Silicon Mac, --jobs 8)

| dir | total | main-4a91b5a | base-fa6d696 |
|---|---:|---:|---:|
| css/CSS2/floats | 120 | 107 | 107 |
| css/CSS2/floats-clear | 215 | 187 | 187 |
| css/CSS2/linebox | 198 | 57 | 57 |
| css/CSS2/margin-padding-clear | 682 | 665 | 665 |
| css/CSS2/normal-flow | 810 | 752 | 752 |
| css/css-display | 242 | 102 | 102 |
| css/css-flexbox | 1350 | 444 | 445 |
| css/css-grid | 2281 | 476 | 475 |
| css/css-inline | 234 | 43 | 43 |
| css/css-overflow | 651 | 156 | 154 |
| css/css-position | 350 | 113 | 113 |
| css/css-sizing | 733 | 168 | 169 |
| css/css-tables | 297 | 114 | 114 |
| css/css-text | 1615 | 469 | 471 |
| **all** | 9778 | 3853 | 3854 |

Wall time: main ~46 min (09:47–10:33 Asia/Shanghai, machine shared with other
work), #113 head css3 part 17 min.

Statuses on main: pass 3853, fail 5890, error 35.

#113 head (fa6d696) vs main: 4 new passes (flexbox animation discrete,
responsive-iframe allow-origins, letter-/word-spacing animating) and 3
regressions, reproducible:

- `css/css-grid/grid-relayout-with-nested-grid.html`,
  `css/css-overflow/overflow-canvas.html`: error — an uncaught exception in a
  classic script (undefined `forceLayout` / missing helper) now fails the
  document load instead of being reported (HTML "report the exception").
- `css/css-overflow/scrollable-overflow-fixed-positioned-initial-containing-block.html`:
  documentElement.scrollWidth 1568, expected 784 (fixed box with ICB containing
  block counted in document scrollable overflow).

These must be fixed on #113 before the gate can pass against the main baseline.
