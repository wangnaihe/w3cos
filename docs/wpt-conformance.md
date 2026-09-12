# Raw Web Platform Tests

> Status: first runnable baseline · 2026-08-22
> Upstream: <https://github.com/web-platform-tests/wpt>
> Pinned revision: `fa5393bb9f5f7d41cc16d1aeede1809ccd378ac0`

W3COS has two different kinds of conformance evidence:

- `tests/wpt/indexeddb-subset.json` maps selected upstream cases to adapted
  assertions. It does not execute the upstream WPT files.
- `w3cos-wpt` serves and executes unmodified WPT HTML, the upstream
  `resources/testharness.js`, and explicit reftest references through the
  dynamic document, DOM, layout, paint, and Skia paths.

Neither result is a claim that the full WPT repository passes.

## Normal line-height representation

`Style.line_height_is_normal` distinguishes the CSS `normal` keyword from an
explicit numeric `line_height`, including an explicit `1.2`. The optional JSON
field defaults to `false`, preserving numeric semantics for old serialized
styles and native component builders. CSS initial styles and omitted
line-height in the `font` shorthand set it to `true`; explicit line-height
values clear it, and inheritance carries both fields together. Font-dependent
used-value resolution must occur before inline lowering so layout struts and
paint receive the same resolved height. The runtime DOM tree entry point now
installs a revisioned provider using the registered face's shared fontdue line
metrics. Font registration/removal invalidates cached used values on the next
tree build. Unregistered fonts retain the numeric fallback; shadow/frame tree
entry points and mixed fallback-font line metrics still need separate proof.
Representation or provider wiring alone is not proof of WPT conformance.

Reftest capture waits for `document.fonts.ready` after the `reftest-wait`
condition clears, polling font fetches and microtasks within the case timeout.
This matters when a stylesheet finishes before the streaming parser appends
the text selecting a deferred face. At the pinned revision, the focused
4105–4112 run in `target/wpt-targeted/batch-4105-4112-font-ready-v1/results.json`
passed 8/8; case 4106 moved from 10,600 differing pixels to zero. This focused
receipt does not establish full-suite conformance.

Percentage-width table tracks are redistributed after layout using their
containing block's used content width, including shrink-wrapped absolute
ancestors. The focused 4121–4128 receipt at
`target/wpt-targeted/batch-4121-4128-percent-table-used-basis-v1/results.json`
passed 8/8; case 4122 moved from 24,960 differing pixels to zero. The adjacent
4113–4120 regression also passed 8/8. Full-suite acceptance remains pending.

Absolute/fixed internal table parts are blockified before DOM table lowering,
so track/background projections cannot overwrite their positioned geometry.
Static table parts retain their internal role. At the pinned revision, case
4184 moved from 10,000 differing pixels to zero in
`target/wpt-targeted/batch-4177-4184-abs-table-part-blockify-v1/results.json`.
The current 4177–4184, adjacent 4169–4176/4185–4192, and earlier
4105–4112/4121–4128 regression batches each passed 8/8.

A leading float does not consume a block's first inline line or text-indent.
Blocks with floats and otherwise inline flow establish the anonymous line box;
float-only blocks and mixed normal block flow are excluded. The focused
4265–4272 run at
`target/wpt-targeted/batch-4265-4272-leading-float-indent-v1/results.json`
passed 8/8, including case 4269's 40px indent assertion (previously 0px).
4257–4264 and 4105–4112 regression batches each passed 8/8.

Inline text fragments retain glyph-advance origins rather than individually
compensating negative ink bearings. Case 4292 moved from 78 differing pixels
to zero in `target/wpt-targeted/batch-4289-4296-inline-bearing-v1/results.json`;
4281–4288 and 4265–4272 regression batches also passed 8/8. The new serif
text-box fragmentation regression passed after first failing with 234 channel
differences. The older `default_ascii_text_is_pixel_invariant_across_inline_fragments`
test still fails both with and without this change; it is not counted as green.

RTL fixed-width block alignment preserves the relative offset selected by the
containing block's direction and translates descendants with the host. A
reverse preorder pass computes contiguous subtree ranges for translation.
Case 4299 moved from 18,432 differing pixels to zero in
`target/wpt-targeted/batch-4297-4304-rtl-relative-align-v1/results.json`.
4297–4304 and 4289–4296 each passed 8/8. The next batch, 4305–4312, passed
4/8 and remains the next repair scope; full-suite acceptance is pending.

The float-after-inline marker inspects the last relevant in-flow box, skipping
hidden/out-of-flow boxes and only collapsing CSS whitespace (not NBSP). A
preceding normal block no longer introduces a synthetic inline-line offset.
Case 4305 moved from 6,432 differing pixels to zero in
`target/wpt-targeted/batch-4305-4312-float-after-inline-v1/results.json` (5/8).
4309–4311 still fail; 4105–4112, 4265–4272 and 4297–4304 regressions passed 8/8.

Leading collapsible space inside an inline observes previous outer fragments
across inline ancestors, retaining one separator without duplicating an already
owned trailing space. Cases 4309–4311 moved from 567/560/1,120 differing pixels
to zero in `target/wpt-targeted/batch-4305-4312-outer-leading-space-v1/results.json`
(8/8). 4297–4304 and 4105–4112 regressions also passed 8/8. The next batch
4313–4320 passed 5/8, with cases 4314, 4318 and 4319 awaiting repair.

Forced-break projection derives the line top from an inline text em box by
removing its half-leading; following text restores its own half-leading while
replaced boxes use the line top. Case 4318 moved from 768 differing pixels to
zero in `target/wpt-targeted/batch-4313-4320-forced-break-line-top-v1/results.json`
(6/8). 4314/4319 remain failures; 4305–4312 and 4097–4104 regressions passed
8/8. Forced-break unit tests passed 5/7; the two remaining failures were
verified to produce identical results with the production change removed.

A left float encountered after inline content may share the current line when
the prior inline extent plus its margin box fits the used content width. The
float uses the line top and preceding inline subtrees move by its outer width;
insufficient room retains the next-line fallback. Case 4319 moved from 2,085
differing pixels to zero in
`target/wpt-targeted/batch-4313-4320-fitting-after-inline-float-v1/results.json`
(7/8). Case 4314 remains unresolved. 4265–4272 and 4105–4112 passed 8/8.
The new fitting-float unit passed; the older zero-width after-inline-float unit
also fails with the new fitting branch disabled and is not counted as green.

Case 4314 (`position-relative-035.xht`) remains unresolved, not skipped.
On 2026-09-12, Chromium 141.0.7390.37 at 800x600 with its computed
`16px / 20px Times` font also produced a 20px source orange box (y=110)
versus a 24px reference orange box (y=126). Both black boxes were y=66,
height=60. This is evidence of a reference mismatch in this browser/font
environment, not proof that the test is invalid on every platform. No fixture,
suite membership or pixel allowance was changed. Further font/reference
qualification is required before closing this failure.

At runtime commit `141d51b`, subsequent batches 4321–4328 and 4329–4336
passed 8/8 each. Reports are respectively
`target/wpt-targeted/batch-4321-4328-current-v1/results.json` and
`target/wpt-targeted/batch-4329-4336-current-v1/results.json`. These focused
results do not establish a green complete 6,548-case run.

The same runtime subsequently passed batches 4337–4344, 4345–4352,
4353–4360, 4361–4368, 4369–4376 and 4377–4384 (8/8 each). Each receipt is
`target/wpt-targeted/batch-<start>-<end>-current-v1/results.json` with those
exact range bounds. Together with the two preceding batches, 4321–4384
passed 64/64. WPT remained clean at revision
`fa5393bb9f5f7d41cc16d1aeede1809ccd378ac0` and the suite retained 6,548
entries; case 4314 remains an open failure outside this passing range.

The next ten eight-case batches, 4393–4472, passed 80/80 using the unchanged
runtime at `141d51b`. Receipts use
`target/wpt-targeted/batch-<start>-<end>-current-v1/results.json`, starting
at 4393 and advancing by eight through 4465. Comparison with the pinned
initial `target/wpt-all/results.json` found 23 failures and 57 passes in this
same range; those 23 initial failures now have focused passing evidence.
This is a range-level comparison, not a claim that all initial failures or
the complete suite are closed. Case 4314 is still unresolved.

CSS block text leaves now use the same glyph-advance origin as inline runs,
without shifting negative left ink bearings into the content box. Cases
4487/4488 were not selector failures: both had a one-pixel horizontal text
shift (506 differing pixels), while Chromium 141 placed source and reference
text at x=8. The new `block_and_inline_text_share_the_same_glyph_origin`
pixel regression failed with 1,518 differing channels before the change and
passed with zero after it. Three related inline-origin/half-leading tests
also passed; this is not a complete runtime-unit gate.

Cases 4487/4488 now have zero pixel differences in
`target/wpt-targeted/batch-4481-4488-block-glyph-origin-v1/results.json`
(8/8). Adjacent 4473–4480 and earlier origin regression 4289–4296 also
passed 8/8; their receipts use the same `block-glyph-origin-v1` suffix.
No WPT fixture, suite entry or tolerance was changed. Case 4314 remains open
and the final complete 6,548-case evidence is still pending.

At runtime commit `b8c6d1a`, six eight-case batches 4497–4544 passed 48/48.
Receipts use `target/wpt-targeted/batch-<start>-<end>-block-glyph-origin-v1/results.json`
with starts 4497 through 4537 advancing by eight. The following batch
4545–4552 passed 7/8: case 4546 (`first-letter-inherit-001.xht`) differed by
449 pixels. Its source uses `float: inherit` on ::first-letter while its
reference uses `float: left`; Chromium 141 computed `left` on both.

Explicit ::first-letter float inheritance now reads the originating block's
computed style instead of the lowered text fragment's initial `none` value.
The new `first_letter_float_inherit_uses_the_originating_block` regression
first reproduced `None` versus expected `Left`, then passed for left/right/none
origins. All five selected first-letter tests passed. Case 4546 now has zero
pixel differences in
`target/wpt-targeted/batch-4545-4552-first-letter-float-inherit-v1/results.json`
(8/8). 4537–4544 and 4481–4488 also passed 8/8 using the same receipt suffix.
These focused results do not close case 4314 or the complete 6,548-case run.

At runtime commit `4aa2924`, fifteen successive eight-case batches
4561–4680 passed 120/120. Receipts are
`target/wpt-targeted/batch-<start>-<end>-first-letter-float-inherit-v1/results.json`
with starts 4561 through 4673 advancing by eight. The same range in the
initial complete `target/wpt-all/results.json` contained 120 failures and
zero passes, all at the unchanged pinned WPT revision. These first-letter
punctuation cases now have focused passing evidence. WPT remained clean at
`fa5393bb9f5f7d41cc16d1aeede1809ccd378ac0`; no fixtures or tolerances changed.
Case 4314 remains unresolved, and the final complete 6,548-case proof is
still outstanding.

The unchanged runtime at `4aa2924` also passed fifteen eight-case batches
4689–4808 (120/120). Receipts use
`target/wpt-targeted/batch-<start>-<end>-first-letter-float-inherit-v1/results.json`
with starts 4689 through 4801 advancing by eight. The initial complete report
contained 120 failures and zero passes in this exact range at the pinned WPT
revision; all now have focused passing evidence. WPT remained clean at
`fa5393bb9f5f7d41cc16d1aeede1809ccd378ac0`. No test entries or allowances were
changed. This does not close case 4314 or establish a complete green run.

At runtime `4aa2924`, eleven eight-case batches 4817–4904 passed 88/88;
the initial complete report had 86 failures and two passes in that range.
Receipts use the `first-letter-float-inherit-v1` suffix with starts 4817
through 4897 advancing by eight. The next batch 4905–4912 passed 7/8,
with case 4911 (`first-line-floats-002.xht`) differing by 812 pixels.
Source and reference geometry matched, but source glyphs were red instead
of green: a floated descendant was incorrectly classified as an in-flow
block, splitting its inline ancestor before ::first-line inheritance.

All three source/lowered/generated block-in-inline checks now exclude
floats. The new real Text-node regression asserts an actual left float and
reproduced red versus expected green before the fix; it now passes. Eight
selected first-line/float/first-letter unit tests passed, not a full unit gate.
Case 4911 now has zero pixel differences in
`target/wpt-targeted/batch-4905-4912-nested-inline-float-first-line-v1/results.json`
(8/8). 4897–4904 and 4265–4272 also passed 8/8 with the same receipt suffix.
Case 4314 and the final complete 6,548-case proof remain outstanding.

Case 4917 (`first-line-pseudo-007.xht`) differed by 1,943 pixels because
a leading empty right float's synthetic auto left margin pushed subsequent
inline text to x=626.26 instead of x=8. Chromium placed the float at x=792,
y=8 with zero extent and retained the text origin at x=8. Fitting leading
right-float prefixes in inline rows now align at the content end in source
order while following inline runs retain the content-start origin. The
first normal inline after a float also preserves its half-leading.

The empty-float regression first failed with text x=197.87 versus expected
zero. It now passes text origin, float-end/top and half-leading assertions;
a second regression passes two nonzero-width right floats in source order.
The selected right-float unit subset is 2/3, not green: the older
`right_float_aligns_to_the_containing_block_end` still fails x=0 versus 50.
Its Block-only input reaches neither new branch; an old-SHA execution
comparison has not been performed, so its baseline status remains unverified.
Case 4917 now has zero pixel differences in
`target/wpt-targeted/batch-4913-4920-leading-right-float-line-v1/results.json`
(8/8). 4905–4912 and 4265–4272 passed 8/8 using the same receipt suffix.
Case 4314 and complete 6,548-case acceptance remain open.

Case 4928 (`first-line-selector-004.xht`) differed by 820 pixels despite
correct green text: applying ::first-line to a lowered block text descendant
incorrectly enabled anonymous inline flow on its parent. The resulting Flex
parent prevented paragraph-margin collapse, moving text from y=51.2 to 67.2.
First-line lowering now enables that inline context only when its remaining
in-flow children are inline-level. The new block-context regression failed
Flex versus expected Block before the change; seven selected first-line
unit tests now pass. Case 4928 has zero pixel differences in
`target/wpt-targeted/batch-4921-4928-first-line-block-context-v1/results.json`
(6/8); 4913–4920 and 4905–4912 passed 8/8 using the same receipt suffix.

4922/4923 remain open at 2,800 pixels each: pixel inspection found only an
extra 140x20 red inline-background rectangle at y=51–70. Their green stripes
already match the reference, but the bottom-aligned inline wrapper's paint
box remains at the tall line's top. Inline paint-box vertical alignment is
the next focused repair, without moving already-correct descendants.
Case 4314 and final complete 6,548-case proof remain outstanding.

Bottom-aligned nonfloating inline containers now locate their own em paint
box at the bottom of the tall content line, while retaining the original
layout origin for descendants and their relative containing block. The new
`bottom_aligned_inline_box_uses_its_em_box_without_shifting_children`
regression failed wrapper y=0 versus expected 80 before the fix; it now
passes the wrapper and all three child-position assertions. Three selected
half-leading/right-float regressions also passed (four tests, not a full gate).
Cases 4922/4923 now have zero pixel differences in
`target/wpt-targeted/batch-4921-4928-inline-bottom-paint-box-v1/results.json`
(8/8). 4913–4920 and 4265–4272 passed 8/8 using the same receipt suffix.
No WPT fixtures, suite entries or tolerances changed. Case 4314 and complete
6,548-case acceptance remain open.

Case 4947 (`selectors/pseudo-007.xht`) remains a failure, not skipped or
adjusted. Its source tests mixed-case `:first-child` with green filler text,
but the linked `universal-selector-002-ref.xht` paints blue 10px borders on
html/div, black filler text and a different instruction. Chromium
141.0.7390.37 at 800x600 confirmed source color rgb(0,128,0)/0px borders
versus reference color rgb(0,0,0)/10px borders. This is a source/reference
content mismatch at the fixed revision; no fixture, membership or allowance
was changed. Native report:
`target/wpt-targeted/batch-4945-4952-inline-bottom-paint-box-v1/results.json`
(7/8, 37,763 differing pixels on 4947). Batches 4937–4944 and 4953–4960
passed 8/8 with that suffix. The following 4961–4968 batch passed 7/8,
with root stacking-context case 4967 awaiting repair at 10,000 pixels.
Case 4314 and full 6,548-case proof remain outstanding as well.

Case 4967 (`stacking-context/root-element-creates-stacking-context.html`)
is now repaired. The numeric root z-order was already the minimum, but
the hierarchical paint key placed the root border after its negative-z
descendant. The root key now uses the same minimum phase sentinel; child
stacking-context prefixes and descendant ordering remain unchanged.
The strengthened `root_sentinel_does_not_raise_negative_descendants_above_normal_flow`
regression failed the root-before-negative assertion before the production
change and passed afterward. Three selected nested/fixed/auto-positioned
stacking regressions also passed (four unit tests, not a full module gate).
The repaired case changed from 10,000 differing pixels to zero in
`target/wpt-targeted/batch-4961-4968-root-background-order-v1/results.json`
(8/8). Batches 4921–4928, 4913–4920 and 4265–4272 passed 8/8 using the
same receipt suffix: 32 focused reftests total. No WPT fixtures, suite
membership or tolerances changed. Cases 4314/4947 and final 6,548-case
acceptance remain open; the next progression batch starts at 4969.

Case 4980 (`syntax/at-charset-012.xht`) is now repaired. XML document byte
decoding ignored declaration encoding when no transport charset was
present, so its imported Shift-JIS stylesheet inherited UTF-8 and missed
the Japanese class selector. Declaration sniffing now reuses the existing
quick-xml parser, following [XML encoding declarations](https://www.w3.org/TR/xml/#charencoding),
while preserving BOM/transport priority and the HTML meta/fallback path.
The new document-to-imported-selector regression failed UTF-8 versus
Shift_JIS before the production change and passed afterward. Two new
priority/HTML/streamed-input tests and three existing encoding tests also
passed: six selected unit tests, not full runtime acceptance.
Case 4980 changed from 410 differing pixels to zero in
`target/wpt-targeted/batch-4977-4984-xml-encoding-fallback-v1/results.json`
(8/8). Batches 4969–4976 and 4961–4968 also passed 8/8 using that receipt
suffix: 24 focused reftests. No WPT fixture, suite membership or tolerance
changed. Cases 4314/4947 and final 6,548-case proof remain open; next batch
starts at 4985.

Subsequent eight-case batches 4985–5056 passed 72/72 with the
`xml-encoding-fallback-v1` receipt suffix. Batch 5057–5064 passed 7/8;
case 5062 (`syntax/case-sensitive-003.xht`) remains open at 1,292 differing
pixels. Read-only actual/reference image comparison localized every
difference to y=157–172, the fifth (`::first-line`) sentence. All sentence
colors are green; the mixed-case pseudo names are recognized. Source
layout represents that sentence as a generated InlineBlock text node,
unlike the reference's ordinary block text. This identifies the next
paint-path investigation, not a completed root-cause fix. No later batch
was started after this failure.

Case 5062 is now repaired. Read-only pixel comparison proved the fifth
sentence was shifted exactly one pixel right: comparing actual x against
reference x-1 reduced all 1,292 differences to zero. The painter compensated
negative ink bearings for its lowered InlineBlock text, unlike ordinary
Block/Inline text. CSS atomic inline text now shares the advance origin;
other display paths retain their existing behavior. The extended
`block_and_inline_text_share_the_same_glyph_origin` regression failed
1,518 channels for InlineBlock before the production fix and passed
Block/InlineBlock/InlineFlex/InlineTable comparisons afterward. Four selected
half-leading/advance/monospace regressions passed as well (five unit tests).
Case 5062 now has zero pixel differences in
`target/wpt-targeted/batch-5057-5064-atomic-inline-origin-v1/results.json`
(8/8). Batches 4977–4984, 4913–4920 and 4265–4272 also passed 8/8 with
that suffix: 32 focused reftests, not full acceptance. No fixture, suite
entry or tolerance changed. Cases 4314/4947 and final 6,548-case proof
remain open; next progression starts at 5065.

Subsequent eight-case batches 5065–5104 passed 40/40 using the
`atomic-inline-origin-v1` suffix. Batch 5105–5112 passed 7/8, stopping
progression on case 5110 (`syntax/declarations-009.xht`) at 1,067 differing
pixels. Its fixture tests malformed at-rules inside declaration blocks;
the next investigation is declaration error recovery. No later batch
was started, and neither fixture nor tolerance was modified.

Superseded trial (see compatibility correction below): case 5110 temporarily
passed. Differences were confined to its third sentence
(y=87–102): declaration recovery reinterpreted `color: red` after a balanced
block inside the malformed `@media` segment. Declaration segments starting
with an at-keyword were discarded through their top-level semicolon;
top-level at-rule parsing and balanced delimiter scanning are unchanged.
The new `malformed_at_rule_declaration_recovers_only_after_its_semicolon`
test failed an extra red declaration before the production change and
passed afterward, including valid declarations after a semicolon. Five
selected existing malformed-block/at-rule/string/bad-url tests passed as
well (six unit tests). Case 5110 changed from 1,067 differing pixels to zero
in `target/wpt-targeted/batch-5105-5112-declaration-at-rule-v1/results.json`
(8/8). Batches 5057–5064 and 4977–4984 passed 8/8 with that suffix:
24 focused reftests, not full acceptance. No WPT fixture, suite entry or
tolerance changed. Cases 4314/4947 and final 6,548-case proof remain open;
next progression starts at 5113.

The next batch 5113–5120 initially passed 7/8, stopping on case 5113
(`syntax/eof-003.xht`) at 410 differing pixels. It is now repaired:
declaration whitespace trimming exposed a trailing string backslash, which
escaped the synthetic EOF closing quote. A pending string escape is now
discarded before closing the quote, as specified by
[string token consumption](https://www.w3.org/TR/css-syntax-3/#consume-string-token).
The new `eof_string_discards_a_trailing_escape_before_closing_the_quote`
regression failed the content spelling before the production fix and passed
afterward, including preservation of paired backslashes. Five relevant
existing EOF/malformed-at-rule/quoted-semicolon/bad-url tests passed as well
(six relevant tests; the broad `eof_` filter also matched three unrelated
compiler tests, which are not counted as CSS coverage).
Case 5113 now has zero pixel differences in
`target/wpt-targeted/batch-5113-5120-eof-string-escape-v1/results.json`
(8/8). Batches 5105–5112 and 5057–5064 also passed 8/8 with that suffix:
24 focused reftests. No WPT fixture, suite entry or tolerance changed;
cases 4314/4947 and final 6,548-case proof remain open. Next batch: 5121.

Compatibility correction: the blanket declaration at-keyword rejection
from `cf249e2` is withdrawn. With that trial rule, batches 5121–5184 passed
64/64 (`eof-string-escape-v1` receipts), but batch 5185–5192 passed 7/8,
failing 5191 (`syntax/malformed-decl-block-001.xht`) at 878 pixels.
That fixture requires recovery after a balanced unknown at-rule block
without a semicolon. Read-only Chromium 141.0.7390.37 at 800x600 showed all
seven of its paragraphs green, while 5110's paragraph `c` is red
rgb(255,0,0) and its other five paragraphs green. Thus the trial's 5110
green result was not modern-browser parity; its old reference requires an
incompatible recovery behavior. The final parser retains balanced-block
recovery and the independent EOF escape fix, without at-rule-name hacks.
The new unknown-block regression failed loss of the first rule under the
trial and passed after withdrawing it; seven relevant parsing regressions
passed. Case 5191 is now zero pixels in
`target/wpt-targeted/batch-5185-5192-balanced-at-rule-recovery-v1/results.json`
(8/8); 5113–5120 also passed 8/8 with that suffix. 5105–5112 passed 7/8,
retaining 5110 at 1,067 pixels (strict runner exit 1, not counted as passed).
Cases 4314, 4947 and 5110 remain visible in the unchanged 6,548-case suite;
no fixture or tolerance was changed. Next progression starts at 5193.

Using the corrected parser, batches 5193–5200 and 5201–5208 passed 8/8.
Batch 5209–5216 passed 7/8, stopping on 5209
(`syntax/unterminated-string-001.xht`) at 610 differing pixels. Receipts use
the `balanced-at-rule-recovery-v1` suffix. The next focused investigation
is unterminated string/newline recovery; no later batch was started.

## Prepare the pinned upstream checkout

Keep WPT outside this repository. The runner rejects a checkout whose `HEAD`
does not match the manifest or whose working tree is dirty.

```bash
git clone --filter=blob:none --no-checkout \
  https://github.com/web-platform-tests/wpt ../wpt
git -C ../wpt sparse-checkout init --cone
git -C ../wpt sparse-checkout set \
  resources infrastructure dom/nodes css/CSS2 \
  css/reference css/support fonts common images fullscreen web-animations
git -C ../wpt fetch origin fa5393bb9f5f7d41cc16d1aeede1809ccd378ac0
git -C ../wpt checkout --detach fa5393bb9f5f7d41cc16d1aeede1809ccd378ac0
```

## Run the gates

The smoke manifest contains one raw `testharness` case and one raw reftest.
Both pass at the recorded baseline, so this command is fail-closed. The same
command runs as a required CI step:

```bash
cargo run --profile wpt -p w3cos-wpt-runner -- \
  --wpt-root ../wpt \
  --suite tests/wpt/w3cos-smoke.json \
  --artifacts target/wpt-smoke
```

The broader ten-case baseline is also fail-closed:

```bash
cargo run --profile wpt -p w3cos-wpt-runner -- \
  --wpt-root ../wpt \
  --suite tests/wpt/w3cos-baseline.json \
  --artifacts target/wpt-baseline
```

The first 2026-08-22 run recorded 2 passing and 3 failing cases. Those failures
were retained as evidence and then closed without expected-result exemptions:

- mixed-ASCII-case HTML attributes now use HTML-namespace ASCII normalization;
- empty `id` presence/equality selectors and Window named access now match;
- block-in-inline collapsible whitespace no longer shifts the opacity group.

The next five raw cases covered namespaced attribute presence/removal, quoted
attribute-value selectors, inherited computed CSS values, and padding around a
block-in-inline split. Their discovery run moved from 8 pass / 2 fail to
9 pass / 1 fail and then 10 pass / 0 fail. The fixes preserve multiple
attributes with the same qualified name but different namespaces, keep quoted
attribute values intact while splitting selector chains, and expose indexed
NodeList entries as own properties.

The recorded baseline is now 10 passing and 0 failing cases. Both new CSS
reftests have zero differing pixels and zero maximum channel difference.
`--report-only` remains available for intentionally red discovery manifests,
but it is not used by either current gate.

`results.json` contains suite, case, subtest, and pixel-difference data.
Reftests additionally emit `actual`, `expected`, and red-highlighted `diff`
PNGs.

## Full fixed-range inventory

The runner can inventory every document below explicit roots and generate a
reproducible suite plus a separate capability-boundary report:

```bash
cargo run --profile wpt -p w3cos-wpt-runner -- \
  --wpt-root ../wpt \
  --suite tests/wpt/w3cos-baseline.json \
  --discover-root dom/nodes \
  --discover-root css/CSS2 \
  --discover-output target/wpt-all/discovered-suite.json \
  --discovery-report target/wpt-all/inventory.json
```

At the pinned revision this scans 11,731 HTML/XHTML/SVG documents and 12 WPT
generated-JS test entries. It produces 6,548 directly runnable cases (370
testharness and 6,178 reftests), records 5,083 non-test/support documents, and
classifies 112 cases at an explicit runner boundary instead of silently
skipping them:

- 43 print-media cases;
- 22 multi-reference reftests;
- 16 `.headers` cases;
- 12 generated JS wrapper cases;
- 6 fuzzy-metadata cases;
- 5 testdriver cases;
- 4 WPT server-handler cases;
- 3 `.sub` substitution cases;
- 1 non-file reference (`about:blank`).

The first complete run used isolated release workers, failure-only PNG
artifacts, and resumable case ranges. All 6,548 runnable cases were executed:
2,368 passed, 3,740 failed assertions/pixel comparisons, and 440 ended as
worker errors. Of the reftests, 2,304 passed, 3,723 had pixel differences, and
151 failed during execution. Of the testharness cases, 64 passed, 17 returned
normal failures, and 289 failed to execute. The complete merged evidence is
`target/wpt-all/results.json`; this discovery result is intentionally red and
is not a required CI gate.

Large suites can use `--jobs`, `--case-start`, `--case-limit`, and
`--failure-artifacts-only`. Completed range reports can be combined with
repeatable `--merge-report` arguments; the merge fails closed on count, order,
revision, or viewport mismatches.

## Execution model

- The local server returns raw upstream files and replaces only
  `/resources/testharnessreport.js` with a result bridge. The upstream
  `testharness.js` is not adapted.
- Each testharness document and each side of a reftest runs in a separate
  process. A page crash or stale Realm state becomes one case error and cannot
  corrupt later results.
- Reftests use the native DOM-to-component, layout, paint-artifact, and Skia
  replay path at the manifest viewport. The bundled Inter face makes the
  offscreen output deterministic instead of depending on a host font.
- Both fuzzy dimensions are enforced: maximum per-channel difference and
  total differing pixels.
- The legacy CSS2 `content-177` overlay reftest receives a path-scoped
  `55 / 5,000` allowance during discovery. Its test paints the same
  antialiased glyph red and then green while the reference paints green once;
  Chrome at the pinned 800x600 corpus revision also differs by 55 / 4,712.
  All other discovered cases remain strict unless upstream fuzzy metadata is
  explicitly represented by the suite manifest.

## Current boundary

The runner supports static HTTP `GET`/`HEAD`, raw HTML testharness cases,
single-reference `match`/`mismatch` reftests, fixed viewport size, explicit WPT
fuzzy allowances, static metadata discovery, isolated parallel workers, and
resumable report merging. It does not yet implement WPT server handlers,
`.sub` expansion, `.headers`, HTTPS origins, testdriver automation,
print/manual tests, generated JS wrappers, fuzzy metadata parsing,
multi-reference graphs, or the full upstream selection system.

ECMAScript language conformance belongs to a separately pinned Test262 runner.
It is the next corpus milestone; WPT results must not be relabeled as Test262
coverage.
