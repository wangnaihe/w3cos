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

Case 5209 is now repaired. Chromium's read-only computed-style check showed
green text and Times, with only `color: green` surviving in its CSSOM.
The compiler kept an unterminated font string open across an unescaped
newline, both swallowing following rules and applying the malformed font.
Block extraction now ends that bad string at LF/CR/FF; declaration splitting
discards the affected segment through its next top-level semicolon.
Escaped newline handling remains unchanged. The new
`bad_string_newline_discards_through_the_next_semicolon` regression failed
one parsed rule versus two before the fix and passed afterward, verifying
both surviving green-only declarations. Seven selected existing parsing
tests also passed (eight relevant unit tests). Case 5209 now has zero pixels
in `target/wpt-targeted/batch-5209-5216-bad-string-newline-v1/results.json`
(8/8); 5185–5192 and 5113–5120 also passed 8/8 with that suffix. No WPT
fixture, membership or tolerance changed. Cases 4314/4947/5110 and final
6,548-case proof remain open. Next progression: 5217.

After the bad-string fix, 5217–5224 passed 8/8; 5225–5232 initially passed
7/8, stopping on 5227 (`syntax/uri-018.xht`) at 2,479 pixels. The URL import
was already successful. Layout dumping exposed text in the default-hidden
head as a visible anonymous Flex row, adding 19.2px before the body.
Anonymous/nowrap lowering now preserves `display:none`, including elements
containing a mixture of hidden elements and text. The strengthened stable
document-root regression failed two visible children versus one before
the fix and passed afterward. A text-only head did not reproduce the bug;
the actual RED required its hidden style child too. Three existing tests,
including author-visible head content, passed (four DOM unit tests).
5227 now has zero pixels in
`target/wpt-targeted/batch-5225-5232-hidden-inline-context-v1/results.json`
(8/8); 5209–5216 and 5185–5192 also passed 8/8 with that suffix. No WPT
fixture, suite entry or tolerance changed. Cases 4314/4947/5110 and final
6,548-case proof remain open. Next progression: 5233.

Following the hidden-display fix, 5233–5240 passed 8/8. Batch 5241–5248
passed 5/8, stopping on the three related dynamic collapsed-row-border
cases 5245/5246/5247 (`tables/border-collapse-dynamic-row-001/002/003.xht`):
2,288/3,660/1,320 differing pixels respectively. Receipts use
`hidden-inline-context-v1`. No later batch was started; these three cases
are the next focused repair scope.

The focused repair now projects a collapsed row border from its left
border edge, matching the equivalent row-group frame. The strengthened
geometry unit reproduced x=20 versus x=0 before the fix and passes now;
nine cell rectangles remain equivalent (1e-4 floating-point geometry
epsilon only, not a pixel tolerance). Four related table units also pass.
5241–5248 now passes 8/8, including zero pixel differences for
5245/5246/5247; 5225–5232 and 5209–5216 pass 8/8 as regressions. Receipts:
`target/wpt-targeted/batch-5241-5248-collapsed-row-frame-v1/results.json`
and the corresponding 5225–5232 / 5209–5216 batch directories. No WPT
fixture, suite entry or pixel tolerance changed. Cases 4314/4947/5110
and the final 6,548-case proof remain open. Next progression: 5249.

5249–5256 passed 7/8, stopping at 5254
(`tables/border-collapse-empty-row.html`, 9,200 differing pixels).
Chromium gives both source and reference table heights 130/140/155/180;
native source initially gave 130 for all four. Empty rows now retain
their used height instead of consuming it as a collapsed-border overlap.
The strengthened unit was RED (second populated row y=20 versus 22),
then GREEN; four related table geometry units also passed. Native table
heights now match Chromium. Strict receipts with suffix
`empty-row-height-v1`: 5241–5248 passes 8/8 and 5249–5256 remains 7/8.
5254 is NOT fixed: its difference increased to 11,920 because the
text-free inline-table baseline correction still aligns table bottoms,
moving the taller tables above the line. Its first row contains an
inline-block and must supply the baseline instead. This is the next
focused repair scope; no later batch was run. WPT inputs and tolerances
remain unchanged, and the final full-suite proof is still outstanding.

The empty-inline-table bottom-baseline fallback now excludes tables
containing cells, even when those cells contain no text. A new focused
unit was RED (second table y=8 became -42 and the parent shrank), then
GREEN; a new no-cell fallback regression and four existing inline-table
units also pass (6/6). Source table positions now all have y=8, matching
Chromium, with correct 130/140/155/180 heights. Strict receipts using
`populated-inline-table-v1`: 5241–5248 passes 8/8; 5249–5256 remains 7/8.
5254's difference decreased from 11,920 to 7,820 but is NOT resolved:
reference table positions remain y=18/16/13/8, unlike Chromium's four
y=8 positions. The next repair must correct the first-row baseline with
unequal block-edge borders, rather than treating cell-free and text-free
tables alike. No later batch, fixture edit or tolerance change was made.

5254 is now resolved with zero differing pixels. Taffy's Block layout
does not export a baseline, so a TableCell containing inline content
previously fell back to its full border-box height. Increasing only the
bottom border from 10 to 20 moved the other table down 10 pixels in a
new RED unit. TableCell now shares the existing inline-formatting-context
path when all its normal-flow children are inline-level, preserving
block-content cells while exporting the content baseline. That unit and
six existing inline-table units pass, as do four related table geometry
units (11/11). Strict receipts with suffix `table-cell-inline-baseline-v1`
pass 5233–5240, 5241–5248 and 5249–5256 (24/24); 5254's 7,820-pixel
difference is zero. No fixture, suite or pixel tolerance changed.
Next progression: 5257. Cases 4314/4947/5110 and the final complete
6,548-case proof remain open.

5257–5264 initially passed 3/8. 5259 (collapsed row/column tracks) had
20,000 differing pixels; 5260–5263 (`border-conflict-element-001a/b/c/d`)
had 6,000/6,000/10,000/16,800. Original full-suite receipts had passed
5260–5262, so those are regressions, not newly discovered baseline failures.
Their cause was the side-color finalizer added in `b28fbeeb`: it selected
the first token of a multi-value border-color for every physical edge,
overwriting the already expanded colors. The new DOM RED expected a green
right edge but obtained red. The finalizer now uses the existing 1–4-color
expansion parser for border-color while preserving last-declaration
selection and relative side-shorthand handling. That unit and three color
regressions pass (4/4). Strict `physical-border-colors-v1` receipts pass
5260–5263 with zero pixels and 5241–5248 / 5249–5256 with 8/8 each.
5257–5264 is now 7/8: 5259 remains at 20,000 pixels, with native height
200 and a projected column reaching width 150 instead of the 100×100
reference square. Progression is stopped there; no later batch, WPT input
or tolerance change. Cases 4314/4947/5110 and full-suite proof remain open.

5259's column projection is partially repaired: used column/background
boxes now remain on their grid tracks; the Skia edge-border path paints
collapsed column borders centered on grid lines, including uniform-width
column borders. Chromium gives column widths 50/0/50 and table height 100.
Native widths were 150/0/50 and now are 50/0/50. The initial diagnostic
unit exposed width 150 versus an assumed 100-pixel paint frame; that
assumption was corrected using Chromium's 50-pixel used column box.
The final strengthened unit checks both 50-pixel grid boxes and the
separate 100-pixel shared border rectangles, leaving cell geometry intact.
It and five related geometry/paint regressions pass (6/6).
Strict receipts with suffix `column-grid-border-paint-v1`: 5241–5248 and
5249–5256 pass 8/8 each; 5257–5264 stays 7/8. 5259's differing pixels
decreased from 20,000 to 10,000, but its native table height is still 200.
It is NOT resolved. The next scope is row/shared-border layout and paint
projection, without scaling the whole table or modifying WPT/tolerances.
No later batch was run; full-suite proof remains outstanding.

5259 now passes with zero differing pixels. Row/row-group borders join
the shared cell-grid conflict set in the layout clone; their own layout
border is cleared only after cells can carry it. Paint transfers winning
part edges to cells (cell > row > row-group on equal widths), excluding
nested tables, then disables the duplicate part border. Part backgrounds
stay on grid boxes and collapsed rows no longer distort those unions.
The new RED found the first visible row height 100 versus the expected
shared-half track 50; it is now GREEN, including regular/cached-layout
parity. Styled-part projection in the cached path uses normalized widths
too. A new paint-priority/nested-table test and seven related regressions
pass (9/9). Strict `shared-part-grid-v1` receipts pass 5233–5240,
5241–5248, 5249–5256 and 5257–5264 (32/32). Native table dimensions are
100×100; visible row heights are approximately 50 each. This closes the
reftest, not general CSSOM conformance: the dump still anchors the
zero-height collapsed row at the table top and gives the zero-width
collapsed column a nonzero height, unlike Chromium. No WPT input or
tolerance changed. Next progression: 5265; cases 4314/4947/5110 and the
final complete 6,548-case proof remain open.

### Hidden-only children do not create a line box (2026-09-13)

Case 5266, `border-spacing-applies-to-016.xht`, regressed from its
original PASS to 14,896 differing pixels: CSS-styled hidden descendants
and folded whitespace promoted the empty red parent into a Flex line
box. Anonymous-line promotion now requires a visible, nonempty-text
candidate. The minimal CSS-rule reproduction is RED without the guard
and GREEN with it. Strict `hidden-line-v1` passes 5217–5224 and
5225–5232 (16/16); 5265–5272 is 6/8, with case 5266 at zero pixels.
Cases 5268 (2,944 pixels) and 5271 (506 pixels) remain unchanged, so
progression stays stopped at this batch. The `hidden_` unit selection
is 3/5 after the guard versus 2/5 without it: the existing anonymous
table whitespace expectation (`a bc d` versus `abcd`) and counter-test
index-out-of-bounds failure occur identically in both runs. These are
not reported as green. No fixture, suite or tolerance was changed;
the fixed 6,548-case final proof and earlier corpus questions remain open.

### List-item text uses the same glyph origin (2026-09-13)

Case 5271, `caption-side-applies-to-003.xht`, is now zero differing
pixels (previously 506). Its lowered ListItem text leaf still compensated
ink bearings, unlike the reference's retained Inline text. ListItem now
uses the same advance origin as Block and Inline; no list indentation,
marker or line-height rule changed. Extending the existing pixel test
produced RED (1,518 differing channel values) and then GREEN. Three
alignment/half-leading regressions also pass (4/4 selected unit tests).
Strict `list-origin-v1` receipts pass 5049–5056 and 5057–5064 (16/16).
The current 5265–5272 batch is 7/8: case 5268 remains at 2,944 pixels,
so no later batch was started. Chromium loads both cat images and gives
the source/reference identical image y positions (119 and 310), while
native dumps differ by 1.92px for the first image; this is the next
cell-line-box alignment investigation, not a completed fix. No WPT
input, suite or tolerance was altered; final full-suite proof remains open.

### Caption/cell baseline line boxes and glyph origins (2026-09-13)

Case 5268, `caption-position-001.xht`, is now zero differing pixels
(previously 2,944). Cell vertical alignment used image bounds without
the baseline line's font descent, shifting the first reference image
1.92px. Caption/Cell text leaves also compensated ink bearings. The
first change removed those differences but left 1,270 pixels on the
second image: fixed-layout cells use border-box sizing, so their line
minimum must include used padding/border, and image captions must
establish the same inline line box. All three paths now agree.
Chromium had independently loaded both images and matched their source/
reference y coordinates; WPT fixtures and tolerance remain unchanged.
The new baseline/middle image unit, expanded glyph-origin pixel unit
and three related table regressions pass (5/5 selected units). Strict
`caption-strut-v2` receipts pass 5233–5240, 5249–5256, 5257–5264 and
5265–5272 (32/32); the intermediate `cell-line-box-v1` receipt remains
7/8 and is not claimed green. Suite revision remains
`fa5393bb9f5f7d41cc16d1aeede1809ccd378ac0`, count 6,548, viewport
800×600. Next progression is 5273; earlier corpus questions and final
complete-suite proof remain open.

### Sequential progression after caption repair (2026-09-13)

On `26fb6c7`, strict `caption-strut-v2` progression passes 5273–5280,
5281–5288 and 5289–5296 (24/24). Batch 5297–5304 is 7/8: case 5297,
`column-visibility-004.xht`, differs by 10,000 pixels (the initial full
report recorded FAIL at 174 pixels, so its current mismatch is worse).
The case requires clipping spanned-cell content intersecting a collapsed
column. The sequential
command exits with failure here, and no 5305 batch receipt exists.
This is the next focused repair, not a final-suite pass. Fixtures,
revision, denominator and zero tolerance are unchanged.

### Collapsed-column spans retain their visible grid width (2026-09-13)

Case 5297, `column-visibility-004.xht`, is now zero differing pixels
(RED receipt: 10,000). Final cell projection wrote signed collapsed
track markers as negative widths and advanced every cell by one column,
discarding colspan. Projection now measures only visible tracks,
advances by the full span, and includes spacing only for visible tracks.
The spanning cell is 100px wide at x=112, with its following cell at
x=214; the removed track's original width remains available to the
existing content-offset/clip path. A new geometry unit and five related
collapsed-column/shared-border units pass (6/6). Strict `visible-span-v1`
receipts pass 5249–5256, 5257–5264, 5265–5272 and 5297–5304 (32/32).
This proves the reftest, not complete CSSOM conformance: the later column
background projector still derives column bounds by cell ordinal, so
the native dump gives the collapsed column width 100 and the next column
width 202. That uncovered background/column geometry remains open, not
silently reported fixed. No WPT input, tolerance or suite changed.
Next progression is 5305; final complete-suite proof remains open.

### Sequential progression to fixed-layout failures (2026-09-13)

On `ba53a3e`, strict `visible-span-v1` progression passes every 8-case
batch from 5305–5312 through 5361–5368 (eight batches, 64/64).
Batch 5369–5376 is 6/8: `fixed-table-layout-027.xht` (5370) differs by
1,200 pixels and `fixed-table-layout-029.xht` (5372) by 800 pixels.
Both were PASS in the initial full report, so regression diagnosis takes
priority; the receipt alone does not identify the introducing commit.
The sequential command exits with failure; no later batch was started.
These two tests are the next focused repair, not a full-suite verdict.
Pinned revision, suite entries, viewport and zero tolerances are unchanged.

### Implicit column-group parser repair (2026-09-13)

The HTML parser now exits an implicit `colgroup` before a row or cell
token, allowing the existing implicit `tbody` insertion to put rows
under the table rather than inside the column group. Fragment roots
are protected from the added stack pop. The new parser regression and
the existing template/table-wrapper regression both pass (2/2).
This isolated parser commit does not claim pixel closure: concurrent
uncommitted cell-grid changes pass 5369–5376 but still fail 5257, 5263
and 5264 in strict `cell-inline-grid-v2` receipts. Those layout/paint
changes remain outside this commit; sequential progression is stopped.
No upstream WPT input, suite entry or tolerance changed.

### Canonical collapsed-cell inline grid (2026-09-13)

Cells now retain shared-grid inline rectangles instead of adding their
painted border halves to the used width again. Inline border painting,
text content insets and retained visual bounds account for the centered
halves separately. Auto tables and the single-column path use the same
inline convention as fixed tables; rows use the table-wide boundary
winner, without subtracting the table border already zeroed for Taffy.
The old extra authored-height border addition is also removed.

The percentage-cell regression was reproduced before repair. An added
table-border origin test also failed at x=0 versus Chromium's x=10
before its fix. Old paint-expanded geometry expectations were replaced
only after independent Chromium rectangle checks (including the two
unequal-border rows); the related `collapsed_` subset passes 29/29.
The draft's four `cell-inline-grid-v1` regressions reduced to three in
v2, then cleared in v3. Strict v3 receipts pass 5257–5264, 5369–5376,
5241–5248, 5265–5272, 5249–5256 and 5297–5304 (48/48).
The runtime library also passes `cargo check --profile wpt` with
`dynamic-js,skia,cpu-render,gpu`; non-Skia pixel acceptance is not claimed.

Remaining geometry is explicit: on 5264 the first cell is correctly
x=58/w=40, but the row/background projector still applies the old
inline half-insets, giving x=68/w=140 instead of Chromium's x=58/w=160.
Its span/column-ordinal issue noted above also remains open. Vertical
CSSOM rectangles have not been canonicalized by this inline repair.
No WPT input, suite, viewport or zero tolerance changed. The next focused
repair is the dependent row/column inline projection, before later
sequential progression; final 6548-case zero-failure proof remains open.

### Canonical inline table-part backgrounds (2026-09-13)

The dependent background projector no longer subtracts inline border
halves from canonical cell rectangles. An extended table-origin unit
first failed with row x=20 rather than Chromium's x=10; after repair
the row has x=10/w=160. A focused column/column-group/row-group unit
also checks unchanged shared inline bounds. Related `collapsed_` units
pass 30/30. Strict `canonical-part-inline-v1` receipts repeat all six
v3 batches above with 48/48 passes. The 5264 native dump now gives
the row x=58/w=160 and first cell x=58/w=40, closing the double-inset
issue recorded above. Vertical half-inset projection remains unchanged;
the separate colspan/column-ordinal issue remains open. No upstream
input, suite, viewport or zero tolerance changed. Next sequential
progression resumes at 5377; final full-suite proof remains open.

### Sequential progression to inline-priority failure (2026-09-13)

On `6a9d793`, strict `canonical-part-inline-v1` progression passes
5377–5384, 5385–5392 and 5393–5400 (24/24). Batch 5401–5408 is 7/8:
`table-anonymous-objects-017.xht` (5408) differs by 498 pixels. It already
failed by 1,040 pixels in the initial report, so this is an unresolved
initial failure, not evidence of a newly introduced regression.
No batch after 5408 was started.

The source has `span { display: table-cell ! important }` over inline
`display:block`. Chromium computes both spans as table cells. The native
dump instead contains two block text nodes, leaving the second red line
uncovered. The stylesheet importance helper accepts the spaced marker,
but computed-style merging unconditionally reapplies inline declarations
after all matched rules, losing author-important precedence at that
boundary. This is the next focused cascade repair; anonymous-table
layout has not yet been independently ruled out as an additional issue.
Pinned upstream inputs and zero tolerances remain unchanged.

### Unified author importance across inline merging (2026-09-13)

Node matching now retains typed importance metadata while the legacy
three-field matching API remains compatible. Computed styles use one
ordered author stream: normal rules, normal inline, important rules,
important inline. Custom properties, inheritance/relative-value winner
queries and border shorthand finalization consume that same stream,
rather than reapplying inline values after the important declarations.
Priority markers are removed for value parsing but retained in the raw
declaration records. Ordinary inline/custom-property whitespace is kept.

Two focused priority tests first failed (block instead of table-cell;
40px instead of inherited 24px). The repaired cache/inheritance module
passes 32/32, plus three anonymous-table shaping units and the existing
stylesheet importance unit (36 distinct targeted passes). Strict
`author-important-restored-v1` receipts pass 5401–5408, 5393–5400,
1122–1129, 5257–5264 and 5369–5376 (40/40). 5408 is now pixel-identical.

The independently executed 1130–1137 batch remains 7/8: 1132 requires
the user stylesheet prescribed by its `userstyle` flag. A temporarily
restored, clean pre-repair `8449f00` build reproduces exactly the same
14,958-pixel failure (`author-important-clean-baseline-v1`), proving no
new regression in that batch. Chromium without that user stylesheet
also leaves the instructions visible, the last line black and `b` bold.
All three repair files were restored exactly before the latest build
and 40-case regression. No fixture, suite entry or tolerance changed.
User-origin stylesheet/profile support is the next prerequisite repair,
not silently skipped or counted green. Full 6548-case closure is open.

### User stylesheet origin matching foundation (2026-09-13)

Rules now carry an explicit author/user origin. Host code can register
user rules without disguising them as author rules or changing selector
specificity. Context, live-node and pseudo-element matching use four
stable buckets ordered by origin and importance, preserving specificity
and declaration order within each bucket. The shared property lookup
also uses that origin rank. Computed merging places normal user rules
below author rules and important user rules above important inline values,
following [CSS2.1 cascading order](https://www.w3.org/TR/CSS2/cascade.html#cascading-order).

Both origin-precedence units first failed red-versus-green, then passed.
A cross-path unit checks both precedence directions in context, node and
pseudo matching. Cache/inheritance units pass 34/34 and stylesheet units
39/39 (73 distinct passes). Strict `user-origin-foundation-v1` receipts
repeat the five restored-author batches above with 40/40 passes.

This is a prerequisite foundation, not 1132 acceptance. The runner has
not yet supplied a user stylesheet, and normal user rules still need to
be placed below HTML presentational hints in computed merging. These
are the next focused changes before testing 1132 with its prescribed
profile. No WPT input, suite entry or tolerance changed; sequential
progression and final 6548-case proof remain open.

### Presentational hint origin boundary (2026-09-13)

Normal user rules now precede HTML body text/background and direction
hints; author declarations follow those hints and important user rules
remain last. HTML font color hints join the same ordered declaration
stream rather than being treated as UA defaults. The inheritance winner
lookup uses that stream too. This does not implement every legacy font
attribute or legacy HTML color parsing rule.

Body and font color precedence tests first failed, then passed; the body
test also checks that important user color still wins. Cache/inheritance
tests pass 36/36 and stylesheet tests 39/39 (75 distinct passes).
The runner builds in 1m50s; strict `presentational-origin-v1` receipts
for starts 5401, 5393, 1122, 5257 and 5369 pass 40/40.
The prescribed user stylesheet runner profile for 1132 is still open;
these unit results are not its pixel acceptance or final-suite proof.

### Explicit CSS2 userstyle profile and font-weight keywords (2026-09-13)

`--user-stylesheet tests/wpt/profiles/css2-userstyle.css` now forwards the
same explicit user-origin profile to isolated actual/reference workers.
Registration runs after navigation reset and before parser/script polling.
The artifact directory retains an exact copy as `user-stylesheet.css`.
Profiles currently require unconditional, self-contained rules; media,
imports, font-face metadata and parser warnings fail explicitly. This is
not complete dynamic user-stylesheet support. The profile contains the
upstream 1132 instructions' selectors, without changing upstream inputs.

The first configured 1130 batch passed 7/8: 1132 decreased from 14958 to
1592 differing pixels, exposing ignored `font-weight: normal` on `<b>`.
A longhand unit reproduced numeric 700 surviving `normal`; `normal` and
`bold` now map to 400 and 700 and that unit passes. Cache/inheritance
36/36 and stylesheet 39/39 pass. CSSStyle tests are 39 passed, 1 failed:
`negative_margin_and_character_relative_lengths_remain_valid` expects
`Em(4)` but gets `Ch(4)` on an unchanged length path. That failure remains
open; the module is not claimed green.

The rebuilt runner completes in 1m51s. Strict `userstyle-profile-v2`
receipts for starts 1130, 5401, 5393, 1122, 5257 and 5369 pass 48/48.
1132 has max difference 0 and differing pixels 0 with both allowances 0;
its retained profile matches the input byte-for-byte. Sequential next
start remains 5409 and final 6548-case zero-failure proof is still open.

### Sequential resume through anonymous-table split failures (2026-09-13)

Strict `userstyle-profile-v2` starts 5409, 5417 and 5425 pass 24/24.
Start 5433 completes all eight with four passes and four failures:
`table-anonymous-objects-081`/082/083/084 have 9743/12804/8982/12043
differing pixels against `no_red_3x3_monospace_multi_table-ref.xht`.
All four are also failures in the original report (8500/12452/11191/12452);
they are not newly added tests. Progression stops here without starting
5441. Sources 081/082 compare three independently sized anonymous table
rows separated by block spans against three explicit tables, swapping
which overlay is absolutely positioned. The next scope is anonymous
table splitting and absolute overlay geometry, not further suite runs.
No WPT input, suite, viewport or tolerance changed.

### Anonymous block-table text flow candidate (2026-09-13, local)

The 081 layout dump exposed block anonymous tables lowered to inline text,
with nowrap forcibly normalized to normal. A new invariant unit first
failed Inline-versus-Block, then passed with block flow and nowrap retained.
The lowering no longer inserts U+2028 separators to emulate block edges;
the existing sibling-boundary unit now checks two actual text fragments
with the lower one block-level rather than requiring that encoding.
Chromium confirms the authored block spans retain nowrap and separate
vertical rows (its font metrics are not substituted for native pixels).

The rebuilt runner takes 1m50s. Strict `anonymous-block-flow-v1` starts
5433, 5401 and 5425 pass 24/24, including all four previous failures at
zero pixel difference and zero allowances. Anonymous-filter unit results
are 22 passed, 1 failed: hidden-script table text now collects `a bc d`
where the existing unit expects `abcd`. This whitespace discrepancy is
not yet resolved; the candidate remains local and is not declared fully
validated. No 5441 batch or final full run has started.

### Preformatted row whitespace prerequisite closed (2026-09-13)

The remaining hidden-script unit did not actually create its intended
TableRow/Pre case: the Rust primitive attribute setter stores `style`
without invoking the runtime's style parser. Both affected units now use
the typed style entry and assert computed TableRow/Pre before proceeding.
This does not alter the browser JS attribute/parser path or WPT inputs.

Chromium DOM-created fixtures establish that isolated leading row space
is absent, preserved trailing bare-text space survives hidden scripts,
and spaces within one significant `" bc "` text node remain. The row
fixup excludes display:none boxes and retains preformatted trailing space
only in an existing anonymous text run. The inline-element fixture checks
`abcd`; the bare-text/hidden-script fixture checks `abc d`, rather than
incorrectly dropping the legal trailing space. These are corrections to
invalid Rust test prerequisites, not changes to the upstream reference.
See [CSS2 anonymous table objects](https://www.w3.org/TR/CSS2/tables.html#anonymous-boxes)
for irrelevant-box and missing-wrapper processing.

With the corrected prerequisites, temporarily restoring the old row
branch reproduces both failures (21 passed, 2 failed). Restoring the
fix passes 23/23; an additional significant-text edge-space invariant
passes too, yielding anonymous units 24/24. Cache/inheritance 36/36 and
stylesheet 39/39 also pass (99 distinct scoped passes, not the full DOM
suite). The earlier CSSStyle ch-versus-em assertion remains separately
open; final 6548-case proof is not yet available.

The rebuilt runner takes 1m50s. Strict `anonymous-row-whitespace-v2`
receipts for starts 5433, 5401, 5425, 5393, 5369 and 1130 pass 48/48.
081–084 remain at zero pixel difference with zero allowances. This closes
the candidate's hidden-script prerequisite; next sequential start is 5441.

### Sequential continuation after anonymous row closure (2026-09-13)

On `1415725`, strict `anonymous-row-whitespace-v2` receipts for starts
5441, 5449, 5457 and 5465 pass 32/32 with the same retained CSS2 userstyle
profile. All reftests have zero pixel allowances. The next sequential
start is 5473; these are incremental receipts, not final same-SHA
6548-case proof. No upstream inputs or tolerance changed.

### Wrappable anonymous cell identity repair (2026-09-13)

On `4f6c7d2`, strict `anonymous-row-whitespace-v2` starts 5473, 5481 and
5489 pass 24/24. Start 5497 completes all eight with six passes and two
failures: anonymous objects 155/156 (indices 5500/5501), each differing
by 785 pixels. No 5505 batch starts. Both fixtures replace an inter-cell
whitespace text node with a middle row on load; the old lowering flattens
the last anonymous row and wraps its final `Col 3` below the overlay.
The original report classifies both as XML-script parse errors, not new
tests introduced in this run.

A unit first fails because two `normal` cells containing `a b` and `c d`
lose their separate contents. Text with internal soft-wrap whitespace
now bypasses both anonymous-cell concatenation and plain-table text
lowering, retaining the table grid instead of forcing nowrap or altering
font defaults. Anonymous units pass 25/25; cache/inheritance 36/36 and
stylesheet 39/39 pass (100 distinct scoped units).

The rebuilt runner takes 1m50s. Strict `wrappable-table-grid-v1` receipts
for starts 5497, 5489, 5433, 5401, 5369 and 1130 pass 48/48. Both 155/156
have max difference 0 and differing pixels 0 with both allowances 0.
No WPT inputs, font defaults, suite, viewport or tolerances changed.
Next sequential start is 5505; final 6548-case same-SHA proof remains open.

### Sequential continuation to generated-after whitespace failures (2026-09-13)

On `bb3d9af`, strict `wrappable-table-grid-v1` starts 5505, 5513 and
5521 pass 24/24. Start 5529 completes all eight with six passes and two
failures: anonymous objects 187/188 differ by 409/524 pixels against
`no_red_antialiasing_a_bc_d-ref.xht`. Both are also failures in the original
report (2375/2325 pixels); they are not newly introduced tests. Sources
place a hidden script between table-cell `b` and `c`, with a generated
`::after` supplying `d`, and expect the visible run `a bc d`. Next scope
is hidden-box table grouping and generated-after whitespace. No 5537
batch or final full run starts, and no input or tolerance changes.

### Hidden-box anonymous table consecutiveness (2026-09-13)

The 187 layout dump contains `a b  c d` rather than reference `a bc d`:
display:none script placeholders break the sibling table run and preserve
the inter-cell whitespace on both sides. Default-parent table fixup now
excludes non-generating hidden boxes before grouping and compares the
nearest substantive siblings across a whole whitespace run. Two linear
scans compute those neighbors; no quadratic per-node search is added.
Whitespace at the table-to-generated-`d` edge remains present.

The new unit first fails with seven fragments where three are expected;
after the repair it verifies one InlineTable containing `bc`, a preserved
edge space and `d`, with no hidden script text. It passes 1/1; anonymous
units 25/25, cache/inheritance 36/36 and stylesheet 39/39 also pass
(101 distinct focused units, not the full DOM suite).

The rebuilt runner takes 1m50s. Strict `hidden-table-consecutiveness-v1`
receipts for starts 5529, 5521, 5497, 5433, 5401, 5369 and 1130 pass 56/56.
187/188 have max difference 0 and differing pixels 0 with both allowances
0. No WPT input, font default, suite, viewport or tolerance changed.
Next sequential start is 5537; final 6548-case proof remains open.

### Sequential stop at CSS2 preformatted improper-row space (2026-09-13)

On `762725e`, strict `hidden-table-consecutiveness-v1` start 5537 completes
all eight with seven passes and one failure: anonymous objects 199 differs
by 637 pixels against `no_red_antialiasing_a_bc_d-ref.xht` (original report
also fails, 1040 pixels). No 5545 batch starts. Its TableRow/Pre contains
proper cell `a`, an improper inline span `bc` with isolated spaces around
it, and proper cell `d`; the fixed WPT reference requires `a bc d`.

Chromium loading the actual file as application/xhtml+xml instead renders
`a\tbcd` in innerText and assigns no client rect to either space. This
contradicts the reference, not evidence that the reference was changed.
The earlier browser-calibrated pre-row whitespace decision must be
rechecked against CSS2 irrelevant-box classification: the middle span is
not an internal table box. Preserve the fixed input/reference and inspect
that distinction before changing the engine or test expectations. Final
6548-case zero-failure proof remains open.

### CSS2 improper row content keeps preformatted surrounding space (2026-09-13)

CSS2 §17.2.1's irrelevant-whitespace conditions distinguish internal table
boxes from ordinary inline content. In 199, the improper inline `bc` is
not a table box, so its surrounding preformatted space remains in the
generated cell. This supersedes the earlier Chromium-calibrated decision
to trim these spaces; Chromium's differing output is retained as a
qualification note, not used to change the fixed WPT reference.

Two corrected TableRow/Pre units first fail `abcd`/`abc d` versus `a bc d`
(anonymous filter: 23 passed, 2 failed). Separator filtering is now shared
by default-parent grouping and row fixup, with row filtering accepting
proper TableCell neighbors. Remaining preformatted improper-child space
is preserved. The edge-space unit also verifies that a whitespace-only
separator between two proper cells does not generate an extra cell.
The linear scan behavior and hidden-box exclusion from 187/188 remain.

Anonymous units pass 25/25, hidden grouping 1/1, cache/inheritance 36/36
and stylesheet 39/39 (101 distinct focused passes).

The rebuilt runner takes 1m50s. Strict `pre-improper-space-v1` receipts
for starts 5537, 5529, 5497, 5433, 5401, 5369 and 1130 pass 56/56.
199 has max difference 0 and differing pixels 0 with both allowances 0.
No fixed WPT input, font default, suite, viewport or tolerance changed.
Next sequential start is 5545; final 6548-case proof remains open.

### Sequential stop at replaced-cell preformatted content (2026-09-13)

On `b438ac2`, strict `pre-improper-space-v1` start 5545 completes all
eight with seven passes and one failure: anonymous objects 211 differs
by 450 pixels against its dedicated 211 reference (original report also
fails, 82400 pixels). No 5553 batch starts. The source requires images
authored as table-cell to participate as inline replaced content inside
one generated cell, including leading space, tabs and trailing spaces;
the reference uses an explicit cell around that same content. Next scope
is the anonymous cell inline formatting context and replaced-item spacing.
No WPT input or tolerance changes, and final 6548-case proof remains open.

### Replaced row content: remove phantom cell identity

- Added `anonymous_replaced_cell_run_has_no_phantom_columns` through real DOM
  images with authored `display: table-cell`. RED: five cells instead of three.
- Removed both empty-cell insertions and their private custom-property marker;
  replaced images retain inline-level used display and one consecutive run.
- Focused DOM regression: anonymous 26/26, computed-style cache 36/36,
  stylesheet 39/39. `git diff --check` passed.
- This is an intermediate structural repair, not pixel closure of case 211.
  Its previous 450-pixel receipt predates this change; row-edge anonymous
  whitespace still needs qualification and a rebuilt strict runner receipt.

### Tabular boundary whitespace and replaced cells: focused pixel closure

- RED `anonymous_table_edge_whitespace_does_not_create_cells`: three cells
  instead of one. CSS2 17.2.1 stage 1.3 permits absent adjacent siblings in
  tabular containers, unlike stage 1.4 outside those containers.
- Applied the linear separator scan to rows, tables and row groups with
  container-specific proper-descendant predicates. Preserve whitespace next
  to improper inline content. Whitespace generated by a pseudo-element keeps
  its principal inline box, rather than masquerading as anonymous text.
- DOM anonymous 27/27 (including all six tabular container kinds and generated
  whitespace), computed-style cache 36/36, stylesheet 39/39; diff check passed.
- Runner build completed in 1m49s. Strict batch starts
  `5545,5537,5529,5497,5433,5401,5369,1130`: 64/64 passed. Receipts are
  `target/wpt-targeted/batch-<start>-tabular-edge-whitespace-v1/results.json`.
  Case 211 now has both `different_pixels=0` and `max_difference=0`.
- WPT revision remains `fa5393bb9f5f7d41cc16d1aeede1809ccd378ac0`, original
  6548-case manifest, 800x600 and zero tolerances unchanged. Runner SHA256:
  `9878bf72edadafe20ce7bc4b87b066ba3df2d8f686447bdaded576b7d842a49c`.
- Evidence was generated from the local repair atop `f8ede08`; this is focused
  closure, not a final clean-SHA 6548-case run. Next sequential start is 5553.

### Sequential 5553 and forced-break whitespace: case 212 closure

- On `8d7c873`, strict batch 5553 completed 8 cases: 6 passed, 2 failed.
  Receipt: `target/wpt-targeted/batch-5553-tabular-edge-whitespace-v1/results.json`.
  Case 212 differed by 347 pixels; `table-backgrounds-bc-cell-001.xht`
  (index 5557) differed by 2166. Later starts 5561 through 5585 were not run.
- Headless source/reference dumps for 212 show the reference coalescing
  `above` + BR + indentation into `above\u{2028} below`. The extra space
  indents its second line by 4px; the source block-table flow is correct.
- Strengthened the existing forced-break DOM test with whitespace on both
  sides of BR (RED), then treated an in-flow BR as a line edge in collapsed
  text sibling analysis (GREEN). Preformatted modes retain their early return.
  Normative basis: [CSS2 whitespace processing](https://www.w3.org/TR/CSS2/text.html#white-space-model).
- Anonymous DOM 27/27, cache 36/36, stylesheet 39/39 and diff check passed.
  Runner rebuilt in 1m50s. Case 5553 strictly passed, followed by 64/64 at
  starts `5545,5537,5529,5497,5433,5401,5369,1130`. Receipts use suffix
  `br-edge-whitespace-v1`, with the single case under `case-5553-...`.
  Runner SHA256 `66fb2576c5a57edf5a225d2f6c8223db8a5f754b958123c6b323ea8d0caacba2`.
- Evidence used the local repair atop `8d7c873`; no final full-suite claim.
  Next failure is index 5557: inspect shared inline border allocation versus
  `box_background_paint_rect` insets before changing collapsed-cell painting.
  Fixed WPT inputs, original suite, viewport and zero tolerances unchanged.

### Centered cell background repair: pending table paint-phase integration

- Local work atop `4da0454`, not committed: corrected collapsed-cell background
  bounds to retain the inline grid rectangle and background-image origin to
  use half inline borders. The old helper test incorrectly supplied a full
  59px border rectangle rather than the observed 57px shared-grid rectangle.
  Its corrected RED result was x=139/width=55, expected x=138/width=57.
- Both focused background tests pass, including asymmetric and separated
  borders. Current unit executable is `w3cos_runtime-11f7b072e926e629`:
  background-image 16/16, collapsed-layout 16/16, paint-artifact 30/32.
  The two paint failures are `auto_positioned_subtree_paints_after_later_normal_flow_content`
  and `inline_fragment_clip_keeps_layout_rect_and_clips_only_paint`; neither
  uses the modified collapsed-cell bounds path. Do not claim module-wide green.
  An earlier direct invocation selected an older feature-specific executable;
  its counts are not current-change qualification.
- Strict starts `5553,5545,5231,5239,5247,5255,5263,5369`, suffix
  `centered-cell-background-v1`: 62 passed, 2 failed. Case 5557 now has zero
  pixel difference, but fixed-table-layout-027 (5370) regressed by 1200 pixels
  and -029 (5372) by 800. All receipts remain retained; do not advance to 5561.
- The regression is NOT a fixed-versus-auto rectangle discrepancy. Layout
  dump 5370 shows shared-grid cell widths 12.5, 75, 12.5. The right red cell
  background paints after the middle green cell's winning shared border,
  covering its inner half. Previous unconditional insets masked that ordering.
  CPU and Skia currently paint background and border together per node.
- Required next scope: common retained table paint phases, consumed by all
  raster backends, with table cell backgrounds preceding table borders as
  required by [CSS2 Appendix E](https://www.w3.org/TR/CSS2/zindex.html#painting-order).
  Preserve border-conflict ownership, clips, effects and table content order;
  do not add a fixture, layout-mode or opaque-border clipping special case.
- Runner build completed in 4m00s including the queued build-lock wait.
  SHA256 `c6b8e3cc6d634c7b1b0f61eb578aea83a4a99f803d444eef61fbc3ab7c9f10e3`.
  All jobs are terminal. Fixed upstream revision, original 6548-case suite,
  800x600 viewport and zero tolerances unchanged. Full closure remains open.

### Shared table replay phases: focused closure of 5557 and fixed-layout regressions

- Added `table_paint` replay shared by window and headless entry points. It
  emits table/column-group/column/row-group/row/cell backgrounds, then shared
  borders, then original content; snapshot identity, coordinates and border
  conflict ownership are retained. Non-table nodes borrow their original data.
  Positioned, floated and independent-effect parts retain atomic replay.
- Background/border-only commands suppress outline; the content pass retains
  it. `filter:none` does not introduce an independent context. This closes the
  neighbor-background overpaint exposed by the centered bounds repair, rather
  than restoring an unconditional inset or introducing a layout-mode exception.
- New stage-order/positioned-cell tests 2/2, background-image 16/16,
  collapsed-layout 16/16; current paint-artifact scope remains 30/32 with the
  same two separately recorded failures. New module formatting and diff check
  passed. This is not a claim that all raster unit tests or native UI journeys pass.
- Initial draft strict batches 5369 and 5553 passed 16/16, retained under
  `table-phases-draft-v1`. After outline/filter refinement, rebuilt runner
  completed in 3m58s including queued lock wait; SHA256:
  `8285ed7903ecb976837239486331efe5503c551469422e890e281843ddd5d16f`.
- Latest strict starts `5369,5553,5545,5231,5239,5247,5255,5263`: 64/64
  passed under `target/wpt-targeted/batch-<start>-table-paint-phases-v1/results.json`.
  5370/5372 no longer regress, 5557 remains zero pixels, and 211/212 pass.
  Evidence used the local repair atop `4da0454`, not a final clean-SHA full run.
- Fixed upstream revision, original 6548-case suite, viewport 800x600 and
  zero tolerances are unchanged. Next sequential start 5561; final full-suite
  closure remains open. Actual CPU/GPU/mobile replay acceptance is not implied
  by compilation of the shared window entry point and headless Skia evidence.

### Sequential 5561 checkpoint after shared replay integration

- On `aa94977`, batch 5561 (`table-paint-phases-v1`) completed 8 cases:
  4 passed, 4 failed. Starts 5569 through 5593 were not executed.
- Failures: 5562 `table-backgrounds-bc-table-001.xht` 34629 pixels;
  5566 `table-backgrounds-bs-row-001.xht` 504; 5567
  `table-backgrounds-bs-rowgroup-001.xht` 6966; 5568
  `table-backgrounds-bs-table-001.xht` 48823. Zero tolerances retained.
- All four paths passed in the original `vendor/w3cos/target/wpt-all/results.json`
  snapshot. The exact introducing version is not yet qualified; do not label
  these as ordinary original failures or assume this replay commit introduced
  them. Separated-border nodes are borrowed unchanged by the new replay.
- Source/reference dumps for 5562 are retained under
  `target/wpt-targeted/table-background-5562-{actual,reference}-debug.bin`.
  Source tables are 291x115; reference blocks are 291x103, with identical
  x/y positions (19/15, 19/120, 19/225). The root used-height/background bounds
  disagree by 12px even though successive table placement agrees.
- Next scope: qualify root grid height versus cell/row border allocation and
  background positioning. Repair authoritative layout/paint bounds rather than
  applying a 12px clipping constant. Keep the already passing cell-background,
  fixed-layout and dynamic-border batches in focused regressions. Full 6548-case
  proof remains unachieved.

### Collapsed table root bounds repair

- Auto-height projection had reintroduced table padding and a full bottom
  border into the collapsed grid. It now ignores padding and adds only the
  remaining border half; separated borders retain their original calculation.
- Border conflict resolution retains resolved outer widths with transparent
  table ink, so background positioning uses the winning border geometry while
  boundary cells remain responsible for painting the shared edge.
- The new auto-height regression failed at 115 instead of 103 before the fix
  and passes afterward; it also checks separated-border height remains 115.
  A paint regression checks stronger cell borders determine table image origin.
- Case 5562 passes with zero differing pixels (previously 34629), receipt
  `target/wpt-targeted/case-5562-collapsed-table-bounds-v1/results.json`.
  Starts 5369, 5553, 5545, 5231, 5239, 5247, 5255 and 5263 each pass 8/8,
  receipts `batch-<start>-collapsed-table-bounds-v1/results.json` in the same
  directory. Total focused pixel coverage: 65/65, unchanged pinned suite,
  viewport and zero tolerances. Runner SHA256:
  `78f928f1c9a1e15efd2e1f551495e804331c0082cf7a96f0230fee14cac7bbd5`.
- Focused runtime tests: collapsed layout 17/17, background image 16/16,
  table replay 2/2, paint artifact 31/33. The two remaining paint-test failures
  concern positioned ordering and inline clipping, not repaired in this scope.
  The other three separated-background failures from batch 5561 still require
  targeted qualification. Final clean-SHA full 6548 proof remains unachieved.

### Separated-background targeted checkpoint

- On `cf4cc5a`, strict batch 5561 completed 5 passed / 3 failed. Receipt:
  `target/wpt-targeted/batch-5561-collapsed-table-bounds-v1/results.json`.
  Remaining differences: row 5566 = 504 pixels; rowgroup 5567 = 6966;
  table 5568 = 48823. No later sequential batch was advanced.
- CSS2 section 17.6.1 requires row, column and group backgrounds to be
  invisible in separated-border spacing. The existing rowgroup test asserted
  the opposite. After replacing that expectation with cell-fragment clipping,
  the test failed before changing production code (missing fragment metadata).
  The shared column-background fragment mechanism now includes rows and row
  groups, retaining full source-box image positioning. The replacement test
  passes after the production fix; focused `separated` tests pass 2/2 and
  `background_image` tests pass 17/17.
- Strict batch 5561 after fragment repair passes 7/8. Row 5566 and rowgroup
  5567 now have zero pixel differences; table 5568 remains at 48823.
  Receipt: `target/wpt-targeted/batch-5561-separated-row-clips-v1/results.json`.
  Runner SHA256:
  `b4b3f9af680245e6a364ca918b6221aecf21b7e25aa0274eaf5270b680dbf96f`.
- Starts 5369, 5553, 5545, 5231, 5239, 5247, 5255 and 5263 pass 8/8 each
  under `target/wpt-targeted/batch-<start>-separated-row-clips-v1/results.json`:
  64/64 strict related regressions. The fixed revision, 800x600 viewport and
  zero tolerances remain unchanged; this is not full-suite closure.
- Independent layout dumps for 5566 and 5568 show tables at x19, y15/162/309,
  width 329, height 145; upstream reference geometry specifies width 325.
  Source row boxes are correctly 303x21. The 4px table-width discrepancy is a
  separate pending repair, not evidence that background clipping alone closes
  all three failures. Qualification and final full-suite proof remain pending.

### Separated auto-table width conversion

- New regression `separated_auto_table_shrink_fit_counts_outer_spacing_once`
  confirms intrinsic border-box width 325, but actual layout failed at 329.
  Shrink-fit conversion had subtracted authored padding and border only,
  leaving outer spacing already present in intrinsic width to be added again
  through Taffy's table padding. The content-box conversion now subtracts
  the table's two effective horizontal outer-spacing edges as well.
- The failing test was established before production changes and passes after
  the fix. Focused tests: `separated` 3/3, `shrink_to_fit` 3/3 and collapsed
  layout 17/17. Strict batch 5561 passes 8/8, including zero differing pixels
  for case 5568 (previously 48823). Receipt:
  `target/wpt-targeted/batch-5561-separated-outer-spacing-v1/results.json`.
  Runner SHA256:
  `522fd664885d8080114c6ec92776d38313811de48eeee0359b66862ba7630995`.
- Post-fix layout dump for 5568 shows all three tables at x19, y15/162/309,
  width 325 and height 145. Receipt:
  `target/wpt-targeted/separate-table-5568-outer-spacing-debug.bin`.
  This qualifies actual layout width, not a clipping-only workaround.
- Related starts 5369, 5553, 5545, 5231, 5239, 5247, 5255 and 5263 each pass
  8/8 under `target/wpt-targeted/batch-<start>-separated-outer-spacing-v1/results.json`.
  Total strict coverage: 72/72. Fixed upstream revision, viewport and zero
  tolerances unchanged. Next sequential start 5569; final clean-SHA full
  6548 proof remains unachieved.

### Sequential inline-table baseline checkpoint

- On `2de6611`, starts 5569 and 5577 pass 8/8 each; start 5585 completes
  7 passed / 1 failed. Receipts:
  `target/wpt-targeted/batch-<start>-post-outer-spacing-v1/results.json`.
  Start 5593 was not executed after that failure.
- Case 5588 `table-vertical-align-baseline-008.xht` differs by 15000 pixels
  from `ref-filled-green-100px-square.xht` at unchanged zero tolerances.
  This path passed in the original full report; introducing revision is not
  qualified yet. It uses definite 50x100 inline-table sizing with zero spacing,
  unlike the auto-width outer-spacing conversion just repaired.
- Actual dump `target/wpt-targeted/inline-table-baseline-5588-debug.bin`:
  the inline-block is at y51.2, 50x100; inline-table at y151.2, 50x100;
  first row and empty baseline-aligned cell have used height 0 despite the
  row's declared 100px height. Wrapper height grows to 200 instead of 100.
  Next scope is row used-height and synthetic empty-cell baseline propagation;
  do not patch the inline-table position with a fixed 100px offset.
  Final clean-SHA full 6548 proof remains unachieved.

### Empty baseline-aligned cell height repair

- Added DOM-backed regression
  `empty_inline_table_first_row_fills_definite_grid_before_baseline_alignment`:
  intended to reproduce the zero-font floated wrapper with two 50x100 boxes.
  The initial unit failed at row height 2 instead of 100, but raw diagnostics
  later showed its style attributes had not applied: it was a block table with
  UA spacing/padding, not the intended inline-table. The test now registers
  these declarations through the stylesheet API and asserts zero spacing and
  padding plus inline-table display. WPT case 5588 remains the authoritative
  pre-fix failure; do not treat that older fixture as exact WPT reproduction.
- The Taffy fallback now stretches a table cell's outer box to its row height.
  CSS vertical alignment remains on the component style and is handled by
  existing content-alignment projection. Previously baseline flex-item
  alignment retained the empty cell's intrinsic height, and table-part
  background projection then replaced the row box with that cell union.
- The initial cell-stretch-only candidate was insufficient: the focused unit
  failed at row height 2 versus 100. Strict case 5588 also failed, receipt
  `target/wpt-targeted/case-5588-cell-stretch-v1/results.json`. It is not a
  qualified repair and must not be submitted as completion.
- Raw diagnostics showed the row had height 96 before projection, while its
  empty cell had height 2 and a fixed zero preferred content height despite
  CSS `height:auto`; subsequent cell-union projection reduced the row to 2.
  Leaf conversion now preserves auto height for empty non-replaced table
  cells, allowing row stretch to assign used height. Temporary debug printing
  is removed. The corrected unit passes; focused baseline tests pass 10/10,
  separated tests 3/3 and collapsed layout tests 17/17. No position constant or
  WPT fixture/tolerance change is introduced.
- Strict current batch 5585 passes 8/8, including zero differing pixels for
  5588. Receipt:
  `target/wpt-targeted/batch-5585-empty-cell-auto-height-v1/results.json`.
  Runner SHA256:
  `0076eb13e6d324779a1e222463cc0e336c3e4ddfc01d290fd7221edf43b76540`.
- Post-fix dump `target/wpt-targeted/inline-table-baseline-5588-auto-height-debug.bin`
  shows inline-block and inline-table both at y51.2, 50x100; first row and empty
  cell also have used height 100.
- Related starts 5577, 5569, 5561, 5553, 5545, 5369, 5231, 5239, 5247, 5255
  and 5263 pass 8/8 each, receipts
  `target/wpt-targeted/batch-<start>-empty-cell-auto-height-v1/results.json`.
  Together with current batch 5585, strict coverage is 96/96 at fixed revision,
  800x600 and zero tolerances. Next sequential start 5593. Final full clean-SHA
  6548 proof remains unachieved.

### Bidi-span targeted checkpoint (anonymous line qualified; 001 open)

- On `5a90c89`, strict start 5593 completes 6 passed / 2 failed:
  case 5598 `bidi-span-001.html` = 2 pixels, max difference 7; case 5600
  `bidi-span-003.html` = 3945 pixels, max difference 255. Receipt:
  `target/wpt-targeted/batch-5593-post-empty-cell-auto-height-v1/results.json`.
  Starts 5601, 5609 and 5617 were not executed after this failure.
- Both paths failed in the original full report (1286 and 1410 pixels), so
  they are original failure paths, not newly discovered suite additions.
- Chrome 153.0.8010.36, 800x600, device scale 1 rendered the same pinned
  source/reference files: both 001 and 003 compare at zero differing pixels.
  Screenshots retained as `target/wpt-targeted/chrome-bidi-<001|003>[-ref]-v1.png`.
  The missing `>` in 003 reference HTML does not make its reftest inherently
  inconsistent: browser recovery leaves its third span in the container,
  where inherited `text-align:right` still aligns it correctly.
- W3COS 003 reference dump has its final inline principal box at x8; source
  has it at x216.25 with identical y63 and width91.75. The reference's mixed
  block/inline container is missing an anonymous inherited-alignment line box.
  DOM regression `inline_run_after_blocks_has_an_anonymous_inherited_alignment_box`
  failed before production changes (`Inline` instead of `Block`) and passes
  after wrapping normal-flow inline runs among block boxes. Original principal
  decoration and event identity remain on children, not the anonymous box.
- The first anonymous wrapper used generic `Box`: structural checks passed,
  but start 5593 still had both failures unchanged, receipt
  `target/wpt-targeted/batch-5593-anonymous-inline-lines-v1/results.json`.
  Dump showed a 300px wrapper but its inline child still at x8. Runtime inline
  formatting uses the canonical `Row` component for authored/anonymous block
  lines; the wrapper now uses that existing representation and the regression
  asserts its kind. The rebuilt runner qualifies 003 at zero differing pixels;
  strict start 5593 now passes 7/8, with only 001 still failing by 2 pixels
  (max difference 7). Receipt:
  `target/wpt-targeted/batch-5593-anonymous-inline-row-lines-v1/results.json`.
  Runner SHA256:
  `f7280b508021e695ad9781832099636638ea9a0fa037ca7b511b985b92addc40`.
- Related starts 5585, 5577, 5569, 5561, 5553, 5545, 5369, 5231, 5239,
  5247, 5255 and 5263 pass 8/8 each (96/96), receipts
  `target/wpt-targeted/batch-<start>-anonymous-inline-row-lines-v1/results.json`.
  Anonymous, bidi and mixed DOM scopes also pass on the canonical Row build.
  Sequential start 5601 remains not run because 5598 is still failing.
- Focused DOM scopes passed after initial wrapping: anonymous 28/28, bidi
  10/10, mixed 5/5 (overlapping filters, not 43 distinct assertions).
- 001 dump isolates its two remaining pixels to the decorated fourth row:
  source uses three visual glyph fragments; reference paints a single run at
  identical line top and summed advances. Font/run raster qualification remains
  pending. Both pixels lie at x13, y78/y79 beside the decorated glyph edge;
  logical background/text paint ordering requires separate qualification.
  No fixture, suite, viewport or tolerance changes; final full proof
  remains unachieved.

### Bidi logical paint order (intermediate candidate)

- Regression `bidi_visual_fragments_paint_in_logical_order_without_changing_layout`
  fails on the old implementation: visual fragment 3 does not paint before
  fragment 2. RED compiled with the WPT profile in 2m22s.
- Candidate shared PaintArtifact change assigns contiguous logical traversal
  ordinals to fully tagged bidi sibling runs while retaining visual node indices
  and layout rectangles. CSS paint phases and authored z-index are unchanged.
  DOM normalization records original unit ranks and no longer rewrites z-index
  for transparent fragments. The new regression passes after the change
  (WPT-profile compile 2m23s). PaintArtifact scope passes 32/34; the existing
  `auto_positioned_subtree_paints_after_later_normal_flow_content` z_order
  assertion and `inline_fragment_clip_keeps_layout_rect_and_clips_only_paint`
  assertion still fail as previously recorded.
- Rebuilt runner SHA256:
  `274e1fcd75dac801268d7c06c8f2e6ca2b4eb26b56cc547d2cb5bea96f833db7`.
  Strict start 5593 remains 7/8: 001 improves to one differing pixel with
  max difference 1; 003 stays at zero. Receipt:
  `target/wpt-targeted/batch-5593-bidi-logical-paint-order-v1/results.json`.
  The logical-order-only candidate did not qualify at zero tolerance and was
  not submitted alone. Foreground glyph overlap/compositing required the shared
  run replay below; starts 5601 onward were not run at this checkpoint.
  This is not full completion evidence.

### Compatible bidi foreground run qualification

- Logical tree ordering alone left one antialiasing pixel (maximum difference
  1) in 001. Shared `bidi_paint` replay preserves per-box logical backgrounds
  and shapes compatible contiguous visual text fragments as one foreground run.
  Headless and window render paths both consume this replay; original node
  identity, rectangles and CSS property trees remain unchanged.
- Eligibility requires static, untransformed text with no float, opacity/filter
  effect, border, padding, margin, shadow, outline or extra spacing; foreground
  styles and property-tree handles must match. Different text styles remain
  borrowed and unmerged. Both new replay regressions pass, as do the logical
  order regression and two existing table replay regressions.
- Strict start 5593 now passes 8/8 at zero tolerances, including 001 and 003.
  Receipt `target/wpt-targeted/batch-5593-bidi-foreground-run-v1/results.json`.
  Runner SHA256:
  `17714d115081c326d969518b533d9548b2d1bf0295882a92f52afa3ca5309823`.
- Related starts 5585, 5577, 5569, 5561, 5553, 5545, 5369, 5231, 5239,
  5247, 5255 and 5263 pass 8/8 each, receipts
  `target/wpt-targeted/batch-<start>-bidi-foreground-run-v1/results.json`.
  Related strict coverage is 96/96, plus the current 8/8. Recompiled DOM bidi
  scope passes 10/10 and anonymous scope 28/28. Next sequential start is 5601.
  Parent read-only `pnpm files:size:check` passes with violations=0; new replay
  module is 240 lines. Final clean-SHA full 6548 proof is still unachieved.

### Post-bidi sequential checkpoint on 7532b9b

- Starts 5601, 5609, 5617 and 5625 pass 8/8 each, receipts
  `target/wpt-targeted/batch-<start>-post-bidi-foreground-run-v1/results.json`.
- Start 5633 stops sequential advancement with 5 passed / 3 failed:
  `letter-spacing-080.xht` = 1600 differing pixels; `letter-spacing-091.xht`
  and `letter-spacing-092.xht` = 800 each; all max difference 255.
  Receipt `target/wpt-targeted/batch-5633-post-bidi-foreground-run-v1/results.json`.
  Start 5641 was not executed. These failures remain open; final full proof
  remains unachieved.

### Letter-spacing computed metric checkpoint (091/092 qualified; 080 open)

- Strict failures at indices 5639/5640 use `12ex`/`+12ex`. Dumps show span
  margin uses Ahem x-height (192px, glyph x220), but text letter spacing uses
  the fallback half-em value (120px, second glyph x148).
  Diagnostics: `target/wpt-targeted/letter-spacing-5639-{source,reference}-debug.bin`.
- DOM regression
  `relative_letter_spacing_uses_final_font_metrics_and_inherits_computed_pixels`
  fails before the fix (120 instead of 192) and passes after resolving authored
  `ex`/`em` spacing against final computed font metrics. Signed ex, em and
  inheritance to a differently sized child are covered. `ex_` filter passes
  13/13 (includes unrelated name matches, not 13 font-metric-only tests).
- Index 5635 (`080`) is a separate pinned-reference inconsistency: source has
  20px Ahem and 6em=120px, while its specified `007-ref` has hardcoded 96px.
  Isolated Chrome 153.0.8010.36, 800x600, scale1, with original pinned resources
  fulfilled read-only at a virtual HTTP origin and confirmed loaded Ahem,
  reproduces source span x148 vs reference x124 and 1804 differing pixels,
  max difference255. Screenshots:
  `target/wpt-targeted/chrome-letter-spacing-080.xht-v1.png` and
  `target/wpt-targeted/chrome-letter-spacing-007-ref.xht-v1.png`.
  No fixture/reference, suite or tolerance change is authorized; 080 stays open.
- Rebuilt runner qualifies 091/092 at zero differing pixels. Strict start 5633
  is now 7/8; 080 remains unchanged at 1600 differing pixels, max255.
  Receipt `target/wpt-targeted/batch-5633-computed-letter-spacing-v1/results.json`.
  Runner SHA256:
  `aaaa555e14a407a303b9523b9824849fcad4178c254e56017c5e77149d268440`.
- Related starts 5625, 5617, 5609, 5601 and 5593 pass 8/8 each (40/40),
  receipts `target/wpt-targeted/batch-<start>-computed-letter-spacing-v1/results.json`.
  Start 5641 is not executed after the open 080 failure. No change to pinned
  fixtures, references or tolerances; final full 6548 proof remains unachieved.

### Text-align / white-space checkpoint (008 qualified; four open)

- Keeping 080 as an open failure (not skip/pass), independently inspected
  adjacent batches on 1c48a62. Starts 5641, 5649 and 5657 pass 8/8 each.
  Receipts `target/wpt-targeted/batch-<start>-post-computed-letter-spacing-v1/results.json`.
- Start 5665 is 3/8: failures 001=6800 pixels, 002=8400, 005=4000,
  006=4000 and 008=2400 (all max255). Receipt
  `target/wpt-targeted/batch-5665-post-computed-letter-spacing-v1/results.json`.
  Start 5673 remains not run after this new batch failure.
- 008 dump shows normal RTL spans start x228 while its justified nowrap
  foreground starts x188. Non-wrapping justified lines need directional-start
  fallback, rather than unconditional left. DOM regression
  `rtl_justified_nowrap_line_keeps_its_directional_start_alignment` fails before
  production changes. Initial row-only fix still failed because a single block
  can lower directly as Text; both row and direct Text normalization now retain
  directional-start alignment before bidi consumes direction, and the test passes.
- Rebuilt runner qualifies 008 at zero differing pixels. Start 5665 now passes
  4/8: 001=6800 pixels, 002=8400, 005 improves to2400, 006=4000 remain open.
  Receipt `target/wpt-targeted/batch-5665-nonwrapping-justify-start-v1/results.json`.
  Runner SHA256:
  `ad3c90f39fc383e08282f8ac49847b1fb81d9a9f26ef0ecd5340e8862d12ffd5`.
  Recompiled DOM bidi scope passes10/10. Related starts 5657, 5649, 5641,
  5625, 5617, 5609, 5601 and 5593 pass8/8 each (64/64), receipts
  `target/wpt-targeted/batch-<start>-nonwrapping-justify-start-v1/results.json`.
- Wrapping justification and cross-span soft-wrap boundaries are separate open
  defects, not implemented by replacing them with right alignment.
  Final full 6548 proof remains unachieved.

### Inline word grouping intermediate candidate (not qualified alone)

- New regression `style_boundaries_inside_one_ascii_word_do_not_create_line_break_items`
  fails on 6941e0e (`Inline` instead of a shared non-breaking word group).
  Candidate groups undecorated same-direction ASCII alphanumeric inline text
  in normal/pre-line block formatting contexts, retaining child styles and
  native event identity. Structural regression passes; bidi filter passes10/10.
- Rebuilt runner SHA256:
  `14dd4273e434e0e5da6dbae087d521bd6840c2256d618b6a5a97d43a1173318c`.
  Strict start5665 remains4/8: 001=6400 pixels, 002=11200, 005=6400,
  006=11200. This intermediate candidate worsened several open failures and
  was not submitted alone as a qualified repair. Receipt
  `target/wpt-targeted/batch-5665-inline-word-group-v1/results.json`.
- Dump `target/wpt-targeted/text-align-whitespace-5665-word-group-debug.bin`
  shows the anonymous word's used width120 while its child glyph extent is280.
  First group is at y51.2, standalone collapsed whitespace at y71.2, and second
  group at y91.2 instead of the next20px line. Intrinsic min-content recovery
  and line-end collapsible whitespace handling are required before qualification.
  Existing 080 conflict and all other failures remain open; no fixture, viewport
  or tolerance changes. Full6548 completion remains unproven.

### Inline word min-content and wrap separator qualification

- Root cause in `component_min_content_width`: horizontal nonwrapping flex
  used largest-child minimum rather than the shared line's sum. It now sums
  child min-content contributions plus horizontal gaps. Regression
  `nonwrapping_inline_flex_min_content_sums_its_word_fragments` passes.
- Generated anonymous word groups carry a private marker; only following
  collapsible whitespace at a filled/overwide line boundary is removed from
  the virtual Taffy flex line without changing the original CSS style.
  Authored flex items and preserved pre-line segment breaks are not discarded.
  Regression `collapsed_separator_after_a_full_generated_word_adds_no_empty_line`
  passes for available widths100 and50 with baseline alignment (next word
  y20, outer height40). Zero size alone failed the overwide baseline case
  (next word y39.2); virtual display-none removes that separator's line strut.
  Three shrink-to-fit tests also pass.
- Grouping bypasses explicit parent bidi controls and child directional/control
  boundaries. The structural word-group regression and bidi10/10 pass.
- Runner SHA256
  `d11d83202db1982ecd33639251839b55149d57c8937d4ce9190e4c6c1a1b8215`:
  strict start5665 is6/8; 001 and005 now have zero differing pixels.
  002=2400 and006=3600 remain FAIL (maximum channel difference255).
  Receipt `target/wpt-targeted/batch-5665-word-min-content-hidden-separator-v1/results.json`.
  The intermediate 4/8 / worsened-pixel receipt above is not completion evidence.
- Related strict batches start5657,5649,5641,5625,5617,5609,5601,5593,
  5585,5577,5569,5561,5553,5545,5369,5231,5239,5247,5255,5263
  each pass8/8 (160/160 total, zero pixel differences). Receipts:
  `target/wpt-targeted/batch-*-word-min-content-hidden-separator-v1/results.json`.
  Runtime focused units pass5/5; DOM structural, bidi and anonymous filters
  pass1/1,10/10 and28/28 respectively. This is focused qualification, not
  the final same-clean-SHA full6548 gate.
- Fresh Chrome153 oracle uses the original `.xht` files served as
  `application/xhtml+xml`, Ahem loaded, viewport800x600. Source/reference
  comparisons remain nonzero: 002=465 and006=485 pixels (maximum107);
  080=1804 pixels (maximum255), reconfirming its existing conflict.
  Earlier text/html-served XHTML screenshots are invalid oracle evidence,
  superseded by `target/wpt-targeted/chrome-xhtml-*-v2.png`.
  These Chrome receipts do not qualify the remaining native failures.
  No pinned fixture, suite, viewport or tolerance edits. Final full proof remains
  unachieved.

### Justified paragraph paint qualification

- Native start5665 still has002=2400 and006=3600 differing pixels after
  c79dd63. Skia's `aligned_text_x` treats justify as left alignment; multiline
  paint did not distribute the positive remainder to inter-word spaces.
- Candidate adds shared shaped-word positions and normalized-source paragraph
  terminal recovery; Skia uses expansion only on automatic normal/pre-line
  wraps. Preserved breaks and RTL paragraph terminals use end-of-paragraph
  alignment rather than justification. Glyph advances are not stretched.
- New word-position and forced-versus-automatic terminal tests pass2/2;
  the complete text-layout unit filter passes25/25.
- Runner SHA256
  `3536621e4c4a2c99bd97b1dd6c6a2c0d12a2992967310575da93687543a8a974`:
  strict start5665 now passes8/8, every comparison zero differing pixels and
  zero maximum channel difference. Both002 and006 are repaired.
  Receipt `target/wpt-targeted/batch-5665-justified-paragraph-v1/results.json`.
  Related starts5641,5649,5657,5593,5601,5609,5617,5625 each pass8/8:
  64/64 comparisons with zero pixel differences, receipts
  `target/wpt-targeted/batch-*-justified-paragraph-v1/results.json`.
  No fixture, revision, viewport or tolerance modifications. This is focused
  proof, not the final same-clean-SHA full6548 gate; other failures remain open.

### Short inline baseline qualification

- Fresh start5681 on40bb123 is7/8. `text-decoration-va-length-002.xht`
  differs by39 pixels (maximum254). Actual/reference images show the differing
  pixels in the preceding instruction paragraph, not a rendered underline.
  Both images lack the expected black decoration; equality alone would not
  prove that decoration capability is implemented.
- Source/reference layout dumps have identical instruction-fragment boxes.
  Short single-line paint nevertheless subtracts string-specific ink-bottom
  overflow from glyph top. Different white inline fragments can consequently
  erase different instruction pixels above their own line.
- Candidate disables that ink-dependent baseline compensation for ordinary
  inline text while preserving existing block/control compensation. Pixel unit
  compares short-line fragments with direct font-baseline paint, using three
  strings with different ink bounds; the new pixel unit passes1/1.
  Skia unit filter is37 PASS /1 FAIL: the older
  `default_ascii_text_is_pixel_invariant_across_inline_fragments` remains failed,
  as previously documented above, and is not counted green.
- Runner SHA256
  `f0e5f53e716e5ecd5304722166b01c0fcf74944ceab603951e56c36b8ceec360`:
  strict start5681 passes8/8 with zero differing pixels and zero maximum
  channel difference. The39 instruction-pixel differences are eliminated.
  Receipt `target/wpt-targeted/batch-5681-inline-baseline-v1/results.json`.
  Related starts4281,4289,4297,4265,5673,5665,5657,5593 each pass8/8,
  total64/64 with zero pixel differences. Receipts
  `target/wpt-targeted/batch-*-inline-baseline-v1/results.json`.
  Underline/overline painting remains a separately identified implementation gap;
  no fixture, viewport or tolerance edits, and no full-suite completion claim.

### Anonymous first-formatted-line indent qualification

- Fresh strict start5689 on1d4b28b is5/8: indent012=2440,
  indent013=3904, indent014=9600 pixels (maximum255). Other five cases pass.
- `wrap_inline_runs_between_block_boxes` copied inherited text-indent into
  every anonymous run, including runs after a preceding formatted block line.
  Candidate keeps the inherited indent only for the first formatted line;
  later anonymous inline fragments have no outer-context indent. Principal
  child blocks and atomic inline containers retain their own inner context.
  Empty preceding blocks do not consume the first formatted line.
- New `anonymous_runs_indent_only_the_first_formatted_line` failed before
  repair (tail indent12 instead of0), then passes. DOM anonymous filter29/29
  and bidi10/10 pass.
- Runner SHA256
  `3114e6efb88c0dc107868755eb4c11f177378d1b01ea89ceecae8a3fc30f4fd9`:
  start5689 passes6/8. Indent014 moves9600 pixels to0 (maximum0), while
  indent012=2440 and indent013=3904 remain FAIL (maximum255). Receipt
  `target/wpt-targeted/batch-5689-anonymous-first-indent-v1/results.json`.
  Related starts5681,5673,5665,5593,4265,4281,4289,4297 each pass8/8,
  total64/64 with zero pixel differences. Receipts
  `target/wpt-targeted/batch-*-anonymous-first-indent-v1/results.json`.
  No fixture, suite, viewport or tolerance changes; final full6548 proof
  remains unachieved.

### Indented unbroken inline box qualification

- Indent013 source/reference dumps show its black inline fragment atx8/width160
  versus reference x168/width92.4375. Source paints its glyphs with an indent,
  but the fixed paragraph-width background remains at the old box location.
- Candidate moves the first in-flow unbroken ASCII word's whole inline box
  using the resolved indent margin and removes duplicate paint indentation.
  The word keeps intrinsic width instead of being forced to100% paragraph
  width. Multi-word paragraph wrapping remains unchanged; a later atomic inline
  is no longer incorrectly selected ahead of preceding ordinary text.
- New `indented_unbroken_inline_text_moves_its_background_box` fails before
  repair (margin0 instead of160), then passes.
- Runner SHA256
  `ec511ffffeac376cf6ae1989144aa8df328873595b88d4a03687b4257ec5c3af`:
  start5689 now passes7/8. Indent013 moves3904 pixels to0 (maximum0),
  indent014 remains0; indent012=2440 pixels (maximum255) remains FAIL.
  Receipt `target/wpt-targeted/batch-5689-indented-word-box-v1/results.json`.
  Related starts5681,5673,5665,5593,4265,4281,4289,4297 each pass8/8,
  total64/64 with zero pixel differences. Receipts
  `target/wpt-targeted/batch-*-indented-word-box-v1/results.json`.
  Final full6548 proof remains unachieved; no fixture or tolerance edits.
- DOM indent4/4, anonymous29/29 and bidi10/10 pass. Indent012 pre-candidate
  source/reference dumps agree on width204 and child positions but disagree
  on outer height54 versus64: the source lacks the10px descent below the
  baseline-aligned50px atomic box. Receipts
  `target/wpt-targeted/indent-5692-*-pre-box-candidate.bin`; this is the next
  independent repair point, not a qualified fix yet.

### Lowered inline row descent qualification

- Indent012 still differs2440 pixels on9b86b71. Source outer height54 versus
  reference64 omits the font strut's10px below the empty50px inline-block's
  bottom-margin baseline; widths and child positions already agree.
- DOM now tags authored block containers lowered into anonymous inline rows,
  so layout can reserve the strut without changing ordinary authored Flex.
  Candidate adds auto-height minimums for bottom-baseline empty inline-blocks,
  including child border/padding/margin contributions and parent box edges,
  never below the complete containing font strut. Percentage height without
  a definite basis and nonempty internal line-box baselines are not guessed.
- New focused unit contrasts an authored Flex (height50) with a tagged
  inline row (height60), including its existing strut min-height50; a short
  atomic box still retains the containing50px font strut. Latest source unit
  passes1/1, related runtime units5/5. Fresh DOM indent4/4, anonymous29/29,
  bidi10/10 pass. Original fixtures and zero tolerances remain unchanged.
- First runner qualification remains7/8: indent012 still2440 pixels and
  height54. The actual DOM already supplies an initial font-strut min-height50,
  so the candidate's min-height-auto guard incorrectly excluded that row.
  Candidate now composes the baseline requirement with the existing minimum;
  the unit includes that DOM-like min-height and the short-box font-strut case.
  Receipt `target/wpt-targeted/batch-5689-inline-block-strut-v1/results.json`.
  This failed intermediate is not a qualified repair.
- Runner SHA256
  `f47066b3603532d79b8c071aff628401480538d34998d2943ee1098f0f663f27`:
  strict start5689 passes8/8 with zero differing pixels and zero maximum
  channel difference. Indent012 moves2440 pixels to0;013/014 remain0.
  Receipt `target/wpt-targeted/batch-5689-inline-block-strut-v2/results.json`.
  Related starts5681,5673,5665,5593,4265,4281,4289,4297,5369,5545,5553,
  5561 each pass8/8, total96/96 with zero pixel differences. Receipts
  `target/wpt-targeted/batch-*-inline-block-strut-v2/results.json`.
  This is focused qualification, not final full6548 proof; full-suite completion
  remains unachieved.

### Final-font ex text-indent qualification

- After bb0446a, starts5697/5705/5713 each pass8/8; start5721 is7/8:
  `text-indent-091.xht` differs1024 pixels (maximum255). Receipt
  `target/wpt-targeted/batch-5721-after-inline-block-strut/results.json`.
- Parser fallback represented12ex asEm(6), unlike margin-left's final-font
  x-height recovery. Computed style now resolves authored signed ex text-indent
  with the final cascaded font, before descendants inherit the computed pixels.
- `ex_text_indent_uses_final_font_metrics_and_inherits_computed_pixels` first
  fails (Em6 versusPx153.6), then passes for12ex,+12ex,-2ex with a child whose
  font size changes. Indent unit filter passes5/5.
- Runner SHA256
  `a3cd3bacd8b0ea9e149e39f0bd2223d8accbf5d3607ed9d169e7f2604735678d`:
  strict start5721 passes8/8, every comparison zero differing pixels and
  zero maximum channel difference. Indent091 moves1024 pixels to0.
  Receipt `target/wpt-targeted/batch-5721-final-font-ex-indent-v1/results.json`.
  Related starts5713,5705,5697,5689,5681,5673,5665,4265 each pass8/8,
  total64/64 with zero pixel differences. Receipts
  `target/wpt-targeted/batch-*-final-font-ex-indent-v1/results.json`.
  No fixture or tolerance changes; final full6548 proof remains unachieved.

### Table inline-row typography qualification

- Fresh start5745 on2636cc0 is5/8: text-indent-applies-to006/007/008 each
  differ320 pixels (maximum255). Source006's word already has the correctx168;
  its anonymous row has font_familyNone/line_height1.2, shifting glyph y to
  52.8 instead of reference51.2 despite the cell's Ahem/line_height1.
  Receipt `target/wpt-targeted/batch-5745-after-final-font-ex-indent/results.json`.
- Table-part anonymous inline rows now inherit text typography, retaining their
  transparent principal-box defaults. Outer indent already projected into child
  layout/paint is not repeated on the row.
- New regression uses an explicit Text child in a valid table/row/cell tree;
  direct element text and malformed-cell setups did not exercise this path and
  are not counted as root-cause RED. Real-path RED is font_familyNone versusAhem,
  then GREEN verifies family,20px size,line_height1 and zero row indent.
  Anonymous30/30, indent6/6 and bidi10/10 filters pass.
- Runner SHA256
  `7d1176f047a16b1fd7d36cb46fab41c6dd0edad4e03d5fae25454c7a331f251e`:
  strict start5745 passes8/8 with zero differing pixels and zero maximum
  channel difference. Applies-to006/007/008 each move320 pixels to0.
  Receipt `target/wpt-targeted/batch-5745-table-inline-typography-v1/results.json`.
  Related starts5737/5729/5721/5689/5681/5673/5665/5369/5545/5553/5561/5593
  pass96/96:94 match comparisons have zero pixel differences;2 expected
  mismatch comparisons pass. Receipts use
  `target/wpt-targeted/batch-{start}-table-inline-typography-v1/results.json`.
  No fixture or tolerance edits; final full6548 proof remains unachieved.

### Intrinsic text-indent batch qualification

- Fresh start5761 on a9f3bdd passes4/8. Intrinsic001/002/003/004 differ
  2196/1476/1008/720 pixels respectively (maximum255).
  Receipt `target/wpt-targeted/batch-5761-after-table-inline-typography/results.json`.
- Source001's final two pre elements lower their authored newline to a space;
  HTML pre UA whitespace/family/margins were missing, and inheritance would
  overwrite inherited-property UA defaults. Pre now supplies pre/monospace/1em
  vertical margins. The cascade retains these when no author declaration exists,
  while explicit author inherit/unset still requests inheritance.
- Two focused tests reproduce Normal versus Pre before the fix and pass after it,
  including actual computed style and explicit author inheritance. UA4/4,
  anonymous30/30 and bidi10/10 filters pass.
- Runner SHA2560cb42404f0c2c07ebb39d7783855754aafa451b9e2a690790c5cea0d1cdf3079
  reruns this batch4/8: pixels2160/1440/1008/720. The first two improve36 pixels
  each, but all four remain open. Receipt
  `target/wpt-targeted/batch-5761-pre-ua-v1/results.json`.
  Preserved newlines now remain Text newline children; intrinsic aggregation
  still sums across them and the rendered line has not yet split.
  This is not completion of the four intrinsic failures or the full6548 suite.
- Pre-UA related starts5753/5745/5737/5729/5665/5593 pass48/48. Receipts use
  `target/wpt-targeted/batch-{start}-pre-ua-v1/results.json`.
- Intrinsic atomic-IFC test reproduces forced-break max-content72 versus48
  before the algorithm change, then passes after it. The latest test also checks
  positive/negative soft-break contributions and pre's no-soft-wrap behavior.
  Atomic inline min-content uses child min-content rather than preferred width.
  Segment aggregation and generated float shrink-fitting qualify below.
- Related runtime tests pass3 shrink-fit width checks,1 nonwrapping fragment
  check and1 collapsed-separator check. The wrapped-text-height check fails
  (`text.height > 19.2`), also on preserved pre-candidate executable11f7b072;
  it constructs direct components without the new IFC marker, so the new path
  is not entered. The older executable uses a different feature set; this is
  baseline evidence, not a same-feature clean-SHA comparison or all-green gate.
- Runner db0b97e4566bd82e0696ab35730336947e9c9d611b72a08ca7167e73801ae983
  passes5/8 at start5761: intrinsic002 moves1440 to0, while001/003/004
  retain216/864/432 pixels. Receipt
  `target/wpt-targeted/batch-5761-intrinsic-segments-v1/results.json`.
- Source001 constrained soft-break floats are56/80px instead of54/78px because
  min-content still includes outer margins. Both shrink-fit bounds now remove
  separately applied margins; the new1px-margin/3px-border assertion passes.
- Negative-indent line-height wrappers hide break Text children. DOM now marks
  transparent internal line items, and the intrinsic pass reads their break
  semantics without removing or changing the principal paint/event box.
  Wrapped-separator assertion passes. V2 runner
  336a10920ef27e52e5192e1c1a805cddb4092e6973896fc1f98796908878c3d5
  passes6/8:001/002 are0 pixels and003/004 each retain216.
  Receipt `target/wpt-targeted/batch-5761-intrinsic-segments-v2/results.json`.
- Remaining differences are the final pre box42px versus reference30px.
  A real DOM regression reproduces a missing preserved newline. Carrying
  white-space alone is insufficient: bidi's empty-inline pruning also classified
  nonempty preserved whitespace as empty. That predicate now retains pre/pre-wrap
  whitespace and pre-line newline content. The same DOM test becomes GREEN;
  UA5/5, anonymous30/30 and bidi10/10 filters pass.
- Latest runner SHA256
  `566d8f0e777e27d6bf15fca99609819a270737b41d8d3dd52999cf2d52cce81d`
  passes strict start5761 at8/8, with zero maximum difference and zero differing
  pixels for every comparison. Intrinsic001/002/003/004 move2196/1476/1008/720
  pixels from the clean pre-candidate baseline to0.
  Receipt `target/wpt-targeted/batch-5761-intrinsic-segments-v3/results.json`.
  Related starts5753/5745/5737/5729/5721/5689/5681/5673/5665/5545/5553/5593
  pass96/96:94 match comparisons have zero differing pixels/maximum difference;
  2 expected mismatch comparisons pass. Receipts use
  `target/wpt-targeted/batch-{start}-intrinsic-segments-v3/results.json`.
  No fixture or tolerance changes; final full6548 clean-SHA proof remains open.

### RTL block-level alignment qualification (wrap remains open)

- Fresh start5769 on8c15d01 passes4/8. RTL002 differs2560 pixels;
  wrap notref-block-margin incorrectly equals its mismatch reference, inline
  margin differs34728 pixels from the float reference, and wrap001 differs36267.
  Receipt `target/wpt-targeted/batch-5769-after-intrinsic-segments/results.json`.
- RTL002's30em principal block is internally Flex under a40em RTL block.
  Existing projection only recognizes DisplayBlock, leaving its x8 instead of
  x168 (reference negative-right-margin block needs x248). Its text already has
  the correct first-line displacement; the missing outer alignment is separate.
- RTL fixed-width projection now recognizes block-level Flex/Grid/Table/ListItem
  outer boxes as well as Block. In-flow floats keep their float-placement
  authority, and inline-level boxes remain outside this block projection.
  Latest margin/descendant/generated-and-authored-flex/float controls pass;
  rtl_block3/3 and rtl_fixed_block1/1 filters pass.
- Runner SHA256
  `473c7a275a077ae503e5c4dadce2e2a49b0ce37b6525311f96cf40c7d5e0175e`
  passes5/8 at start5769. RTL002 moves2560 pixels to0, maximum difference0;
  the other four passing cases stay passing. The three wrap failures retain
  their pre-candidate pixel differences/relations, not hidden or counted green.
  Receipt `target/wpt-targeted/batch-5769-rtl-block-level-v1/results.json`.
  Related starts5761/5753/5745/5737/5729/5721/5689/5681/5665/5545/5553/5593
  pass96/96:94 match comparisons have zero differences;2 expected mismatch
  comparisons pass. Receipts use
  `target/wpt-targeted/batch-{start}-rtl-block-level-v1/results.json`.
  No fixture/tolerance changes; final full6548 zero-failure proof is unachieved.
- Right-float raw-IR alignment unit fails0 versus50px on both latest414fd857
  and preserved pre-candidate11f7b072 executables. Its parent is LTR and never
  enters this RTL projection. The older executable has a different feature set;
  this is baseline evidence, not a same-feature clean-SHA gate or a green test.
- Separate wrap diagnostics on the pre-candidate runner show784px original
  available width but684px throughout the inline-margin reference, producing an
  extra line. Its long InlineText layout rect is16px tall despite its paragraph's
  multi-line height. These remaining three reference failures are still open;
  neither uniform padding nor a tolerance change is an appropriate closure.
  Next focused entry: start5773 limit4 retains the three failing wrap cases and
  their passing mismatch control, before requalifying the enclosing8-case batch.

### Inline continuation margin qualification (float wrapping remains open)

- Base903b59b retains three wrap failures at5773/5775/5776. Inline-margin and
  block-margin references incorrectly paint identically; the first inline edge
  must not contract every continuation line.
- Shared inline continuation geometry now restores logical-start margin along
  with padding/border, while keeping the first fragment and block boxes intact.
  Skia, CPU and GPU use the same box for continuation width and position.
  Text-layout26/26 tests pass, including LTR/RTL leading-edge and block controls.
  The dynamic-js + Skia + GPU + CPU-render library check passes (existing
  warnings remain); CPU/GPU pixels have not been accepted.
- Runner SHA256 is
  `2afe425891e749862fbd16165052025bda4ccca26ab5b913a0b4c1fdcef32989`.
  `batch-5773-inline-continuation-margin-v1/results.json` passes two of four:
  the block-margin mismatch now correctly differs by34667 pixels; the other
  expected mismatch remains34728. Inline-margin versus float still fails34733,
  while the primary indent versus inline-margin difference falls36267 to1600.
  The enclosing5769 eight-case receipt improves5/8 to6/8. All pixel tolerances
  remain zero, upstream inputs and viewport800x600 are unchanged.
- Related starts5761/5753/5745/5737/5729/5721/5689/5681/5665/5545/5553/5593
  each rerun eight cases under `batch-<start>-inline-continuation-margin-v1`:
  96/96 pass, with94 exact zero-pixel matches and two expected mismatches.
  The existing Skia continuation-edge unit also passes. This is focused
  qualification, not a full6548-suite or browser/native journey acceptance.
- Float-reference diagnostics still show4539.125px unwrapped long InlineText
  below a100px by4.8px float, instead of a first-line exclusion followed by
  normal-width lines. Multi-line inline background fragments and layout height
  are also open. The margin correction alone is not full closure of these cases.

### Skia shaped inline background qualification (float wrapping remains open)

- Continuing from698cefa, the primary wrap's remaining1600 pixels form the
  100px first-line indentation multiplied by the16px inline background height.
  Glyph rows agree, but both implementations still lack proper multi-line
  background fragments; removing only the indentation paint is not closure.
- A non-fixture-specific Skia pixel test,
  `inline_background_uses_first_and_continuation_fragments`, is confirmed RED:
  the indent probe is yellow instead of white. Its first/continuation probes
  additionally require actual fragment backgrounds (transparent glyph paint).
  The initial test-only compile used an incorrect type name; after correcting
  it toDimension, compilation succeeds and the pixel assertion genuinely fails.
- Candidate shared fragment geometry excludes margins, paints vertical edges
  on every line, and includes logical-start/end edges only on first/last lines.
  Skia uses retained shaped lines and advances for background slices instead
  of the leaf's single layout rectangle. The post-fix pixel unit is GREEN;
  background-filter35/35 and text-layout26/26 unit tests pass. CPU/GPU
  integration and full-suite acceptance remain pending.
- Runner SHA256 is
  `724f505119b64ebc8083a8193eb9eefdd2095513024a94e4c90266ab6de1b8f3`.
  `batch-5773-shaped-inline-background-v1/results.json` passes3/4: the primary
  indent versus inline-margin match becomes exact zero pixels (previous1600).
  The two expected mismatches pass46485 and73839 pixels. Inline-margin versus
  float remains FAIL73806 pixels (previous34733); painting actual multi-line
  backgrounds exposes the underlying float layout difference, not closure.
  Inline border/radius/image fragmentation and float exclusions are not claimed
  closed by this scoped Skia qualification.
- The enclosing5769 batch improves6/8 to7/8. Related eight-case starts
  5761/5753/5745/5737/5729/5721/5689/5681/5665/5545/5553/5593 under
  `batch-<start>-shaped-inline-background-v1` pass96/96:94 zero-pixel matches
  and two expected mismatches. This is not final6548 acceptance. The next
  focused failure remains5775, requiring breakable float-side line layout
  rather than an unwrapped atomic text box below the float.

### Float-text integration and focused qualification history

- Following3ea871c,5775 remains FAIL73806 pixels. Existing layout evidence
  shows the float reference's long InlineText as an atomic4539.125px box below
  a100px by4.8px float. A decoration-only or first-line-only patch does not
  establish general float-side line layout.
- Shared run-width wrapping now accepts resolved per-line widths, falling back
  to the full containing width after supplied exclusion bands. Existing
  first-line APIs delegate to the same greedy algorithm. Forced breaks consume
  a band; no-wrap/pre retain their authored break behavior. A unit covers
  different widths on three successive lines, forced breaks, white-space
  controls and equivalence with the existing one-band API.
- Text-layout27/27 unit tests pass, including the new multi-band test and
  existing first-line, whitespace, shaped-width and punctuation controls.
  This primitive is not yet connected to float
  geometry, retained-cache band identity, layout height or paint positions;
 5775 is not claimed repaired. Current PaintArtifact receives both indexed
  layout rectangles and parent-linked immutable paint-node styles; geometry
  must originate from actual layout, not fixture text or authored API metadata.
  No preparation commit or push is claimed.
- Retained shaped-text keys now contain all band widths, not merely the first
  width. The existing Skia first-line retained API delegates to the multi-band
  API; origins remain paint geometry and do not invalidate identical shaping.
  A cache unit verifies same-band pointer reuse and changed second-band line
  results. Text-layout28/28 tests pass after this cache extension; layout/paint
  float-band producers still need integration before5775 can be re-evaluated.
- A runtime Component-row layout reproducer,
  `breakable_inline_text_starts_in_the_leading_float_side_band`, is confirmed
  RED: in a240px row beside a60px by6px float, the text is x0/y6/width507.7422/
  height16. It retains unwrapped intrinsic width and starts below the float.
  The existing oversized-inline-replaced-box float control passes, and must
  remain distinct from breakable text. The DOM-generated inline-formatting
  marker must govern CSS integration: authored CSS Flex must not accidentally
  acquire float semantics from this runtime Component-row reproducer.
- Shared layout now has typed resolved float margin boxes and a line-band
  intersection function. It handles simultaneous left/right floats, distinct
  float bottoms, partial vertical overlap and completely excluded zero-width
  bands. A geometry unit exercises multiple affected lines and release back
  to full width. Its qualification is pending; this geometry primitive is not
  yet wired into the layout/paint flow producer. A zero-width band must advance
  past an exclusion before the wrapping API is called, not be clamped into a
  fictitious one-pixel/full-width line.5775 remains open.
- A shared float-text resolver now reads parent-linked node styles and actual
  indexed layout rectangles. It resolves parent padding/borders and float
  margin boxes using the containing width/viewport, builds multiple affected
  line bands, and measures used height through the existing font-aware run
  measurement. Admission requires the DOM inline-formatting marker and a
  leading float group followed by one inline text run; other shapes remain
  open. Completely closed bands stay on the existing below-float path pending
  vertical-advancement integration. Skia + dynamic-js library type checking
  passes (existing warnings remain). The preceding line-band geometry unit
  passes; the new resolver's runtime tests and consumer integration remain
  pending. This is not5775 pixel acceptance or full float conformance.
- Layout projection now consumes the resolver after float placement, replacing
  the unwrapped leaf geometry and propagating multi-line used height through
  unconstrained auto-height ancestors and subsequent in-flow sibling subtrees.
  The layout postcondition now explicitly supplies the generated-IFC marker;
  the earlier raw Component-row RED is diagnostic evidence, not a claim that
  authored CSS Flex should support floats. Post-fix qualification is pending.
- Window/headless retained paint supplies the actual layout viewport. Artifacts
  clear stale private band annotations and regenerate them from the shared
  resolver; Skia uses these widths and origins for wrapping, backgrounds and
  glyph placement. Cache, producer and consumer thus share resolved line bands.
  Source has not yet passed focused pixels; CPU/GPU consumers remain open.
  Parent adjacency is built once, float-free documents exit early, and only
  float-owning formatting contexts are examined (no per-parent full-tree scan).
- The marked-IFC layout postcondition passes after projection integration.
  The integration library check also passes; subsequent height propagation
  admits auto/Px minimum/maximum constraints (including DOM line struts), with
  minimum height winning conflicting maximum height. Other height constraint
  forms remain open. Latest source still requires runner build and focused
  pixels before any commit/closure claim. CPU/GPU band consumption is not yet
  implemented and must not be represented as multi-backend acceptance.
- The integration runner (before the subsequent flowing-sibling coordinate
  correction) SHA256 is
  `d7524e2c79dd0aa1dbe65458764ed5214ba138bafd7808bcd5d731b7738e50c5`.
  `batch-5773-float-text-integration-before-following-flow-v1/results.json`
  passes4/4, including the previously failing float reference exact match.
  This is diagnostic integration evidence, not latest-source qualification.
- Static review found that a second float-text paragraph compared later siblings
  against original coordinates after the first paragraph had translated them.
  Projection now snapshots current sibling coordinates at each propagation
  level. A consecutive-two-row/following-flow regression is compiling; the
  latest-source runner is queued behind it. Final focused rerun and related
  regression remain required before any scoped commit/push or closure claim.
- Latest-source consecutive-two-row regression passes. The marked float-side
  layout postcondition and oversized-replaced-item control also pass; shared
  text-layout28/28 pass after all integration/coordinate changes. The latest
  runner remains in build; focused pixels and related regression are pending.
- Latest candidate runner SHA256
  `a2899d9b5b9154c95c176b37164304507121e142e16e623f28e8de5cc5494380`
  passes enclosing5769 batch8/8. Related96 regression is95/96, with a genuine
 263px regression in5689's text-indent-013 (previous exact zero). The float-side
  first band discarded the inline leading margin/indent already projected by
  DOM. Candidate resolution now preserves that first-fragment displacement
  while continuation boxes restore the containing line; qualification pending.
- Extra single-case starts1358/1359/1361/1363/1371/1372/1374/1389 pass5/8.
  Float-root1371 fails179px, float-table-align-left-quirk1372 fails105px,
  floats-placement-vertical-0031389 fails2640px. These were historically PASS
  but require a direct3ea871c baseline comparison before assigning cause.
  A separate clean baseline worktree shares the current copied Cargo.lock;
  `--locked --offline` build is running. Its initial unlocked build was stopped
  and is not valid comparison evidence. Candidate binary is retained under
  `target/wpt-targeted/w3cos-wpt-float-text-flow-candidate-v1`. No commit/push.
- Direct clean3ea871c baseline (copied identical lockfile, locked/offline build)
  runner SHA256
  `2f2e69ad67517275556a147bd631e638730bdb849435a3ecbc6dd5796f4af2c6`
  also fails1371/1372/1389 at identical179/105/2640 pixels. They are pre-existing at the
  immediate commit baseline, not newly introduced by this float-text change.
  The exact013 index is5693 (the initial extra5691 baseline is a different
  passing control, not evidence for013).5693 baseline passes exact zero,
  confirming the candidate013 regression. The corrected candidate build is
  running. Full6548 zero-failure evidence is still absent.
- Corrected runner SHA256
  `c37d3b1757a1e9abf7ce30bda631be6bc6bdce50adcd1385d47576fba390c9e1`:
  `case-5693-float-text-first-margin-v1` restores013 from263 to exact zero;
  enclosing `batch-5769-float-text-first-margin-v1` remains8/8 PASS. Related
  eight-case starts5761/5753/5745/5737/5729/5721/5689/5681/5665/5545/5553/5593
  under `batch-<start>-float-text-first-margin-v1` pass96/96 (94 zero-pixel
  matches and two expected mismatches). Revision/800x600/zero tolerances remain
  unchanged. Extra eight float controls still pass5/8, with the identical
  pre-existing179/105/2640px failures proved by the direct baseline. No new
  failure remains in these observed ranges. This is scoped Skia qualification,
  not full float conformance, CPU/GPU acceptance or final6548 zero-failure proof.

### Root float shrink-to-fit and block end alignment

- Immediate baseline `c548231` reproduces `float-root.html` (index1371)
  at179 differing pixels, max255, with zero allowances. Its root float was
  still800px wide; the reference body's21.328125px right float remained at
  the left edge. Resolve an auto-width floated root through the same
  shrink-to-fit sizing as other floats, place its margin box against the
  viewport end, and align right floats in a block's content box using
  overlapping right-float edges. Descendants move with their float.
- Fresh optimized library build passes the previously RED
  `right_float_aligns_to_the_containing_block_end` (x0 becomes x50) and the
  new `floated_root_shrink_wraps_and_aligns_its_margin_box` (50px root,
  asymmetric8/12px margins, x738 in an800px viewport).
- First candidate runner SHA256
  `336c375ce0ce9fd3c1add6b804b07bee8fb4b93ae6aafa8e2a744be93facab8c`
  passes1371 at exact zero pixels. Receipts:
  `target/wpt-targeted/case-1371-root-float-before-v2/results.json` and
  `target/wpt-targeted/case-1371-root-float-after-v2/results.json`.
- Text-layout tests pass28/28. The broader float filter is28PASS/2FAIL;
  paint-artifact tests are32PASS/2FAIL. All four failed tests also fail in
  the preserved older1178-test executable, but it has different build
  flags and is not a same-SHA immediate baseline qualification. Keep these
  failures open. Index1389 remains2640px, unchanged from the verified WPT
  baseline. This is a focused root-float repair, not full float conformance
  or final6548 zero-failure proof.
- First candidate related regression passes104/104:100 exact zero-pixel
  matches and four expected mismatches, under `batch-<start>-root-float-v2`
  at starts5769/5761/5753/5745/5737/5729/5721/5689/5681/5665/5545/5553/5593.
  Extra controls1358/1359/1361/1363/1371/1372/1374/1389 improve5/8 to6/8;
  the remaining105/2640px differences are unchanged. Before publication,
  preserve relative offsets, resolve float percentage margins against
  parent content width, and exclude positioned roots from float sizing.
  A relative right float with10px parent padding,10% margin and5px left
  offset receives a separate focused test; final-build verification follows.
- Final optimized build passes right-float filter4/4, root shrink-fit1/1
  and text-layout28/28. Final runner SHA256
  `13eecdbd8bcd19b8389b8491ab33ce7974cd7f16ee160f88cf116c480e4c7d5d`
  repeats1371 at exact zero, related104/104 (100 zero-pixel matches and four
  expected mismatches), and extra6/8 with identical105/2640px open failures.
  Final receipts use `batch-<start>-root-float-final-v3/results.json` and
  `case-<index>-root-float-final-v3/results.json` below
  `target/wpt-targeted`. No fixture, suite, viewport or allowance was changed.

### Block table float avoidance

- Baseline `c8feaf0`, index1372 `float-table-align-left-quirk.html`, fails
  with105 differing pixels (max255, zero allowances). Raw layout diagnostics
  show the third group's normal table at y108.8, overlapping its leading
  float, while the reference places it at y134.0. The zero-width parent's
  height also shrinks incorrectly from50.4 to25.2.
- New `block_table_avoids_a_float_and_keeps_parent_height_when_it_cannot_fit`
  is RED before the production change: expected y20, actual y0. It also
  checks a fitting50px table beside a20px float in a100px content box.
- Include block tables in float avoidance, test the available exclusion
  band before pulling an atomic/BFC box upward, and shift fitting boxes
  into the remaining horizontal band. Use parent content edges, preserve
  relative inline offsets, and leave ordinary visible-overflow blocks
  eligible to overlap floats. Rejecting an upward move preserves the
  parent height rather than applying a stale negative flow correction.
- Baseline receipt:
  `target/wpt-targeted/case-1372-table-avoid-before-v1/results.json`.
  Preserve immediate baseline runner
  `target/wpt-targeted/w3cos-wpt-table-avoid-baseline-c8feaf0` (SHA256
  `13eecdbd8bcd19b8389b8491ab33ce7974cd7f16ee160f88cf116c480e4c7d5d`).
- Same-feature optimized RED executable (c8 production plus only the new
  test) is preserved at
  `target/wpt-targeted/w3cos-runtime-table-avoid-red-c8feaf0`.
  Its float filter is29PASS/3FAIL; the candidate is30PASS/2FAIL, with the
  new table test GREEN and the two other failures unchanged. Paint-artifact
  tests remain32PASS/2FAIL at both immediate baseline and candidate;
  text-layout tests pass28/28. Keep the four unchanged unit failures open.
- Neighboring immediate baseline batches1366 and1385 pass5/8 and0/8;
  receipts `batch-<start>-table-avoid-baseline-c8/results.json` below
  `target/wpt-targeted`. They contain11 existing failures, not new candidate
  failures. Final WPT qualification is pending; full6548 zero-failure proof
  remains absent.
- Initial candidate runner SHA256
  `2be198eca14cfe2602b1b09dc6ed555a39889f68bea8f9fc1e6ac59614be59bd`
  passes1372 at exact zero, receipt
  `target/wpt-targeted/case-1372-table-avoid-initial-v1/results.json`.
  Before publication, clamp only positions outside the available band
  rather than replacing every fitting position with a band-edge alignment.
  Add a separate auto-margin centering test, including a5px relative offset;
  keep this final guard's qualification separate from the initial receipt.
- Resolve atomic/BFC horizontal margins against parent content width in
  both the upward-fit check and horizontal avoidance. The generic
  `margin_lengths()` resolves percentages to zero and em against16px;
  it is not sufficient for this used-layout calculation. Extend the first
  table test with a10% left margin (x30 beside a20px float in100px).
  The earlier centering-guard compile was superseded and deliberately
  stopped (owned rustc SIGTERM), not a semantic test failure; the combined
  final code is being verified from a new build.
- Combined final library build passes both new table tests. The broad
  `block_table_` selector also includes the existing wrapper-em-margin test:
  it fails32 versus16 at both the direct c8 production baseline and final
  candidate; keep it open rather than labeling the selector all-green.
  Final float filter is31PASS/2FAIL (the two unchanged failures), and
  text-layout remains28/28. Final WPT runner rebuild is pending.
- Final runner SHA256
  `53746f1c9b737e78203b7ae841df88225116ef1abcbed2dfba2946e424180b83`:
  1372 passes exact zero. Related starts5769/5761/5753/5745/5737/5729/5721/
  5689/5681/5665/5545/5553/5593 pass104/104 (100 zero-pixel matches and four
  expected mismatches), receipts `batch-<start>-table-avoid-final-v2/results.json`
  below `target/wpt-targeted`. Neighbor batch1366 improves5/8 to6/8 only
  through1372 (105 becomes0); batch1385 stays0/8. All ten other neighboring
  failures retain identical pixel differences and statuses. Keep them open;
  no new failure is observed in these compared ranges. This is focused
  Skia qualification, not final6548 zero-failure proof.
- Additional controls1358/1359/1361/1363/1374 remain PASS, receipts
  `case-<index>-table-avoid-final-v2/results.json`. Together with1371/1372
  and1389 in the compared neighbor batches, the original eight extra float
  controls now pass7/8; only1389 remains2640px in that selected control set.

### Grouped float stale preceding-line marker

- Immediate baseline98bfb7b reproduces index1389
  `floats-placement-vertical-003.xht` at2640px. Its anonymous float group
  starts at y14, but the marked blue float starts at y20; the reference's
  blue box starts at y14. A marker inherited before grouping still reserves
  one6px line despite there being no preceding inline sibling in the float's
  new parent. A separate right-float ordering issue remains: the source's
  right yellow box starts at y144, versus reference y114.
- New `grouped_marked_left_float_does_not_add_another_inline_line` is RED
  before production changes: float y12 versus group y6. Preserve that
  same-feature executable at
  `target/wpt-targeted/w3cos-runtime-grouped-float-red-98bfb7b`.
  Only reserve an extra preceding line when actual same-parent inline
  content exists; do not treat the retained marker alone as a line box.
- Immediate baseline runner is preserved at
  `target/wpt-targeted/w3cos-wpt-grouped-float-baseline-98bfb7b`;
  baseline receipt `case-1389-grouped-float-before-v1/results.json` below
  `target/wpt-targeted`. Qualification is pending. This stage does not claim
  that1389 or the final6548 suite is closed.
- Optimized same-feature library qualification: new grouped-marker test
  passes. Float filter improves31PASS/3FAIL at the preserved immediate
  baseline to32PASS/2FAIL; the two other failures are unchanged.
  Text-layout remains28/28. WPT runner rebuild and pixel qualification
  are pending; do not infer the remaining right-float fix from this unit.
- Fresh runner SHA256
  `2b0721ff8a9534e6584007a04921848930862341c253f2cfd38597cbebe42d35`:
  1389 improves2640px to1800px, still FAIL with zero allowances. Raw layout
  confirms the blue float now starts at y14 (reference y14), while the right
  yellow float still starts at y144 (reference y114). Receipt
  `case-1389-grouped-float-after-v1/results.json` below `target/wpt-targeted`.
- Related eight-case starts5769/5761/5753/5745/5737/5729/5721/5689/5681/
  5665/5545/5553/5593 pass104/104 (100 exact zero-pixel matches, four expected
  mismatches), under `batch-<start>-grouped-float-after-v1`. Neighbor batches
  1366/1385 remain6/8 and0/8; their only pixel change is1389's improvement.
  No new failure is observed in these compared ranges. Additional controls
  1358/1359/1361/1363/1374 remain PASS. This is a qualified partial repair,
  not a claim that1389 passes or that the final6548 suite is complete.

### Anonymous float group continuation

- Baselinee8d6b1e leaves1389 at1800px: the right float starts at y144
  instead of reference y114. Grouping left floats creates a Flex row with
  no identity, so the surrounding block mistakes its whole130px height
  for normal-flow advancement.
- New `right_float_shares_anonymous_group_last_row_but_not_a_real_flex_box`
  is RED at the immediate baseline: anonymous=true gets y130 instead of
  y100, while the real-Flex counterexample passes. Preserve its executable
  at `target/wpt-targeted/w3cos-runtime-float-group-red-e8d6b1e` and baseline
  runner at `target/wpt-targeted/w3cos-wpt-float-group-baseline-e8d6b1e`.
- Mark generated non-BFC float groups explicitly. Their direct floats
  contribute exclusions/source-order bounds to the surrounding block;
  the generated row does not become the preceding normal-flow box.
  Subsequent right floats search the remaining band at or below the last
  preceding float top and real normal-flow end, honoring clear boundaries.
  Real Flex boxes and clearance-created floated BFC rows are not imported.
- Extend the focused test with5px relative offsets and clear:both. Keep
  relative paint shifts separate from imported margin-box exclusions.
  An earlier draft compile was explicitly stopped after these safeguards
  superseded its source snapshot; the combined source is being verified.
  Final qualification and full6548 zero-failure proof remain pending.
- Type checking caught an unsupported `LayoutRect::default()` fallback;
  replace it with explicit zero fields before qualification. Also require
  actual visible, non-positioned float data before setting imported/pending
  group state. A hidden-group counter checks that following floats retain
  the parent's10px padding. These safeguards superseded one draft build;
  owned obsolete compiler processes were deliberately stopped, not passed.
- The combined anonymous selector is9PASS/2FAIL: hidden-group counter
  passes, while the new real-Flex relative-offset case loses its5px shift
  (y130 versus135) in the generic preceding-box float projection. Preserve
  relative shifts when deriving that position. Marked floats following
  actual inline content keep their extraction-stage line anchor instead
  of being pulled above the established line by that generic projection.
  The other selector failure (anonymous inline negative-margin wrapping)
  also fails in the preserved immediate production baseline and remains open.
- Final optimized same-feature library build completes in4m36s. The
  anonymous-versus-real-Flex test passes all six clear/relative variants;
  the hidden-group padding counter passes. Float selector is35PASS/1FAIL:
  the existing inline-following float line-anchor failure now passes,
  while leading-float/normal-flow margin expectation24 versus80 remains
  unchanged and open. Text-layout remains28/28. The existing anonymous
  negative-margin wrapping counter still fails10 versus20; do not infer
  complete float or inline layout coverage. Fresh WPT pixels are pending.
- Fresh runner build completes in2m06s, SHA256
  `88fc943ee2251638d546bd9e35ea31aa0831947ef98cbbde18ecef8ac0117a17`.
  DOM float selector passes11/11. Zero-allowance WPT1389 passes1800->0px
  and1392 `floats-placement-vertical-004.xht` passes800->0px. Neighbor
  batch1385 improves0/8 to2/8; batch1366 remains6/8. All eight remaining
  neighbor failures retain identical pixel differences and statuses.
  Receipts: `case-1389-anonymous-group-final-v1/results.json` and
  `batch-<1366|1385>-anonymous-group-final-v1/results.json` below
  `target/wpt-targeted`.
- Related eight-case starts5769/5761/5753/5745/5737/5729/5721/5689/5681/
  5665/5545/5553/5593 remain104/104PASS:100 exact zero-pixel matches and
  four expected mismatches. Ordered paths, statuses and full pixel-diff
  objects are unchanged versus immediate baseline receipts. Final receipts
  use `batch-<start>-anonymous-group-final-v1/results.json`. This qualifies
  a focused Skia repair, not all float sizing/margin/percentage semantics,
  GPU/device parity, or the final same-clean-SHA6548 zero-failure gate.

### Inline text line-bottom continuation

- Immediate productionc81c7e4 still fails1369 `float-nowrap-9.html` at96px.
  Raw source/reference layouts contain identical visible text but lower the
  nowrap run as InlineBlock versus Inline. Its following right float starts
  at y46.4 versus44.800003; the generic preceding-box float projection uses
  the Inline glyph-box bottom without the remaining half-leading.
- Add `float_after_inline_text_uses_line_bottom_not_glyph_bottom`:10px
  overflowing nowrap Inline text with20px line-height must retain5px
  trailing half-leading before an already-next-line float. The first draft
  compute test used short text; its3m08s build fails0 versus20 because the
  float can correctly fit alongside that text. This is an invalid regression
  expectation, not valid RED evidence, and is replaced rather than weakened.
- Isolate the continuation projection with an explicit160px text layout
  overflowing its100px container: glyph box y5/height10, next-line float
  y20. Preserve that float's line-top instead of pulling it to glyph-bottom15.
  Production projection includes trailing half-leading only for Inline text;
  existing upward-only correction does not force a same-line float downward.
  Real WPT1369's96px failure supplies the repair's RED pixel evidence.
  Revised unit build and fresh pixel qualification are pending. Keep1367 separate: it has a
  zero-width shrunken float and an extra anonymous whitespace node, not just
  this line-bottom discrepancy.
- Re-run the immediate production runner (SHA256
  `88fc943ee2251638d546bd9e35ea31aa0831947ef98cbbde18ecef8ac0117a17`):
  `case-1369-line-bottom-before-v1/results.json` retains96 differing pixels,
  max-channel255, both allowed thresholds0. Preserve the baseline binary
  at `target/wpt-targeted/w3cos-wpt-line-bottom-baseline-c81c7e4`.
- Revised optimized same-feature library build completes in3m45s; the
  isolated overflowing-line projection test passes. Float selector is
  36PASS/1FAIL (the unchanged leading-float/normal-flow margin expectation
  24 versus80); text-layout remains28/28. Fresh runner pixels are pending.
- Fresh runner completes in2m29s, SHA256
  `ff9126a1c7bbd289a92b3ccdfcbc2ad350ee65241301e58c3d985a3cd8bde4ce`.
  Neighbor1366 improves6/8 to7/8 through1369's96->0px zero-allowance PASS.
  Neighbor1385 remains2/8. Ordered paths/statuses/pixel counts confirm the
  other seven failures are unchanged. Receipts below `target/wpt-targeted`:
  `batch-<1366|1385>-line-bottom-after-v1/results.json`.
- Related eight-case starts5769/5761/5753/5745/5737/5729/5721/5689/5681/
  5665/5545/5553/5593 pass104/104,100 exact-zero matches plus four expected
  mismatches. Ordered paths, statuses and full pixel-diff objects match the
  immediate baseline. Receipts `batch-<start>-line-bottom-after-v1/results.json`.
  This closes one historical pixel failure in focused Skia evidence, not the
  remaining nowrap/float capabilities or final6548 same-clean-SHA gate.

### Floated widths in anonymous nowrap rows

- Production4bddd00 still fails1367 `float-nowrap-7.html` at2078px. Raw
  source/reference layouts lower the block's nowrap line to Flex and shrink
  the5ch right float from48px to0px beside overflowing text. The source also
  retains an anonymous extraction whitespace strut; keep that distinct issue
  open until pixel qualification rather than treating correct width as closure.
- New `floated_boxes_in_anonymous_nowrap_rows_do_not_flex_shrink` checks
  direct/nested left/right floats. RED at the immediate production baseline:
  direct left float has flex-shrink1 versus0. The first test compile catches
  a missing WhiteSpace enum qualification; fix that type error before RED.
- At the block/inline-block/list-item/table-cell float lowering boundary,
  preserve floats' used widths with flex-shrink0. Hidden nodes still return
  before mutation; authored Flex containers do not enter this lowering call.
  DOM selector and fresh WPT pixel qualification are pending; no full6548
  or complete nowrap/float acceptance is claimed.
- Width-only candidate: DOM selector12/12PASS, runner2m42s; neighbor
  starts1358/1366/1385 pass8/8,7/8,2/8. The source float width is correctly
  restored0->48px, but1367 worsens2078->3134px because its extra extracted
  space shifts the source float10.664px and alters its baseline. No commit
  is qualified by this intermediate result. Receipts below
  `target/wpt-targeted`: `batch-<start>-float-shrink-after-v1/results.json`.
- A nowrap inline run already owns its unbroken-line strut. Suppress the
  extra extraction whitespace only for nowrap; ordinary wrapping extraction
  keeps its existing static-line strut. Extend the nested-right counter to
  require no anonymous single-space node. Combined qualification is pending.
- Combined nowrap-strut candidate: DOM12/12PASS, runner2m26s. Neighbor
  starts1358/1366/1385 pass8/8,8/8,2/8;1367 now passes exact-zero pixels.
  Receipts `batch-<start>-float-shrink-strut-after-v1/results.json`. Before
  qualifying a commit, add `extracted_right_float_strut_uses_its_inline_line_context`
  with mismatched parent/float white-space values; the containing inline's
  line behavior, not the float's internal text behavior, owns this decision.
  Context-counter RED and final combined regression are pending.
- Context counter is RED: lineNoWrap/floatNormal incorrectly retains a
  space strut (true versusfalse). Pass the containing inline's nowrap state
  explicitly into nested float extraction, independently of the float's
  own white-space. Final DOM selector and fresh runner qualification pending.
- Final DOM float selector13/13PASS, optimized build23.88s, including both
  mismatched line/float white-space cases. Fresh final runner remains pending;
  the earlier zero-pixel result does not attest the final context-safe source.
- Final context-safe runner build2m21s, SHA256
  `fbdc89f52d6b4d2f5400eaf725e2c09498e643b348545890dec4fcb876f05ef9`.
  Neighbor starts1358/1366/1385 pass8/8,8/8,2/8.1367 improves2078->0px;
  the other six neighbor failures retain identical pixel values/statuses.
  Raw1367 layout confirms5ch float width48px and no extra whitespace leaf.
  Receipts `batch-<start>-float-width-context-final-v1/results.json` below
  `target/wpt-targeted`; raw frame `nowrap7-width-context-final.frame`.
- Related starts5769/5761/5753/5745/5737/5729/5721/5689/5681/5665/5545/
  5553/5593 pass104/104:100 exact-zero matches plus four expected mismatches.
  Ordered paths/statuses/full pixel-diff objects are unchanged versus the
  immediate baseline. Final receipts use the same float-width-context-final-v1
  suffix. This is one qualified historical failure repair, not all CSS float
  placement/nowrap semantics, CPU/GPU/device parity or final6548 closure.

### Overflow BFC float-height chain

- Immediate production4b37aa8 leaves1386 `floats-placement-vertical-001a.xht`
  at21033px. Raw reference paragraphs have overflow:auto yet height19.2px
  despite their50px floats; subsequent paragraph y51.2 versus source y82.
  Raw source paragraphs also center text at y36.800003 rather than reference
  y17.6, and marked left floats start at y46.800003 rather than y16. These
  are separate sizing/line-metric/float-placement discontinuities, not just
  a border/radius or paint tolerance issue.
- Add `overflow_auto_height_bfc_contains_float_and_advances_its_next_sibling`
  covering visible (non-BFC) versus auto/hidden/scroll, a48px float with8px
  bottom margin, and a following normal-flow sibling. First verify whether
  the primitive Block path reproduces the failure; if it passes, narrow the
  discontinuity to the anonymous Flex inline-lowering path shown in the raw
  WPT layout rather than changing an already-correct primitive.
  Library RED qualification is pending; production behavior is unchanged.
  Raw frames below `target/wpt-targeted`: `vertical001a-actual-4b37aa8.frame`
  and `vertical001a-ref-4b37aa8.frame`. No final6548 closure is claimed.
- Code evidence narrows the reference discontinuity to
  `resolve_float_text_layouts`: its used-height was only text-line count plus
  edges, and `project_float_text_layouts` propagates that reduction to siblings
  and ancestors. Preserve the maximum float margin-box bottom plus bottom
  edges when the marked authored-block flow establishes a BFC (overflow,
  floated/positioned, or root); visible non-root flow still ignores floats
  for normal-flow auto height. Add a separate anonymous-flow reflow counter
  with a following sibling. The still-running primitive-test build predates
  this source/test extension; fresh combined qualification is required.
- Immediate baseline runner reproduces1386 FAIL at zero allowance in
  `case-1386-bfc-flow-height-before-v1/results.json`; preserve binary at
  `target/wpt-targeted/w3cos-wpt-bfc-flow-height-baseline-4b37aa8`.
  No new production renderer has been built or accepted yet.
- Primitive baseline library build4m33s is RED: visible/non-BFC control
  passes, overflowAuto container height0 versus expected56 fails before
  hidden/scroll variants execute. Preserve executable at
  `target/wpt-targeted/w3cos-runtime-bfc-height-red-4b37aa8`. This confirms a
  second general BFC-height discontinuity outside the marked text reflow:
  the resolver change alone cannot close the added primitive test. Complete
  BFC float ownership, auto-height and following-flow propagation together
  before qualifying the next candidate; neither primitive nor WPT is closed.
- Generalize positioned-only float-height containment to postorder auto-height
  BFC containment on both compute paths. Owned float scans stop at nested BFCs
  and ignore hidden/out-of-flow principal boxes. Resolve float bottom margins
  and padding before height growth; retain max-height/min-height used-box
  constraints. Translate following ordinary-flow sibling subtrees when an
  in-flow BFC grows and propagate its parent's auto-height delta; positioned
  and floated owners do not consume outer normal-flow height.
  Non-vertical parent extent growth is not a full Flex/Grid track re-solve;
  do not infer that broader capability from these focused tests.
  The combined BFC library selector is being compiled; no combined GREEN
  or new WPT renderer proof exists yet.
- Combined optimized library3m57s: BFC selector3/3PASS, including primitive
  overflow and anonymous text-reflow/sibling counters plus existing positioned
  nested-float containment. Broad float selector38PASS/1FAIL: the unchanged
  leading-float/normal-flow margin expectation24 versus80. Text-layout28/28.
  Fresh native runner build is in progress; pixel qualification remains open.
- Fresh runner2m10s SHA256
  `49c11fa625d22707f0db58a02270a3fa7e5fd68efbbbc1b7165d258fcecc65cc`:
  starts1358/1366/1385 remain8/8,8/8,2/8.1387 improves15030->12856px and
  1388 improves15444->13454px, while1386 worsens21033->30518px. Source
  first paragraph now height80.8 because its50px float still starts30.8px
  below the paragraph's line top; that incorrect placement is now reflected
  in its correct containment rather than hidden by a too-small used box.
  Receipts `batch-<start>-bfc-height-after-v1/results.json`. No commit is
  qualified by this intermediate worsening.
- Add `tall_float_after_inline_keeps_the_text_strut_and_float_line_top`,
  a50px float between16px inline runs in the marked baseline-aligned line.
  Require text at trailing-half-leading1.6px and float at line-top, with BFC
  height50px. Its RED build is pending; close line metrics/placement before
  final combined regression and commit qualification.
- Tall-float counter is RED in3m25s: text y20.800001 versus expected1.6,
  matching the source WPT's incorrect line offset. Preserve executable at
  `target/wpt-targeted/w3cos-runtime-tall-float-red-bfc-candidate`.
  In the marked anonymous-line model only, set floating child layout nodes
  to cross-start rather than baseline participation; ordinary Flex keeps its
  authored alignment. Add a genuine-Flex tall ordinary-item counter requiring
  its baseline offset to remain. New optimized qualification is pending.
- Optimized combined library3m24s: tall-float counter passes; the genuine-Flex
  baseline counter passes in the broad float selector40PASS/1FAIL (unchanged
  leading-float/normal-flow margin expectation24 versus80). BFC3/3 and
  text-layout28/28 remain green. Fresh runner pixels are pending; ordinary
  Flex is not converted into CSS float-line alignment by this repair.
- Fresh runner2m11s SHA256
  `8d947e0d784a2a492cf064cac33e2682cf767acd411e07cae0c8737767bde68f`:
  neighbor starts1358/1366/1385 remain8/8,8/8,2/8. Relative to immediate
  production baseline,1386 improves21033->20000px,1387 15030->10000px,
  1388 15444->13454px,1390/1391 each20016->20000px.1385 stays10000px;
  all five improved cases still FAIL at zero allowances, with no new PASS
  implied. Source first paragraph now height50/text y17.6/float y16/next
  paragraph y82, matching the corresponding reference geometry. Its remaining
  horizontal float/band discontinuities remain open. Receipts
  `batch-<start>-bfc-line-metrics-after-v1/results.json`; raw source frame
  `vertical001a-metrics-actual.frame` below `target/wpt-targeted`.
- Related eight-case starts5769/5761/5753/5745/5737/5729/5721/5689/5681/
  5665/5545/5553/5593 pass104/104 (100 exact-zero matches, four expected
  mismatches). Receipts `batch-<start>-bfc-line-metrics-final-v1/results.json`.
  Ordered paths, statuses and full pixel-diff objects match the baseline.
  This qualifies a partial BFC/line-metric repair, not closure of the five
  remaining pixel failures, full float semantics, or final6548 acceptance.

### Anonymous-line physical float edges (focused follow-up)

- Only marked CSS inline-formatting contexts project floats to their physical
  left/right exclusion-band edges; genuine Flex justification is unchanged.
  Single-text-leaf exclusion layout now accepts floats on either side of the
  leaf in source order, without moving their resolved vertical placement.
- Baseline case1386 remains RED at20000px. Raw reference RTL/right-aligned
  floats incorrectly had x289.5625. A fresh headless Chromium800x600 geometry
  check of the unmodified local upstream reference and001c confirms six float
  origins `(8,16),(358,82),(8,148),(358,214),(8,280),(358,346)`.
- Optimized library build3m13s: the new anonymous-line/real-Flex counter passes;
  float selector41PASS/1FAIL (unchanged24-vs80 margin expectation), text-layout
  28/28 and BFC3/3 pass. Runner build2m14s, SHA256
  `ccc023f1c22643d90ab21089cd29dd9a67294b153835aa14ded3913f661c451b`.
- Neighbor starts1358/1366/1385 yield8/8,8/8,3/8. Case1387 (`001b`) improves
  10000->0px and now PASS;1386 (`001a`) improves20000->1910px but still FAIL.
  1388 (`001c`) changes13454->13558px, still FAIL: its nested right floats
  remain incorrectly owned by inner inline geometry (x43.546875/325.10938,
  not browser358). Correcting the shared reference is not a closure of that
  source-side error.1385/1390/1391 remain10000/20000/20000px;1389/1392 stay0.
  Receipts `batch-<start>-float-physical-edges-after-v1/results.json`.
- Related eight-case starts5769/5761/5753/5745/5737/5729/5721/5689/5681/
  5665/5545/5553/5593 pass104/104 (100 zero-pixel matches, four expected
  mismatches), with ordered paths/statuses/full pixel-diff objects identical
  to the preceding BFC receipts. New receipts use suffix
  `float-physical-edges-final-v1`. No tolerance, upstream fixture or suite
  changes. Multi-run/nested-inline ownership, full margin/relative-float
  packing and final same-SHA6548 acceptance remain open.

### Float-band text alignment and extraction struts

- At clean baselinea21d597,1386 (`001a`) is RED at1910px. Raw frame comparison
  isolates955 pixels each to the RTL/right-aligned left-float paragraphs.
  The shared full-line text painter incorrectly forced these Inline leaves
  to left alignment, despite their exclusion-band geometry representing a
  complete line rather than a fragment. Ordinary inline fragments still
  retain their no-double-alignment rule; marked float-band lines now honor
  the containing block's alignment.
- Fresh Chromium800x600 Range geometry of the unmodified upstream reference
  gives text origins58/8/339.5625/289.5625/339.5625/289.5625 and advance68.4375.
  Baseline native reference ink was at59/9/59/9/59/9, confirming its painting was
  also wrong. This is not merely a source/reference agreement repair.
- Paint-only intermediate build3m24s passes its alignment counter, but WPT
  neighbor starts1358/1366/1385 give8/8,8/8,3/8:1386 reaches0 while1387
  regresses0->1910px. Receipt suffix`float-line-alignment-after-v1` records
  that unqualified candidate. A synthetic extracted-right-float strut was
  being counted as a second real text leaf, preventing the shared text flow.
- DOM now marks only that internal strut; layout excludes the marked strut
  from real-text cardinality while preserving its line-height/static anchor.
  Authored whitespace is not excluded. New paired counter checks both paths,
  and the DOM extraction-context counter checks marker provenance.
  Final library3m38s: new counter PASS, float43PASS/1FAIL (unchanged24-vs80
  margin expectation), text-align4/4, text-layout28/28, BFC3/3. DOM build
  17.83s passes its strut counter and all13 float tests.
- Final runner2m12s SHA256
  `497870f652d2c27bce19a7d23358c753e7e329bbacd50e88dbfe34a86a7ae5f2`:
  neighbor starts1358/1366/1385 now8/8,8/8,4/8.1386/1387 both PASS0;
  1388 improves13558->13110px but remains FAIL;1385/1390/1391 remain
  10000/20000/20000px,1389/1392 stayPASS0. Receipts use suffix
  `float-line-alignment-strut-after-v1`.
- Related eight-case starts5769/5761/5753/5745/5737/5729/5721/5689/5681/
  5665/5545/5553/5593 pass104/104 (100 exact-zero matches, four expected
  mismatches), with ordered paths/statuses/full pixel-diff objects identical
  to a21d597. Receipts use`float-line-alignment-strut-final-v1`. No allowances,
  suite or upstream files changed. Nested inline float ownership, broader
  multi-run line layout and final same-clean-SHA6548 proof remain open.

### First nested right-float ownership and nowrap anchors

- Baselinee2c5638 case1388 (`001c`) remains RED at13110px. A new paired DOM
  regression is RED in20.64s: an initial right float in a wrappable Inline
  descendant is not extracted (outer count0 versus1). Ordinary static inline
  descendants share the outer context; atomic inline-block/flex/table and
  floated principal boxes retain descendant ownership. The collector now
  respects these boundaries and extracts first right floats in wrappable runs.
- The old static-line unit used InlineFlex as a synthetic inline model while
  requiring descendant hoisting. Correct that native unit fixture to Inline;
  keep its extraction/order assertions and add explicit atomic-box negative
  ownership checks. No upstream WPT file or suite fixture changed.
- Fresh Chromium800x600 probes show an Inline right float at outer x358,
  InlineBlock(width100) at its own x93.546875, and InlineFlex at x43.546875
  as its own item. These prove ownership boundaries, not full Flex parity.
- Intermediate runner2m05s gives neighbor starts1358/1366/1385 as7/8,8/8,5/8:
  1388 reaches0 but1363's mismatch falsely reaches0. Receipt suffix
  `inline-float-ownership-after-v1` preserves that unqualified regression.
  An initial float in an unbroken nowrap run must retain its source anchor,
  not move to the queue after the entire run. Preserve that anchor and add a
  paired nowrap counter. Chromium upstream3-ref/4 float y38/y23 confirms the
  two paths must differ (host monospace13px, not native font raster proof).
- Final DOM optimized16.89s passes15/15 float tests. Fresh runner2m05s SHA256
  `a14941d35db69106aac2221a8a1f072860dafda39bb23929b4600a785c1bf8c7`:
  starts1358/1366/1385 now8/8,8/8,5/8;1386/1387/1388 all PASS0;1363
  mismatch is restored at6986px.1385/1390/1391 still FAIL10000/20000/20000px;
  1389/1392 stayPASS0. Receipts use`inline-float-ownership-nowrap-after-v1`.
- Related eight-case starts5769/5761/5753/5745/5737/5729/5721/5689/5681/
  5665/5545/5553/5593 pass104/104 (100 exact-zero matches, four expected
  mismatches); ordered paths/statuses/full pixel-diff objects match e2c5638.
  Receipts use`inline-float-ownership-nowrap-final-v1`. Runtime unit binaries
  were not rebuilt for this DOM-only change; native runtime proof here is the
  fresh runner's focused WPT receipts. Broader source-order/atomic layout and
  final same-clean-SHA6548 acceptance remain open.

### Forced-break float struts and next-line exclusion bands

- Baselinef147a68 cases1390/1391 (`004-ref/ref2`) are RED20000px each.
  Raw first reference blue float is at(8,108), while Chromium800x600 puts
  both references and the source at(108,14), following the6px text strut.
  Forced-break projection incorrectly counted a100px float in line height.
- Float/positioned boxes no longer enlarge the text strut or line alignment
  width. Following floats retain the forced-break vertical source constraint
  without advancing the normal text cursor; physical band projection then
  resolves their horizontal edge. New isolated strut/cursor counter passes
  after an optimized3m17s build.
- Intermediate runner2m17s gives8/8,8/8,6/8 for starts1358/1366/1385:
  1390/1391 reach0 but1392 becomes FAIL20000px. Its source frame is byte-for-
  byte identical to baselinef147a68: correcting the reference exposed an
  existing false agreement, not a source change from forced-break projection.
  Receipts use`forced-break-float-strut-after-v1`.
- For marked left floats after inline text that did not fit the current line,
  try the next text line's exclusion band before jumping to earlier floats'
  bottoms. Existing same-line placement is retained. A paired100/110px test
  in a100px band checks both early fitting and required downward advancement.
  Final optimized library4m43s passes that counter; float45PASS/1FAIL,
  text-layout28/28, BFC3/3. Forced-break selector6PASS/2FAIL: the two old
  counters still show40-vs19.2 and16-vs200, exactly reproduced with the saved
  pre-change `w3cos-runtime-tall-float-red-bfc-candidate`. The old24-vs80
  margin failure also remains. None of these failures is relabeled PASS.
- Final runner2m20s SHA256
  `b3066b5695b1722d4e77d753c9ee8495829b20217248b6c436e2c6eeab0a1321`:
  starts1358/1366/1385 now8/8,8/8,7/8.1390/1391/1392 all PASS0;1386/1387/
  1388/1389 stayPASS0.1385 (`008`) remains FAIL10000px. Native and browser
  source colored boxes now share(8,8,100,100)/(108,14,100,100). Receipts use
  `float-next-line-band-after-v1`; raw source`vertical004-source-next-band.frame`.
- Related eight-case starts5769/5761/5753/5745/5737/5729/5721/5689/5681/
  5665/5545/5553/5593 pass104/104 (100 exact-zero matches, four expected
  mismatches), with ordered paths/statuses/full pixel-diff objects identical
  to f147a68. Receipts use`float-next-line-band-final-v1`. No tolerance,
  upstream file or suite changes. This closes focused pixel comparisons,
  not full geometry: native source body/root heights remain200/216 whereas
  Chromium reports6/114. Stale auto-height after float relocation, broader
  packing/relative-offset closure and final same-clean-SHA6548 proof stay open.

### Right-float queues cannot cross later flow boundaries

- Baseline9c17d3d case1385 (`floats-placement-008`) is RED10000px: the direct
  right float after an InlineBlock is queued behind a later clear:both left
  float, incorrectly placing the right green box100px below the container.
  New paired DOM clear/normal-block boundary test is RED in20.78s (index1
  FloatNone versus requiredRight), preserving the source-order failure.
- Flush previously queued right floats before a subsequent cleared float or
  normal block/list/table/flex/grid box. Hidden and absolute/fixed boxes do
  not act as flow barriers. Inline-run coalescing remains within a segment,
  not across a later clearance/source-position constraint.
- Optimized DOM19.28s passes16/16 float tests. Fresh runner3m04s SHA256
  `24ffd2673ce3c4d293bb782b1213851139221056c59d72b444643cb4f27d32d7`:
  neighbor starts1358/1366/1385 all8/8;1385 improves10000->0px and the
  1386–1392 exact matches remain0. Receipts use`float-queue-boundary-after-v1`.
- Related eight-case starts5769/5761/5753/5745/5737/5729/5721/5689/5681/
  5665/5545/5553/5593 pass104/104 (100 exact-zero matches, four expected
  mismatches), with ordered paths/statuses/full pixel-diff objects identical
  to9c17d3d. Receipts use`float-queue-boundary-final-v1`. No WPT fixture,
  suite or tolerance changes; runtime unit binaries were not rebuilt for this
  DOM-only change, so runtime evidence is the freshly built focused runner.
- Browser source geometry places the right float at the container top and
  the cleared left float100px lower. Absolute browser y50 versus native51.2
  still differs through default normal-font line metrics; stale auto-height
  after relocation also remains open. This closes the focused historical
  float queue failure, not complete browser geometry, all historic failures,
  or final same-clean-SHA6548 acceptance.

### Shared-BFC floats outside their immediate containing block

- Baseline38982c7 fresh start1393 is3/8: cases1394/1396 are9500px,
  1397 is8500px and1398 is8160px RED. Local-parent exclusions miss
  earlier floats in the same BFC but outside the immediate containing block.
- Both layout entry paths now project these cross-parent float collisions
  against full margin boxes in source order, applying CSS2 float rules3/7
  and clearance while moving the complete floated subtree. Independent
  overflow/atomic BFCs and real flex/grid items retain separate ownership;
  hidden subtrees and ignored track-item floats do not create exclusions.
  Same-parent layout continues through the existing local float solver.
- Optimized runtime tests3m49s: new shared-BFC3/3, BFC6/6 and text28/28
  pass. Float tests48/49 retain only the known leading-margin failure24
  versus80. Forced-break6/8 retain the same known16 versus200 and40
  versus19.2 failures; these are not represented as passing gates.
- Fresh runner2m18s SHA256
  `ce37081b54b2ff80c31e666ad096d7657343f5a863aaf1ed37ca0f75a3eb0755`:
  start1393 is7/8; all four targeted failures become exact0px and prior
  passing cases remain passing. Case1400 retains25000px and requires a
  separate in-flow BFC avoidance repair. Neighbor starts1358/1366/1385
  pass24/24. Related starts5769/5761/5753/5745/5737/5729/5721/5689/
  5681/5665/5545/5553/5593 pass104/104 (100 exact-zero matches, four
  expected mismatches); ordered paths/statuses/full pixel-diff objects are
  identical to38982c7. Receipts use`shared-bfc-final-v1`.
- This is pinned normative WPT conformance, **not Chromium pixel parity**.
  [CSS2.2 §9.5.1](https://www.w3.org/TR/CSS22/visuren.html#float-position)
  scopes the placement rules to the shared BFC. In fresh800x600 source
  geometry, installed Chromium141.0.7390.37 places all four blue floats
  at y8, whereas the upstream references require y308. Browser x values
  are8/33/108/-17 respectively. Firefox/WebKit binaries are unavailable;
  cross-engine behavior is unverified. Preserve this standard-versus-browser
  acceptance discrepancy; do not change fixtures, suite or tolerances to
  erase it. Percentage-spacing/general geometry and final same-clean-SHA
  6548 acceptance are not proven by this focused batch.

### In-progress automatic BFC and table settlement qualification

- Base main `dbb06cb`; fixed WPT revision and 800x600 manifest are unchanged.
  HTML table width attributes enter the presentational-hint cascade before
  authored declarations. Consecutive synthetic left-float groups align their
  margin-box tops rather than descendant baselines.
- Auto-width overflow BFCs reflow their complete subtree into the available
  float band, preserving viewport units and the original containing-block
  basis for root percentage padding and width bounds. Settled auto-table
  heights propagate signed changes through ordinary flow ancestors/siblings;
  independent owned floats and minimum heights remain containment floors.
- Initial settlement omitted the separated table's outer bottom spacing:
  `float-table-align-left-quirk.html` regressed by338 strict pixels. Restoring
  that spacing returns it to exact0. The opt-level1 runtime unit build passes
  auto-table9/9, BFC8/8 and text-layout28/28; this is not the production runner
  optimization profile or a full runtime gate.
- Fresh default optimized runner takes2m42s, SHA256
  `8a43b64ccf7a659d1757ac56ef96ac03daad5a6e457400ef4e8558b820d47244`.
  Receipts `batch-1366-auto-bfc-spacing-v2` and
  `batch-1393-auto-bfc-spacing-v2` pass8/8 each.
  `batch-1400-auto-bfc-spacing-v2` passes1/8: case1400 becomes exact0;
  cases1401–1407 retain22500/20000/7500/35200/35200/18300/18300 pixels.
  The auto table wrapper still stretches beyond its intrinsic grid, and
  mixed-side/wrapping float cases need separate repairs. Neighbor starts1358/
  1385 pass16/16; the13 related eight-case starts listed in the preceding
  batch pass104/104 (100 zero-pixel matches, four expected mismatches).
- A focused real-flex-item negative test is RED: float-band reflow wrongly
  changes a300px flex item to200px. The new reflow now excludes genuine
  flex/inline-flex/grid parents while retaining anonymous inline/float
  formatting contexts. GREEN float-band qualification passes3/3 in2m57s;
  auto-table9/9, BFC8/8 and text-layout28/28 pass again. Float tests52/53
  retain the previously recorded leading-margin failure. Fresh production
  runner SHA256`b97dac5715f00907643bf4aff15a4ecddc9ea4dde49eb8e583e3117aaba6d9a2`
  finishes in4m43s including the unit-build lock wait. Eighteen eight-case
  receipts use`auto-bfc-track-guard-v3`:137/144 pass, and ordered paths,
  statuses and complete pixel-diff objects are identical to spacing-v2.
  The seven remaining failures are the explicitly listed1401–1407 cases.
  Opt-level1 RED compilation
  takes4m10s; do not claim it is a demonstrated build-speed improvement.
- This is focused candidate evidence, not final same-clean-SHA6548
  acceptance. Do not infer a current global remaining-failure count from it.

### Nested automatic table intrinsic-width qualification

- Base`223f35f`; static WPT revision/viewport/suite remain unchanged.
  The atomic/table shrink-fit branch omitted TableCell containing blocks.
  It now uses the existing intrinsic border-box sizing in cells as in blocks,
  rather than shrinking a painted wrapper after the table grid was laid out.
- New nested-table/float unit is RED300 versus150 under TableCell (3m27s),
  then GREEN with150px width and top-aligned placement (3m18s). Float-band
  tests3/3 pass. Broad table units75/81 retain six failures, all also present
  in the saved earlier optimized runtime test binary; that older binary is
  not an exact`223f35f` unit baseline. Do not label the broad unit gate green.
- Default runner build5m58s includes waiting for the unit-build artifact
  lock; SHA256`0b30c19dc45fe567270c7e6ce837b7d3b3989c031939eb664b5eab523905c514`.
  Receipt`batch-1400-table-cell-intrinsic-v1` passes2/8. Case1401 improves
 22500 to exact0, while1400 stays0. Cases1402–1407 retain20000/15000/35200/
 35200/18300/18300 pixels. Case1403 increases7500 to15000: native source
  purple150x50 is now at(8,8), but its native reference's second mixed-side
  float remains at(8,108). This is unresolved reference rendering, not a
  passing comparison. Seventeen neighboring/related eight-case receipts
  pass136/136; ordered paths/statuses/full pixel-diff objects are identical
  to`auto-bfc-track-guard-v3`. Overall18 batches pass138/144, not144/144.
- This focused repair does not prove final same-clean-SHA6548 acceptance.

### Mixed-side float top-band qualification

- Base`f88fc77`; ordinary Block/ListItem/TableCell sibling floats now try
  the highest available shared band before advancing below exclusions.
  Both physical sides use static margin boxes, source-order flow floors and
  clearance; relative visual offsets do not alter occupied float space.
  The complete floated subtree moves, not just its wrapper. Genuine flex/
  grid tracks and previously imported synthetic float groups keep their
  separate existing paths.
- New opposite-side unit is RED(100,100) versus(100,0) in3m23s, then GREEN
  in2m51s. It covers mirrored sides, insufficient space, clear and a relative
  predecessor. Float units54/55 retain only the known leading-margin failure;
  auto-table10/10, BFC8/8 and text-layout28/28 pass.
- Default optimized runner4m52s includes the unit artifact-lock wait,
  SHA256`c717e47ccbfb9f0f6d013957223823fe0c1000f17e4ac9a9b5734e520883a31e`.
  Receipts use`mixed-float-top-band-v1`. Start1400 improves2/8 to4/8:
  cases1402/1403 become exact0, while1400/1401 remain exact0. Cases1404–
  1407 retain35200/35200/36000/36000 pixels; right wrapping differences
  increase after correcting native reference float placement, not to PASS.
- Saved previous runner`w3cos-wpt-before-mixed-f88fc77` has SHA256
  `0b30c19dc45fe567270c7e6ce837b7d3b3989c031939eb664b5eab523905c514`.
  Fresh neighboring baselines1408/1416 are3/8 and1/8. Candidate results
  are4/8 and1/8; case1410 becomes exact0. The anonymous block BFC used by
  wrapping002 is internally Flex and remains304px wide, so it does not
  enter the current Block-only width reflow. Seventeen related/neighbor
  batches pass136/136 with ordered paths/statuses/full pixel-diff objects
  identical to`table-cell-intrinsic-v1`; neither new neighboring batch loses
  a previously passing case. Overall20 batches pass145/160 (15 FAIL), not
 160/160. This does not prove final same-clean-SHA6548 acceptance.

### Anonymous inline BFC subtree-reflow qualification

- Base`ddfc897`; the Block BFC float-band reflow also accepts Flex boxes
  marked as internal anonymous inline formatting contexts. These implement
  authored Block outer boxes; genuine Flex items remain protected by the
  existing parent-context guard. Root containing-block/viewport constraints
  and the complete wrapped subtree use the existing reflow machinery.
- New unit is RED300 versus200 width in3m28s, then GREEN in2m48s. It
  verifies200x100 bounds and two150x50 boxes on separate lines under both
  Block and TableCell parents, with inter-box whitespace. BFC9/9,
  auto-table10/10 and text-layout28/28 pass; float units55/56 retain only
  the known leading-margin failure. The initial test fixture used a nonexistent
  vertical-align field; it was corrected to the existing align-self mapping
  before obtaining the logical RED receipt.
- Default optimized runner5m22s includes unit-build lock waiting;
  SHA256`359b593e45de9f6ca308c2777b3ac4cb072bf73f9480ae5cfd99888e64d8759e`.
  Receipts use`anonymous-bfc-reflow-v1`. Start1400 remains4/8, but case1404
  improves35200 to400 pixels and1406 improves36000 to800. Cases1405/1407
  retain35200/36000. Do not represent the residual differences as zero.
- Fresh native1404 geometry identifies the residual constraint failure:
  authored outer table width300, internal row group/row/cell width304,
  resulting overflow BFC width204 beside the100px float. This requires
  proper constrained auto-track distribution, not clipping or a tolerance.
  Saved previous runner`w3cos-wpt-before-anonymous-bfc-ddfc897` has SHA256
  `c717e47ccbfb9f0f6d013957223823fe0c1000f17e4ac9a9b5734e520883a31e`.
  All20 batches finish145/160 PASS (15 FAIL), preserving every prior PASS.
  Only start1400's full path/status/pixel-diff report changes; the other19
  reports remain identical to`mixed-float-top-band-v1`. Final same-clean-SHA
 6548 acceptance is not established by this focused repair.

### Constrained automatic table-track contraction qualification

- Base`f73ad53`; maximum and minimum cell tracks share the existing
  column/row/span/collapsed-column collection path. When the declared grid
  is smaller than maximum content, contraction is distributed in proportion
  to each column's available max-minus-min space. Rigid columns do not
  contract, and insufficient declared width retains the minimum total.
  Existing growth/already-fitting paths do not perform the additional min
  collection. This is not clipping, a fixture change or a fuzzy allowance.
- New unit is RED305.33594 versus300 in2m49s, then GREEN in2m41s. It
  covers one flexible column, rigid350 content, flexible/rigid columns
  producing220+80 at width300, and their150+80 minimum at declared200.
  Table units76/82 retain the same six recorded failures; BFC9/9 and
  text-layout28/28 pass; float55/56 retains the known leading-margin failure.
- Default optimized runner4m50s includes unit-build lock waiting;
  SHA256`6f4ab2c3c225d51146c460ce84bc276ff6cf1184de6314337eb71b0e0191a5f5`.
  Receipts use`auto-track-contraction-v1`. Start1400 improves4/8 to6/8:
  cases1404/1406 become exact0 from400/800;1400–1403 remain exact0.
  Cases1405/1407 still fail35200 pixels each and require auto-width nested
  table band sizing and corresponding track reflow.
- Saved previous runner`w3cos-wpt-before-track-contraction-f73ad53` has
  SHA256`359b593e45de9f6ca308c2777b3ac4cb072bf73f9480ae5cfd99888e64d8759e`.
  All20 batches pass147/160 (13 FAIL), preserving every previous PASS.
  Eighteen full ordered path/status/pixel-diff reports remain identical to
  `anonymous-bfc-reflow-v1`. Besides1400, start1408 changes only failed
  caption-combination case1414 from45745 to52944 pixels; its full cause
  remains unqualified and it is not declared repaired. Final same-clean-SHA
 6548 acceptance is not established by this focused contraction repair.

### Auto table float-band reflow and real DOM wrapping qualification

- Base`aab1afa`; auto nested tables may reflow into the current float band
  only when their minimum border-box content fits. The minimum includes
  column tracks, effective spacing, outer borders/padding and captions.
  Forced HTML-table widths retain border-box treatment; unbreakable content
  continues below the float rather than being compressed or clipped.
- The new layout unit is RED300x50 versus200x100, then GREEN, covering
  spacing0/2 and rigid300 content. A runtime-only runner still fails both
  real002 table cases: DOM-generated table-cell inline rows were NoWrap
  and lacked the inline-formatting-context marker. The new real DOM unit
  reproduces NoWrap versus Wrap, then passes for normal/nowrap/pre after
  preserving inherited whitespace wrapping and adding that internal marker.
  This integration failure was not published as a successful repair.
- DOM table tests40/42 retain the two recorded failures. Integrated runtime
  table tests77/83 retain the same six failures; float56/57 retains the
  leading-margin failure. BFC9/9 and text-layout28/28 pass. Runtime table
  test compilation takes3m05s; the default optimized runner build takes
  5m00s including artifact-lock waiting, SHA256
  `2d9578ac5eeb2493355a4f8cb8cdcb5584de18912473ea718c27c18fe38eda28`.
- Receipts use`auto-table-band-dom-wrap-v2`, with the unchanged frozen WPT
  revision/suite and800x600 viewport. Start1400 improves6/8 to8/8:
  cases1405/1407 become exact0 from35200 pixels each. All20 selected
  eight-case batches finish149/160 PASS,11 FAIL, with no prior PASS lost.
  The other19 complete ordered path/status/pixel-diff reports are identical
  to`auto-track-contraction-v1`; the17 related/neighbor batches remain
  136/136 PASS. These are executions with overlapping batches, not160
  unique cases or a current global remaining count. Caption case1414
  remains52944 pixels; the remaining float/margin failures are not repaired.
  Final same-clean-SHA6548 acceptance remains unestablished.

### Intermediate float-release band qualification

- Base`51e7a72`; the first independent table/BFC searches float-bottom
  boundaries for a fitting band instead of testing only the initial band.
  Collision advancement releases the shortest overlapping float first.
  The new unit reproduces26px versus6px (RED2m37s), then passes all four
  left/right combinations (GREEN3m09s). Table78/84 and float57/58 retain
  the same six/one recorded failures; BFC10/10 and text-layout28/28 pass.
- Default optimized runner build5m27s includes the test-build lock wait;
  SHA256`f09ea15ef399140caa5cf6a063e2652bf75541abab44b3bcb567757f2d26acf6`.
  Receipts`partial-float-band-v1` use the unchanged pinned suite/revision
  and800x600 viewport. Case1416 becomes exact0 from5000 pixels; start1416
  improves1/8 to2/8. Start1400 remains8/8 exact0. Twenty eight-case batches
  finish150/160 PASS,10 FAIL, with no previous PASS lost; all18 reports
  outside starts1408/1416 retain identical ordered path/status/pixel diff.
  Batch executions overlap and are not a global remaining count.
- Case1412 improves44000 to30100 pixels but is not repaired: source
  tables now correctly fit at the shorter float's6px bottom with20px outer
  heights; the runtime-rendered reference still misplaces its float boxes.
  Case1413 worsens25707 to29277 and remains unqualified: its source table
  is now correctly below the20px float, but authored HTML height20 remains
  absent from its projected style (Auto,12px content), unlike the reference.
  These unresolved reference-float/HTML-height paths require separate fixes,
  not fixture/tolerance changes. Caption1414 remains52944 pixels. Final
  same-clean-SHA6548 zero-failure acceptance is still not established.

### Cascaded HTML table-height hint qualification

- Base`c69b6db`; HTML table height now enters the existing author-origin
  presentational-hint cascade before author declarations. Numeric/percent
  heights, zero and invalid input are covered, and authored height:auto
  overrides the hint. The existing zero-width behavior is unchanged.
  Rendering authority: [HTML tables](https://html.spec.whatwg.org/multipage/rendering.html#tables-2).
  DOM regression is RED Auto versus20px in18.73s, then GREEN18.71s;
  DOM table tests41/43 retain the same two recorded failures.
- Default optimized runner build2m35s includes a short test-build lock wait,
  SHA256`92a9ddb550924f5009b0a2bc3e8c62bfb924e489e14ce2f30d0aa078335dc54a`.
  Receipts`html-table-height-v1` use the unchanged pinned revision/suite
  and800x600 viewport. Twenty eight-case batches remain150/160 PASS,
 10 FAIL, with no lost PASS; only start1408's ordered path/status/pixel
  report changes. Related/neighbor batches remain136/136 PASS; executions
  overlap and do not represent unique cases or a global remaining count.
- Case1413 improves29277 to6000 pixels but still fails. Real source dumps
  confirm nested table heights20px and enclosing table heights40px, matching
  the reference dimensions. The remaining difference is the third source
  overflow BFC at x208 instead of x8 after its left float has ended:
  horizontal displacement still considers a non-overlapping float. That
  independent float-band correction needs its own regression/fix. No case
  is declared newly repaired by this height-only step; final clean-SHA6548
  zero-failure acceptance remains unestablished.

### Expired-float horizontal displacement qualification

- Base`cfcca41`; first overflow BFC horizontal displacement now uses only
  left floats intersecting its selected vertical band. A float that has
  already ended cannot push a below-float box sideways. Genuine flex/grid
  ownership and the existing active-float fitting paths are unchanged.
  Fresh WPT RED receipt`expired-float-red-v1` reproduces case1413 FAIL6000
  pixels before editing. The new regression passes in2m57s and covers
  Block/internal-IFC Flex with both50% and150px widths, expecting x0/y20.
  BFC11/11 and text-layout28/28 pass; table78/84 and float58/59 retain
  the same six/one recorded failures.
- Default optimized runner5m13s includes the test-build artifact-lock wait,
  SHA256`50401ae6354562708933a96482123e5df5568a1741734738fdc27f85b4b0fccc`.
  Receipts`expired-float-edge-v1` retain the frozen WPT revision/suite and
 800x600 viewport. Case1413 becomes exact0 from6000 pixels; start1408
  improves4/8 to5/8. Start1400 remains8/8 exact0 and start1416 remains2/8.
  Twenty eight-case batches finish151/160 PASS,9 FAIL, with no lost prior
  PASS. Only start1408's full ordered path/status/pixel-diff report changes;
  the17 related/neighbor batches remain136/136 PASS. Execution counts
  overlap and are not a current global remaining count.
- Cases1412/1414/1415 and the six selected margin failures remain open.
  In particular1412's runtime-rendered reference still has incorrect float
  positions: internal anonymous float groups bypass the ordinary float
  context path. This needs a separate generic float fix, not a reference
  or tolerance change. Final same-clean-SHA6548 acceptance remains open.

### Anonymous float-group ordinary formatting qualification

- Base`d330313`; internal Flex carrying the exact anonymous-float-group
  marker now participates in ordinary float positioning. Genuine authored
  Flex remains excluded. The new regression is RED(0,20) versus(100,6)
  for the third float in3m18s, then GREEN3m06s; its unmarked genuine Flex
  control retains(0,20). A fixture type-alias compile error was corrected
  before this logical RED and is not counted as a reproduced layout bug.
  Table78/84 and float59/60 retain the same six/one failures; BFC11/11
  and text-layout28/28 pass.
- Default optimized runner build5m15s includes the unit-build lock wait,
  SHA256`d063d6a02e5dd3402f30a51bccc7279880fa2516b1f9439c45ab201828fe5aa9`.
  Receipts`anonymous-float-group-v1` use the unchanged frozen revision/suite
  and800x600 viewport. Twenty eight-case batches remain151/160 PASS,
 9 FAIL; no previous PASS is lost, and only start1408's ordered
  path/status/pixel-diff report changes. The17 related/neighbor batches
  remain136/136 PASS. Counts overlap and are not a global remaining count.
- Case1412 improves30100 to20100 pixels, without becoming PASS. Reference
  dumps confirm its first two float groups now correctly release space at
  the6px boundary. The remaining mixed cases have an anonymous group's
  original vertical position below an external right float, and subsequent
  shared-BFC float positions still differ. This needs a separate generic
  cross-group placement fix. Case1413 retains exact0; caption1414 remains
 52944. No additional complete WPT case is claimed repaired by this step;
  final same-clean-SHA6548 zero-failure acceptance is still open.

### Cross-group float static-top qualification

- Base`55825f1`; an anonymous float wrapper no longer uses preceding
  out-of-flow float height as its static starting position. Its initial
  top is bounded by previous normal content and float source-order tops;
  shared-BFC collision/clearance still decides individual float positions.
  A direct simple/shared projection regression is RED(0,20) versus(0,0)
  for the first grouped left float in2m28s, then GREEN2m45s; the later
 150px float correctly moves to(0,6) beside the external right float.
  Table78/84 and float60/61 retain the same six/one known failures;
  BFC11/11 and text-layout28/28 pass.
- Default optimized runner4m42s includes the unit-build lock wait,
  SHA256`f99fedf583fff131174e6cccf5ed98bfd7f57893c615867d076eebf19c32fd2e`.
  Receipts`cross-group-static-top-v1` retain the frozen revision/suite and
 800x600 viewport. Case1412 becomes exact0 from20100 pixels, improving
  start1408 from5/8 to6/8. Start1400 remains8/8 exact0;1413 retains exact0
  and start1416 remains2/8. Twenty eight-case batches finish152/160 PASS,
 8 FAIL with no previous PASS lost. Only start1408's full ordered
  path/status/pixel-diff report changes; the17 related/neighbor batches
  remain136/136 PASS. Execution counts overlap and are not global counts.
- Caption1414 remains52944 pixels,1415 remains17650, and the six selected
  margin failures remain open. No clean-SHA full6548 zero-failure claim
  follows from this focused cross-group repair.

### Post-float-avoidance automatic BFC height qualification

- Base`a31cfd3`; after an upward float-flow correction, automatic BFC
  height now settles against its last normal-flow child's final static
  margin edge, with parent bottom padding/border. A later downward float
  avoidance must not leave that child outside a stale contracted height.
  Negative trailing margins and relative visual offsets remain accounted
  for, rather than treating every painted descendant as in-flow content.
  The new cell regression is RED14px versus15px in2m28s, then GREEN2m29s.
  Table78/84 and float61/62 retain the same six/one recorded failures;
  BFC12/12 and text-layout28/28 pass.
- Default optimized runner4m37s includes the unit-build artifact-lock wait,
  SHA256`de607c1f2b50a4369b02e9d12b8adf313ad734fcf762780ec7ca406da162f93c`.
  Receipts`final-bfc-auto-height-v1` retain the frozen revision/suite and
 800x600 viewport. Case1415 becomes exact0 from17650 pixels, improving
  start1408 from6/8 to7/8. Start1400 remains8/8 exact0;1412/1413 retain
  exact0 and start1416 remains2/8. Twenty eight-case batches finish
 153/160 PASS,7 FAIL with no previous PASS lost; only start1408's full
  ordered path/status/pixel-diff report changes. The17 related/neighbor
  batches remain136/136 PASS. Execution counts overlap and are not a
  current global remaining count.
- Caption1414 remains52944 pixels and the six selected margin failures
  remain open. This focused repair is not final clean-SHA6548 acceptance.

### Cleared-float intrinsic wrappers and float-only BFC height settlement

- Base `c48cb6f`: cleared synthetic floated wrappers now use automatic
  intrinsic width instead of excluding the entire parent with `100%`.
  The DOM regression is RED (percentage versus automatic width), then
  GREEN; table 41/43 and float 17/18 retain their recorded failures.
  Automatic float-only BFC height now uses final occupied float extents,
  preserving explicit minimum height. Its regression is RED 90px versus
  76px, then GREEN, including the 100px minimum-height control.
  Runtime table 78/84 and float 62/63 retain the same six/one failures;
  BFC 13/13 and text-layout 28/28 pass.
- Optimized runner SHA256
  `b64cb47b02c5172d9a5e8958dd15d89c80939ac4f5e18dced573444641a8f5a1`.
  Receipts `float-only-height-settlement-v1` use the pinned revision,
  frozen suite and 800x600 viewport. Twenty eight-case batches finish
  153/160 PASS, 7 FAIL, with no prior PASS lost; only start 1408's full
  ordered path/status/pixel-diff report changes. The 17 neighboring
  batches remain 136/136 PASS. These overlapping executions are not a
  global remaining count.
- Caption 1414 improves from 52944 to 5460 differing pixels, but remains
  FAIL. The unpublished wrapper-only intermediate worsened it to 67294;
  final float-only height settlement removes that stale-height mismatch.
  The remaining differences are two table-grid background strips when
  the caption is wider than the grid. Six selected margin failures remain
  unchanged. Neither this partial repair nor its push proves full6548
  clean-SHA zero-failure acceptance.

### Caption-wide wrapper versus table-grid background qualification

- Base `e99adf2`: table background now excludes the horizontal extent
  contributed only by a wider caption, using the widest laid-out direct
  row/group and retaining table padding, borders and separated spacing.
  This changes the shared PaintArtifact path consumed by both raster
  backends, not caption layout or application CSS. Missing grid data keeps
  the old width; collapsed-border horizontal painting is unchanged.
  The new top/bottom caption regression is RED 192px versus 100px in
  2m42s, then GREEN with the existing vertical-inset test in 2m47s.
  Paint-artifact tests finish 33/35; the two failures also reproduce in
  the saved older executable (32/34), with the same previously recorded
  positioned-subtree and inline-fragment assertions.
- Default optimized runner finishes in 5m00s including the unit-build
  lock wait; SHA256
  `bb8ab39badf23eb38b9e13f90976a9b3fb8fb53fc61c9364e24b34da149937e4`.
  Receipts `caption-grid-background-v1` retain the pinned revision,
  frozen suite and 800x600 viewport. Caption 1414 becomes exact zero
  from 5460 pixels; start 1408 is now 8/8 PASS. The original twenty
  eight-case batches improve to 154/160 PASS, 6 FAIL. Only start 1408's
  full ordered path/status/pixel-diff report changes; no old PASS is lost.
- Additional starts 5268/5276/5284 reproduce the pushed-base 21/24 PASS
  with identical full reports: caption-position-001 (876 pixels) and
  collapsing-border-model-003/009 (2400 each) remain FAIL. All 23 batches
  finish 175/184 execution PASS, 9 FAIL; overlapping executions are not
  a current global remaining count. The six selected float-margin
  failures remain unchanged; final clean-SHA full6548 acceptance is open.

### Flow-root float-band and genuine-flex repair

- Based on `59537d745265da90708e041ee0ff4c204197570e`, preserve
  `Display::FlowRoot` through CSS declaration round-trip, DOM float fixup,
  static codegen and runtime block/BFC mapping. This is an engine change,
  not application CSS or a changed upstream fixture.
- Auto-width BFCs use physical float bands, overlapping same-side margins
  rather than adding them again. Small negative margins cannot enlarge the
  float band; sufficiently negative margins force clearance. The minimum
  padding/border box is not added twice to the negative-margin width floor.
  Large line-end margins may overflow alongside a line-start float, in
  both LTR and RTL. Genuine flex/grid items ignore float exclusion and keep
  authored margins; RTL horizontal flex directions map to physical tracks.
  Synthetic inline lines, float groups and unbroken inline words retain
  their existing physical-order semantics.
- Retained RED/GREEN receipts cover CSS round-trip (2 PASS), static
  `codegen::tests::` selected by exact names (14 PASS), and seven layout
  flow-root regressions plus the RTL unbroken-word regression (8 PASS).
  Isolated Chromium 141 fixtures at 800x600 confirm the negative-margin,
  positive line-end overflow, flex-item margin and RTL track geometries.
  The old zero-height right-float fixture now explicitly represents an
  anonymous inline context; browser proof corrects its second float x from
  350 to 370, preserving overlap rather than inventing a nonzero exclusion.
  The table margin fixture's x=20 also matches browser geometry; this does
  not claim its empty-table height is browser-equivalent.
- Intermediate candidates were not published: `flow-root-display-v1`,
  `bfc-inline-bounds-v1`, `flow-root-authored-flex-v1` and
  `flow-root-opposite-negative-v1` each retained four failures in start
  1416, some worse than base. `flow-root-band-and-flex-v1` reached 7/8;
  `flow-root-inline-end-v1` reached 8/8 but regressed RTL
  `text-align-white-space-006` by 1200 pixels in start 5665. A minimal
  unbroken-word test reproduces x=60 versus 0 before the synthetic-word
  exclusion, then passes. The final word unit build takes 2m41s; the
  optimized runner build takes 5m10s including its unit-build lock wait.
- Final runner SHA256:
  `c7efd7697029dacb144b6a0ae9bf10c32852e05828f62f198ad2ebc17479f29a`.
  Receipts `flow-root-qualified-v1` bind WPT revision
  `fa5393bb9f5f7d41cc16d1aeede1809ccd378ac0`, frozen suite and 800x600.
  Original twenty eight-case batches now finish 160/160 PASS; start 1416
  becomes 8/8 with exact zero differences for all six selected margins.
  Additional starts 5268/5276/5284 remain 21/24 with identical full reports.
  All 23 batches finish 181/184 execution PASS, 3 FAIL. Full ordered
  path/status/pixel-diff comparison changes only the six repaired cases;
  no old PASS is lost. Caption-position-001 (876 pixels) and collapsing
  border-model-003/009 (2400 each) remain failures. Overlapping executions
  are not a global remaining count; clean-SHA full6548 acceptance is open.
- Neighbor layout checks retain the recorded float leading-margin failure,
  six table failures and three text failures. The three text failures also
  reproduce in the saved older `w3cos-runtime-auto-bfc-px-candidate-v1`
  executable. BFC checks pass 13/13. Broader substring attempts are not
  module-green evidence: `flow-root-codegen-module.log` was interrupted
  after accidentally selecting ESM generated-project tests with three
  failures; `flow-root-flex-margin-rtl-red.log` also selected an unrelated
  dynamic-script color serialization failure. Those failures are not
  claimed fixed or proven baseline against the published base.

### Bottom-caption wrapper margin repair

- Based on `a5df27d6141c636a8d7814b021af9664985ee01d`, auto-table height
  settlement now includes caption bottom margins, not margins on internal
  row/group boxes. The source and reference cat images already had identical
  positions and sizes; the second source wrapper was 16px too short.
- `auto_table_wrapper_retains_bottom_caption_margin` reproduces RED
  136px versus 152px, then GREEN in 2m36s. An isolated Chromium 141 fixture
  at 800x600 confirms a 20px row plus 100px bottom caption with 16px top and
  bottom margins has a 152px wrapper and caption y=36. Exact-name layout
  table checks finish 58/64, retaining the same six recorded failures.
- Optimized runner build finishes in 4m48s including the unit-build lock
  wait; SHA256
  `29a8ae97f6804d0bf35850f4e8f8f3906d0357e3ed0b298025d46a0afb6cb55b`.
  Receipts `bottom-caption-margin-v1` bind the pinned WPT revision, frozen
  suite and 800x600 viewport. Start 5268 becomes 8/8; caption-position-001
  improves from 876 pixels to exact zero. Across all 23 eight-case batches,
  182/184 executions PASS and two FAIL. Full ordered path/status/pixel-diff
  comparison changes only this caption case; all other reports are
  identical to `flow-root-qualified-v1`, with no old PASS lost.
  Collapsing-border-model-003/009 remain at 2400 pixels each. These
  overlapping focused executions are not the global remaining count;
  final clean-SHA full6548 acceptance remains open.

### Preserve computed collapsed-row height floors

- Based on `16f6f833c055d40e5a32f38d81a4086dd7e3269f`, the existing
  `collapsed_table_adds_outer_half_border_to_specified_row_height` test
  reproduces RED 96px versus 144px. Tree construction already computed
  the 144px minimum; later auto-height settlement discarded that computed
  floor because it read only the declared `min-height`.
- Settlement now reuses `collapsed_table_specified_rows_min_height` and
  caption-height accounting on the same component tree, preserving the
  computed grid floor without changing declared CSS or inventing a second
  collapsed-border formula. The regression is GREEN in 3m14s; exact-name
  table checks improve from 58/64 to 59/64, with the other five recorded
  failures unchanged. Isolated Chromium 141 fixtures at 800x600 confirm
  144px outer table heights for both 96px top and bottom cell borders.
  Chromium also reports the top-border row at y=48 versus native y=0;
  row-position equivalence is not claimed by this height/pixel repair.
- Optimized runner finishes in 5m11s including its unit-build lock wait;
  SHA256
  `776fcffe1fe40fdc9816e318605a1eb19727292fe17e8e9354db0c10004217a8`.
  Receipts `collapsed-row-floor-v1` bind the pinned revision, frozen suite
  and 800x600. Start 5284 becomes 8/8, with collapsing-border-model-003/009
  improving from 2400 pixels each to exact zero. All 23 eight-case batches
  now finish 184/184 execution PASS. Full ordered path/status/pixel-diff
  comparison changes only those two cases; every other report is identical
  to `bottom-caption-margin-v1`, with no old PASS lost. Overlapping focused
  executions are not the global remaining count. The stored full report
  still dates to 2026-08-22; final same-clean-SHA full6548 acceptance remains
  open, as does the observed top-row coordinate difference.

### Normalize collapsed block-axis grid-line centers

- Based on `6b322346f991fa5e88edbb3b942c9712bf1443bd`, the new exact-name
  `collapsed_top_border_row_rect_begins_at_grid_line_center` regression is
  RED at row y=0 versus Chromium's y=48, despite matching the 144px outer
  height. The fix represents cell block-axis borders as half-width layout
  edges, includes the outer grid halves in the table wrapper and expands
  the paint rectangle back to the full border footprint. Obsolete vertical
  negative-margin overlap is removed; horizontal conflict handling remains.
- Auto-height settlement also preserves the resolved descendant bottom
  half-border for auto rows. Explicit cell heights remain minimum grid-box
  heights, and cell baseline alignment uses the same half-border geometry.
  Native block trees containing only rows/row groups receive a context-free
  anonymous table without changing the principal block's own CSS box.
- Isolated Chromium 141 fixtures establish the empty-row coordinates
  (5,25,27) at outer height 52, and the three anonymous groups at
  (y,height)=(4,20),(24,20),(44,20), outer height 68. The former unit
  expectations encoded full-border overlap and are corrected to those
  browser measurements. Exact regressions are GREEN: row center 2m50s,
  equal-border bottom edge 2m41s, inline-table baseline 3m05s and anonymous
  wrapper 3m44s. The final unit executable passes 77/82 exact-name table or
  collapsed checks; five previously recorded failures remain. Receipt:
  `target/wpt-targeted/collapsed-grid-center-v2-unit-neighbors.json`.
- Intermediate runner `collapsed-grid-center-v2` builds in 2m25s, SHA256
  `a89337318eb05acdcef30aed012f5771949f486786a1c52f007fdbbbfa81f766`.
  All 23 eight-case reports finish 180/184 PASS; four old background PASS
  cases regress at start 5553. Full ordered report comparison changes only
  those four cases. The candidate is held, not published as qualified.
- Chromium's column-background fixture has table height 103 and column
  (y,height)=(17,97), whereas the intermediate native result is 99 and
  (19,91). Table-part projection still removed legacy block-axis border
  halves after cell geometry had switched to grid centers. Both table-part
  and cell backgrounds now retain the complete center-bounded cell box;
  the projection unit gains block-axis assertions (GREEN in 4m07s).
  Intermediate `collapsed-grid-center-v3` builds in 6m08s including its
  preceding unit-build lock wait, SHA256
  `54c9cd4bee576aee4f4917e68b9f6af7f3b088841ee5198342554dd6860215d3`.
  Start 5553 recovers to 7/8; only the cell image case remains, at 2130
  pixels. The background-image padding-origin calculation still uses full
  block-axis borders. It now halves all four resolved cell borders; its
  fixture uses the measured center-bounded cell box (138,55,57,19).
  The corrected paint-background fixture is rebuilt and GREEN in 5m00s
  including its preceding runner-build lock wait. At this intermediate
  stage, final image-origin unit and pixel qualification are pending;
  earlier prototype receipts do not qualify the later source.
- Final runner `collapsed-grid-center-v4` builds in 4m22s including its
  unit-build lock wait, SHA256
  `d2bd2e43ffbde41dd5beb84849e7e784dfeae33fe38f2b136a4cee4cd3a7b881`.
  The original background batch returns to 8/8, all exact-zero pixel diffs.
  All 23 eight-case batches finish 184/184 PASS. Full ordered test-object
  comparison with `collapsed-row-floor-v1` reports no differences, with no
  old PASS lost; receipt `collapsed-grid-center-v4-report-comparison.json`.
  The final native column fixture now agrees with Chromium: table height
  103, column y=17 and height 97. Receipts use the pinned revision, frozen
  suite and its 800x600 viewport. These overlapping executions do not
  constitute a full6548 run or a global remaining count.
- The final image-origin regression is rebuilt and GREEN in 5m04s including
  its runner-build lock wait. The final executable passes 87/92 exact-name
  table, collapsed paint and image-origin checks; the same five recorded
  table failures remain, with no additional failure. Receipt:
  `collapsed-grid-center-v4-unit-neighbors.json`. Static `git diff --check`
  passes. The parent size check excludes vendor and is not represented as
  a W3COS size gate. No whole-runtime gate or full6548 acceptance is claimed.
- Fresh eight-case probes at starts 0 and 8 pass 16/16; start 16 is 4/8,
  with two dynamic-inline containing-block assertion failures and two
  hypothetical abspos reftest failures. These are current failure-subset
  evidence, not a current global remaining count. Full6548 acceptance is
  still open.

### Preserve non-collapsing Unicode spaces in abspos static positions

- Based on published `f005de5d3ad18ffea7cf921cf4f4797a21872d2c`, fresh
  eight-case starts 0,8,16,24 finish 26/32 PASS and six FAIL. Receipts:
  `batch-<start>-nbsp-before`. These are current focused results, not the
  old 2026-08-22 global failure count.
- `between-float-and-text.html` has a NBSP line followed by an auto-inset
  absolutely positioned block and a float with 20px top margin. Chromium
  places both blocks 20px below that line's start; native puts the abspos
  at the line start (8000 different pixels). The new exact-name
  `absolute_block_after_nbsp_uses_the_next_line_static_position` reproduces
  RED y=0 versus 20 in 3m22s, without changing WPT expectations.
- Static-position accounting previously used Unicode `is_whitespace` to
  classify a line as empty. NBSP and other Unicode spaces survive CSS
  whitespace collapsing. It now tests the CSS collapsible character set
  (space, tab, LF, CR, form feed), retaining non-collapsing line content.
- Exact-name positioning/forced-break baseline is 30/34. Besides the new
  RED, three existing tests fail: forced-break inline static position,
  decorated inline fragment block static position and standalone forced
  break strut. Receipt `nbsp-static-unit-before.json`. GREEN and pixel
  qualification of this candidate remain pending; no full-runtime or
  full6548 acceptance is claimed. The parent size check excludes vendor.
- The new regression is GREEN in 3m06s. The same exact-name 34 checks now
  pass 31/34; ordered status comparison changes only the new NBSP test,
  with the three other failures unchanged and no old PASS lost. Receipt:
  `nbsp-static-unit-after.json`. Optimized-runner pixel qualification is
  still pending at this stage.
- Optimized runner builds in 5m13s including its unit-build lock wait,
  SHA256 `0865d761c8ffe28226046c007cd91550d5b58aec9f982529c1be7b87f82e419a`.
  Pinned starts 0,8,16,24 now finish 27/32 PASS and five FAIL. Ordered full
  test-object comparison changes only `between-float-and-text.html`, from
  8000 pixels to exact zero; the other 31 reports are identical, with no
  old PASS lost. Receipt `nbsp-static-v1-comparison.json`. This qualifies
  the NBSP repair, not the five remaining focused failures or full6548.
  Static `git diff --check` passes. All source changes remain within W3COS.

### Use the inline line-box origin for descendant positioning

- Based on published `082b9c721e068ce02d25908b288a736be512ddc5`, the exact
  existing `block_static_position_follows_a_decorated_inline_fragment` test
  is RED y=142 versus 100. The 42px excess is the inline half-leading for
  16px text at 100px line height. Receipt `inline-static-line-origin-red.log`.
- Ordinary descendants already recurse from the inline line-box origin,
  subtracting the paint/em box's half-leading. The relative containing box
  passed to positioning omitted that subtraction. It now uses the same
  origin, without changing authored line height or adding case-specific CSS.
- WPT `static-inside-inline-002.html` and the existing unit require a 100px
  static offset; the published runner differs by 8400 pixels. Isolated
  Chromium 141 instead reports a 0px relative offset for this fixture.
  This discrepancy is retained explicitly: pinned WPT is the acceptance
  criterion here, and browser equivalence is not claimed for this case.
- Unit GREEN and the four eight-case abspos batches remain pending for this
  candidate. Existing `nbsp-static-v1` receipts are the comparison baseline
  (27/32 PASS, five FAIL). Final full6548 acceptance remains open; the parent
  size check excludes vendor and is not a W3COS size gate.

  The existing regression is GREEN in 2m34s. Exact-name positioning/break
  checks improve from 31/34 to 32/34, with only the decorated-inline status
  changed. Receipt `inline-line-origin-v1-unit-neighbors.json`.
  Optimized runner builds in 4m23s including its unit-build lock wait,
  SHA256 `d2bb4d073ec417bffb4305eea055ec342b3367525c5dd6fec9d9fb8676310810`.
  Pinned starts 0,8,16,24 finish 28/32 PASS and four FAIL. Ordered full test
  comparison changes only `static-inside-inline-002.html`, from 8400 pixels
  to exact zero; every other report is identical and no old PASS is lost.
  Receipt `inline-line-origin-v1-comparison.json`. Static `git diff --check`
  passes. This closes the pinned WPT defect, not the documented Chromium
  discrepancy or the remaining focused/global failures.

### Count inline static advance from the content origin

- Based on published `d9da5bf14d4db98124ed93d91da30395fc8600db`, the existing
  `auto_inset_absolute_inline_uses_the_line_after_a_forced_break` test is
  RED y=38.4 versus 19.2. Receipt `inline-cursor-content-origin-red.log`.
  Pinned WPT `hypothetical-inline-alone-on-second-line.html` remains at
  454 different pixels in the `inline-line-origin-v1` baseline.
- The containing box already starts after the parent's inline edges.
  Static-position accounting nevertheless initialized its local cursor
  with margin/border/padding again. For a padded inline, comparing this
  padded cursor with the unpadded content width spuriously wraps the first
  fragment, adding one line before the real forced break. The cursor now
  starts at zero; authored padding and the post-break fragment correction
  remain intact. This is a shared coordinate fix, not a test-path exception.
- Unit GREEN and pinned starts 0,8,16,24 are pending. Comparison baselines
  are `inline-line-origin-v1-unit-neighbors.json` (32/34) and the associated
  WPT batches (28/32 PASS, four FAIL). No full6548 acceptance is claimed;
  the parent size check excludes vendor and is not a W3COS size gate.

  The existing regression is GREEN in 2m26s. Exact-name positioning/break
  checks improve from 32/34 to 33/34; only the forced-break static-position
  status changes, with no old PASS lost. The standalone break-strut failure
  remains. Receipt `inline-cursor-v1-unit-neighbors.json`. Pixel qualification
  is still pending at this stage.

  Optimized runner builds in 4m13s including its unit-build lock wait,
  SHA256 `2a65635fcba5b226582d1a666841d4f78b4e45c36492f04071a826908a7d32c9`.
  Pinned starts 0,8,16,24 now finish 29/32 PASS and three FAIL. Ordered full
  test-object comparison changes only the hypothetical second-line case,
  from 454 pixels to exact zero; every other report is identical, with no
  old PASS lost. Receipt `inline-cursor-v1-comparison.json`. Static
  `git diff --check` passes. The remaining three focused failures and
  full6548 acceptance are not closed by this repair.

### Preserve forced-break struts and float source anchors

- Based on published `7b7f16dcc3a08d5ac67c611b34043a7131ffcbac`, the existing
  `standalone_forced_break_establishes_its_line_height_strut` test is RED:
  reserved height 200 becomes a 16px paint/em box. Forced-break text nodes
  now retain their measured layout strut rather than undergoing glyph-box
  projection. This exact regression is GREEN in 2m52s; the same positioning
  and break checks improve from 33/34 to 34/34, with no old PASS lost.
  Receipt `forced-break-runtime-unit-after.json`.
- The new DOM source-anchor test first runs zero cases under an incorrect
  module filter; that invocation is not a passing regression. The exact
  `document::image_component_tests::float_after_forced_break_keeps_its_inline_source_anchor`
  invocation reproduces RED: extraction moves the float outside its prior
  inline break. Receipt `float-forced-break-source-anchor-red-exact.log`.
  After an in-flow forced break, float extraction now retains the owning
  inline source anchor. Nested passive inline breaks count; hidden,
  out-of-flow and atomic inline contents do not create this outer anchor.
- The exact DOM test (both float sides) is GREEN in 2m27s including its
  runtime-build lock wait. Related DOM float checks improve from 15/17 to
  16/17, with only this status changed; the earlier static-line/block-order
  failure remains. Receipts `forced-break-dom-unit-before/after.json`.
- WPT `static-inside-float-inside-inline.html` remains RED in the published
  baseline (50200 pixels). Chromium 141 confirms float and nested abspos
  offsets of 200px from the wrapper. The published native lowered tree puts
  the float before the break; this prototype's optimized-runner pixel
  qualification is pending. These unit results do not close that WPT case,
  the remaining focused failures or full6548. Parent size checking excludes
  vendor and is not presented as a W3COS size gate.

  Intermediate optimized runner `forced-break-source-v1` builds in 4m29s
  including queued unit-build waits, SHA256
  `8cd5c26a7b08d45ae756b89536a946b56087fe318fb80138f0225f4dca7e0f37`.
  Start 24 remains 7/8: the float case improves from 50200 to 36800 pixels,
  but is not qualified or published. The native source order is restored
  and br height is 200; float and abspos are still 292px below the wrapper.
  Forced-break projection falls back to the parent's paint/em-box y,
  retaining its extra 92px half-leading. It now anchors an empty preceding
  line to the preserved break rectangle's raw origin. A direct projection
  regression covers both the float and nested abspos. Unit and pixel
  qualification for `forced-break-source-v2` remain pending.

  Final projection regression is GREEN in 2m49s; the final executable
  passes all 35 exact positioning/break checks. Receipt
  `forced-break-source-v2-unit-neighbors.json`. Optimized runner builds in
  4m50s including its unit-build lock wait, SHA256
  `041de32edd370d1fad76925e3698d1f5e82525dfdbb9be3c388da4c3d6dfd803`.
  Pinned starts 0,8,16,24 and six related float batches
  (1358,1366,1385,1400,1408,1416) finish 78/80 PASS; only the two existing
  dynamic-inline harness failures remain. Ordered full test-object comparison
  changes only the float/abspos case, from 50200 pixels to exact zero; every
  other report is identical, with no old PASS lost. Receipt
  `forced-break-source-v2-comparison.json`. Native float and abspos now both
  start 200px below the wrapper, matching the isolated Chromium measurement.
  Static `git diff --check` passes. This is focused qualification, not a
  full-runtime module gate or final full6548 acceptance.

### Empty anonymous lines beside dynamically positioned inline ancestors

- Based on published `d9faf5f`, the existing JS DOM regression
  `block_in_inline_static_position_ignores_adjacent_collapsible_whitespace`
  reproduces `offsetTop = 19.2` rather than `0`, even without a prior layout
  flush. Transient tree diagnostics identify a zero-width pair of empty
  inline descendants inside an anonymous block that reserves a 19.2px line.
  Diagnostics were removed; no CSSOM getter correction was added.
- Removing the unconditional line minimum makes the existing exact unit
  pass (2m57s build). Prototype `dynamic-inline-empty-line-v1` builds in
  2m11s and passes 7/8 at start 16: the nested-inline fixture still fails
  because collapsible whitespace leaf measurement retains a font strut.
- Prototype v2 additionally gives empty anonymous block lines zero automatic
  content height. Build: 2m10s; runner SHA256
  `1628f9b302e9eba85761a29ee79d08e80502562458588d355b41d7df293ba761`.
  Ten eight-case batches at starts 0, 8, 16, 24, 1358, 1366, 1385, 1400,
  1408 and 1416 pass **80/80**. Receipts bind revision
  `fa5393bb9f5f7d41cc16d1aeede1809ccd378ac0` and 800x600; ordered full
  test-object comparison against `forced-break-source-v2` changes only
  indices 17 and 18 from FAIL to PASS, with the other 78 objects unchanged.
  See `dynamic-inline-empty-line-v2-comparison.json`.
- The first prototype retains 35/35 previously passing positioned-layout
  units. Extended empty-inline checks pass 23/24. A control rebuild that
  restores the original unconditional minimum and removes the zero-height
  branch, leaving only the unused new helper, reproduces the same painted
  inline failure (`y = 1.6000004`, expected `0`). This is a controlled
  behavioral baseline, not a whole-module green run or a clean-SHA build.
  See `dynamic-inline-painted-baseline-control.log` (3m36s build).
- [CSS 2.2 section 9.4.2](https://www.w3.org/TR/CSS22/visuren.html#inline-formatting)
  requires empty line boxes to have zero height, while preserving text,
  preserved whitespace/newlines, decorated inlines and atomic in-flow boxes.
  Candidate v3 extends the content predicate to all decoration edges,
  unresolved percentage/viewport spacing, and correct `pre-line` whitespace
  handling, with a focused predicate unit. Its unit build and production
  qualification remain pending; v2 receipts do not qualify v3. No commit
  or push has been made for this candidate and no full 6548 run is claimed.

- Candidate v3 predicate unit passes (2m56s build); combined related exact
  units pass 48/49, with only the controlled baseline painted-inline failure.
  Production build: 2m10s; runner SHA256
  `74d60506f4774465256221d86cbf4534c9706b3eda29e79aced6ee5a827eac5d`.
  Expanded qualification stops after 13 eight-case batches: **103/104**.
  `text-indent-on-blank-line-rtl-left-align.html` at index 5765 loses its
  prior PASS from `collapsed-grid-center-v4`, differing by 20000 pixels.
  No publication: an expanded older baseline detects a regression, but this
  comparison alone does not attribute it specifically to the v3 predicate
  rather than intervening forced-break changes.
- Actual fixture layout shows its flex-backed break at the preceding line's
  bottom, height zero. Treating this boundary as the reserved line's origin
  projects the following inline block an additional 100px downward. Candidate
  v4 anchors a break's origin only when the break actually reserves positive
  height, retaining the existing positive-height float/abspos regression.
  New direct projection unit and final production qualification are pending.
  See `empty-line-v3-rtl-blank-5765-layout.log`; an earlier diagnostic at 5764
  is a different fixture and is not evidence for the 5765 failure.
- Separately, v2 bounded discovery at starts 32 through 112, in eight-case
  batches, passes **88/88**. This locates the next discovery entrance at 120;
  it does not bind those passes to v4 or establish global remaining failures.

- Final v4 direct zero-height-break projection unit passes (3m00s build).
  Combined related exact units pass **49/50**; the only failure remains the
  controlled baseline painted-inline coordinate assertion. Production build:
  **2m02s**; runner SHA256
  `fb9c7b52b4114a1d18fd5825b4185630e74d5772d59feb921647ea9568bdf80d`.
  The RTL batch at 5761 restores 8/8 and zero differing pixels. Expanded
  qualification completes all 27 eight-case batches, **216/216 executions**.
  Ordered full test-object comparison uses `forced-break-source-v2` for the
  ten core batches and `collapsed-grid-center-v4` for the other 17. Only the
  two dynamic-inline harness cases change from FAIL to PASS; the other 214
  executed test objects are unchanged and no earlier PASS is lost. This is
  an execution count, not a unique-case count or full-suite result.
  See `dynamic-inline-empty-line-v4-comparison.json`.
- Bounded v3 background discovery at 120, 128, 136 and 144 stops at **30/32**:
  `background-applies-to-006.xht` differs at (103,55), actual RGB 20 versus
  expected 0; `background-applies-to-012.xht` differs at (7,103), actual RGB
  245 versus expected 255. Both are one-pixel failures. Root cause remains
  unverified; no tolerance, reference change or blanket clipping is applied.
  Candidate v4 recheck of this next eight-case repair batch is separate from
  the successful 216-execution qualification.

- Final v4 recheck at 144 remains **6/8** with the same two one-pixel
  failures; all eight full test objects are identical to the v3 discovery
  receipt. This next repair batch is still open. Full 6548 conformance,
  whole-module gates and browser application acceptance remain unproven.

### Background references, serif overhang and block/inline text baselines

- Start from published `b6bc6f5` and its recorded v4 runner. The two one-pixel
  failures at 144 remain open. Isolated Chromium 141.0.7390.37 at 800x600,
  device scale 1, also fails literal fixture-versus-reference comparison:
  `background-applies-to-006.xht` differs at (103,53), RGB 5 versus 0;
  `background-applies-to-012.xht` at (7,101), RGB 252 versus 255. Browser
  default family is Times. These are comparisons within Chromium, not
  Chromium/native pixel parity; the native coordinates and values differ.
- Temporary Skia font diagnostics (2m34s unit build) show Times `b` ink
  bounds with left -1 and Times New Roman with left -2. Both paint a tiny
  negative-side-bearing edge; Georgia does as well. The native generic
  serif fallback resolves to Times New Roman. No font swap, blanket glyph
  clipping, fuzzy threshold or reference change is introduced to hide the
  two failures. The temporary diagnostic was removed.
- Next clean-source runner discovery at 152 passes 8/8. At 160 it stops at
  6/8, with `background-bg-pos-204.xht` differing by 3071 pixels and
  `background-bg-pos-208.xht` by 3579. Both are still open. Actual and
  expected fuchsia diamond extents match: (790,590)-(799,599) for 204 and
  (790,41)-(799,50) for 208, with 60 fuchsia pixels each. Root box geometry
  is also recorded in `root-background-162-before-layout.log` and
  `root-background-163-before-layout.log`, alongside their reference logs.
- The large differences lie in paragraph text, not background positioning.
  The reference contains an out-of-flow image and therefore lowers its
  text into an inline fragment. Current single-line block painting applies
  a string-dependent ink-bottom-overflow correction, while inline painting
  retains the shared font baseline. A new block-versus-inline descender
  raster regression has been added; its RED build is pending. No production
  correction or publication has been made for this batch.

- The first direct baseline unit used default white foreground on a white
  surface and misleadingly passed; it is not valid regression evidence.
  The fixture now explicitly uses black text and asserts visible pixels in
  both renderings. Its corrected exact RED reproduces **3071 differing
  pixels**, matching the real WPT 204 difference (2m31s build).
  Real text ink bounds are one pixel higher in actual than reference:
  204 actual (32,34)-(525,47), reference (32,35)-(525,48);
  208 actual (0,34)-(582,47), reference (0,35)-(582,48).
- Candidate removes the string-dependent single-line ink-bottom correction
  rather than changing background positioning, clipping text or adjusting
  references. Its exact GREEN build is pending. Seven focused baseline
  units pass 6/7 before the correction, with only the new descender unit
  failing. Wider isolated renderer-module before/after receipts are retained
  separately; no publication or WPT closure is claimed yet.

- Final candidate exact descender unit passes with **0 differing pixels**
  (2m31s build). Isolated renderer module improves **39/41 to 40/41**,
  with no previous PASS lost. Only the previously failing
  `default_ascii_text_is_pixel_invariant_across_inline_fragments` remains.
  Before/after receipts are `block-inline-render-module-before.json` and
  `block-inline-render-module-after.json`; no whole-runtime green is claimed.
- Production build: **1m57s**; runner SHA256
  `3813553c4b54cb75de6b8d1992874f85c653310abcfda79e78d1392aa140ca50`.
  Fixed batch 160 passes 8/8, with indices 162 and 163 improving respectively
  **3071 to 0** and **3579 to 0** differing pixels. All prior 27 related
  eight-case batches also pass, giving **224/224 executions** including 160.
  Ordered full-object comparison against the published empty-line v4
  receipts (and the clean-source discovery receipt at 160) changes only
  these two cases; the other 222 executed objects are unchanged and no prior
  PASS is lost. See `block-inline-descender-v1-comparison.json`.
- The separate open batch at 144 remains **6/8**, with all eight result
  objects identical to its v4 receipt. The two one-pixel failures are not
  included in the 224 successful repair/regression executions and are not
  declared fixed. Final 6548 same-clean-SHA conformance remains pending.

### Sole atomic inline line-strut alignment (follow-up to ccbebd1)

- Clean-source discovery at `ccbebd1` examines 15 eight-case batches,
  starts 168 through 280, under the same pinned 6548 manifest and 800x600
  viewport: **115 PASS / 5 FAIL / 120 executions**. Four failures retain
  the known one-pixel font-overhang result signatures; the fifth is
  `background-position-applies-to-001.xht` (index 284), **450 pixels**.
  These remain raw FAIL results, not relaxed or counted as passes.
- For index 284, both blue regions contain 225 pixels. The test background
  occupies `(92,154)-(106,168)`; the reference image occupies
  `(92,73)-(106,87)`. Layout dumps confirm the table row-group background
  is correctly bottom-right, while the reference's 15px image with
  `vertical-align:bottom` is incorrectly top-aligned inside a 96px line.
  The reference is also rendered by W3COS; no WPT fixture is modified.
- New real-DOM unit
  `jsdom::tests::sole_inline_image_bottom_aligns_inside_the_block_line_strut`
  first fails with **3px versus 84px**. Its 102px container and 15px
  image height assertions pass before the failing alignment assertion.
  RED build: 2m31s. DOM-lowering correction passes this exact unit;
  intermediate build: 2m47s.
- The isolated pre-correction image/strut neighbors receipt contains
  **15/17 PASS**: the new DOM test and the existing direct-component
  `layout::tests::block_inline_image_uses_line_height_strut_and_vertical_align`
  both fail with the same 3px/84px signature. The same wrapped-flex
  line-strut problem therefore has both DOM and direct-component entry
  points, rather than being a table background-positioning error.
- Both entry points retain a single flex line for a sole atomic inline
  in an automatic-height block with automatic min/max height. This line
  can use the font strut for cross-axis alignment; real multi-child lines
  and explicit height/min/max constraints retain the wrapping path.
  The direct-component exact unit passes after the second entry-point
  correction (2m36s build). The final isolated neighbors receipt passes
  **17/17**, with no previous PASS lost.
- Production build succeeds in **2m24s**, binary SHA-256
  `ff5c83eaaf5426debd2164d32712174b2f3989067f3278b36a9ec26d8cca7077`.
  Target batch 280 passes **8/8**; index 284 is now **0 differing pixels**
  and **0 maximum difference**, with both allowed thresholds still zero.
- All 28 prior successful eight-case regression batches, all 15 discovery
  batches, and the separate open batch at 144 are replayed:
  **346 PASS / 6 FAIL / 352 executions**. Ordered full-object comparison
  changes only index 284 from FAIL to PASS; the other 351 executed
  objects are unchanged, with no prior PASS lost. See
  `sole-atomic-strut-v1-comparison.json` and the two
  `sole-image-strut-neighbors-{before,after}.json` receipts.
- The four newly discovered font-overhang aliases are verified at the
  same coordinates/colors as the two original open cases: 006 variants
  differ at `(103,55)` RGB20 versus RGB0; 012 variants differ at `(7,103)`
  RGB245 versus RGB255. All six remain strict FAIL, with no font swap,
  glyph clipping, fixture edit, or tolerance change. These are not a
  fresh total remaining count for the 6548 suite. Same-clean-SHA final
  full-suite conformance remains unproven.

### Post-3aa670e discovery: font aliases and bidi fragment whitespace

- Published clean source `3aa670e`, unchanged production binary
  `ff5c83eaaf5426debd2164d32712174b2f3989067f3278b36a9ec26d8cca7077`:
  ten eight-case batches at starts 296 through 368 yield
  **74 PASS / 6 FAIL / 80 executions**, under the pinned revision,
  frozen 6548 manifest and 800x600 viewport. Discovery stops on each
  unfamiliar failure, then resumes only after classification. All raw
  FAIL results and zero thresholds are preserved.
- `background-position-applies-to-012` differs only at `(10,125)`,
  RGB20 versus RGB0: white `b` spills left onto its black border.
  Chromium 141.0.7390.37 independently comparing the same literal fixture
  and reference at 800x600/DPR1 also has one differing pixel, `(10,122)`,
  RGB5 versus RGB0. This supports a font-overhang diagnosis, not native
  pixel equivalence or a PASS claim.
- `background-repeat-applies-to-006` differs only at `(107,55)`,
  RGB(20,138,20) versus (0,128,0); its 012 variant only at `(7,105)`,
  RGB(243,249,243) versus white. Chromium independently reproduces one
  pixel in each paired fixture/reference comparison: 006 at `(107,53)`
  RGB(5,130,5) versus green; 012 at `(7,103)` RGB(252,254,252) versus white.
  These three additional font cases remain open, alongside the six
  previous cases; this is not a full-suite remaining count.
- Batch 368 stops at three new failures: index 368 `bidi-006`,
  **1299 pixels**; index 372 `bidi-010`, **10240 pixels**; index 375
  `bidi-text/bidi-003`, **246 pixels**. The other five cases pass.
- Index 375 actual/reference screenshots and native layout dumps show
  correct visual text order, but incorrect decorated-fragment whitespace
  ownership. In the second paragraph, actual `ddd eee fff` has width
  88.28906 instead of reference `ddd eee fff ` width 92.28906; actual
  `jjj kkk lll` is 77.671875 instead of 81.671875. Origins and heights
  match. Both lost trailing spaces are exactly 4px; their border spans
  are correspondingly shortened. The next bounded repair target is
  `reorder_explicit_bidi_children` edge-whitespace normalization, not
  replacement of the existing bidi algorithm. No implementation fix is
  claimed by this discovery entry; 368/372/375 remain strict FAIL.

### Bidi decorated-fragment boundary-space repair (after f8df510)

- New exact DOM unit
  `explicit_bidi_preserves_decorated_fragment_trailing_space` is first
  RED: all three non-final decorated visual runs lose their trailing
  spaces. Build 19.06s. The first correction preserves spaces already
  present in those runs; the direct unit passes (17.24s build), but the
  actual index-375 reftest remains **24 pixels FAIL**, down from 246.
  The 24 remaining pixels are only the orange border's missing 4px span,
  at x=221..224, y=155..189. This intermediate result is not acceptance.
- Native layout confirms actual `ddd eee fff` still has width 88.28906,
  while the reference's trailing-space run is 92.28906. In the real DOM
  pipeline, prior logical whitespace collapse assigns the boundary space
  to the adjacent bare run. The direct regression input is refined to
  reproduce that normalized boundary and becomes RED again (23.37s
  build), missing only the `ddd eee fff ` trailing space.
- The second correction lets a collapsed visual boundary space extend
  the preceding decorated inline's paint span, without duplicating the
  space in the anonymous run. Line-end whitespace remains trimmed.
  The ownership rule is font-independent; an intermediate font-specific
  condition is removed rather than retained as a conformance workaround.
  The refined exact unit passes (24.49s build).
- The old unit `explicit_bidi_moves_collapsed_spaces_outside_decorated_fragments`
  asserted the now-proven incorrect border-shortening behavior. Its same
  fixture is retained in the corrected unit
  `explicit_bidi_collapses_boundary_spaces_without_losing_fragment_ownership`,
  checking exact single-space visual content and decorated ownership.
  Before/after isolated bidi/RTL neighbors are **15/17 -> 16/17**;
  the only remaining failure is the pre-existing
  `rtl_inline_block_aligns_its_single_text_line_to_the_inline_end`.
  Comparison explicitly maps this corrected test identity; it is not
  presented as an unchanged old assertion. Whole DOM-module green is
  not claimed.
- Additional neighbor baselines at 376/384/392 are **21 PASS / 3 FAIL**,
  collected while the old production binary hash remains unchanged.
  Production v2 build succeeds in 2m29s, and index 375 reaches **0 pixels**
  under zero thresholds. Qualification then stops at a prior-PASS
  regression, index 5600 `bidi-span-003.html`, **168 pixels**. The partial
  v2 receipt contains **191 PASS / 1 FAIL / 192 executions**; it is not
  final qualification and this candidate is not committed.
- The ordinary RTL fixture has two unsplit sibling inline decorations
  separated by an external space. V2 incorrectly extends the first
  border into that external space. New exact unit
  `ordinary_bidi_keeps_external_space_outside_unsplit_decorations` is RED,
  actual `["inspect ","pause"]` versus expected `["inspect","pause"]`.
  V3 allows external-space reassignment only when the preceding source
  inline really has multiple bidi visual fragments, preserving ordinary
  unsplit sibling boundaries. This unit passes; final DOM bidi/RTL units
  are **17/18**, with only the same pre-existing RTL-alignment failure.
  V3 production build succeeds in 2m24s: index 375 and prior-regression
  5600 both have zero differing pixels. Full comparison then stops at
  another prior-PASS regression, index 376 `bidi-004`, **57 pixels**.
  The partial v3 receipt is **435 PASS / 13 FAIL / 448 executions**;
  only index 375 improves and index 376 regresses. No commit is made.
- V3 counts a source inline's fragments across all visual lines. The
  wrapped fixture's orange inline has multiple fragments overall, but
  only one on each line; the extra external space must remain outside
  its border. The first two direct-fixture attempts retain uncollapsed
  logical boundary whitespace and produce a different wrap, so their
  missing-fragment failures are explicitly not valid semantic RED proof.
  After matching real DOM whitespace collapse, exact unit
  `wrapped_bidi_keeps_external_space_outside_single_fragments_per_line`
  is valid RED: `pXpX ` versus `pXpX`.
- V4 counts fragments per `(source inline, visual line)` in a linear
  hash-map pass. External boundary whitespace extends decoration only
  for a source inline actually split on that same line, not for a
  continuation created merely by wrapping. The normalized wrapped unit
  passes. Final isolated DOM bidi/RTL neighbors are **18/19**, with only
  the same old RTL-alignment failure. Whole-module green is not claimed.
- V4 production build succeeds in **2m31s**. Binary SHA-256:
  `f38327ce305d0565909a4d795f71383bd06e78e872e650941994260a3104d3f4`;
  qualified DOM source blob: `4b221b56de53a922914f53d157d76b4148780671`;
  frozen suite SHA-256:
  `5d0468173f766098c4f2a39126337e027e8533de3ad195b684b70db6cb1e672c`.
  Initial targets 375, 376 and 5600 all have **0 differing pixels** and
  **0 maximum difference**, with both allowed thresholds still zero.
- Final V4 qualification executes 58 eight-case batches:
  **450 PASS / 14 FAIL / 464 executions**. Ordered full-object comparison
  against the published sole-atomic-strut receipts, post-3aa670e discovery
  and unchanged-source neighbor baselines changes only index 375 from
  FAIL to PASS. All other 463 executed result objects are unchanged,
  with no prior PASS lost. In particular, both intermediate regressions
  376 and 5600 return to their original zero-pixel PASS. Source blob and
  binary hash are checked throughout qualification; see
  `bidi-trailing-space-v4-comparison.json` and
  `bidi-trailing-space-neighbors-v4-after.json`.
- The 14 failures remain open within this subset; this is not a fresh
  global remaining count. Full same-clean-SHA 6548 zero-FAIL/ERROR
  conformance is still pending. No fixture, font, glyph clipping or
  tolerance workaround is introduced by this repair.
- Independent read-only diagnosis of index 368 shows only three native
  layout rows differ from its reference: bare `a`, `fgh`, and `lm` runs
  retain InlineBlock display at y=197 rather than Inline at y=203.2.
  Their x positions, widths and heights match. This 6.2px baseline-entry
  difference is a separate next repair target; index 368 is not declared
  fixed by the boundary-space correction.

### Anonymous nowrap text inline semantics (after 46461db)

- New real-DOM-to-layout unit
  `layout::tests::nowrap_dom_text_keeps_intrinsic_width_and_inline_baseline`
  compares bare `a`/`fgh` runs with equivalent ordinary span runs beside
  a bordered, vertically padded inline. First valid RED build: **3m23s**.
  The first width assertion passes, **14.203125px** in both layouts, but
  actual relative y is **-6.199999px** versus reference **0px**. Both
  runs are present with nonzero widths; this is not an empty-fixture pass.
- DOM lowering used InlineBlock solely to retain nowrap text advance.
  Current layout already resolves intrinsic width for nowrap Inline
  text, so anonymous text nodes now keep Inline display, including under
  nowrap. This removes unintended atomic-box baseline semantics without
  changing principal inline-block elements or swapping font metrics.
- GREEN build: **2m49s**, the new unit passes both runs' intrinsic-width
  and baseline assertions. The focused runtime sample is **14/14 post-fix**;
  its original `before` filename is misleading because compilation had
  already replaced the executable. It is not a before/after comparison.
  DOM bidi/RTL comparison remains **18/19**, with the same existing
  `rtl_inline_block_aligns_its_single_text_line_to_the_inline_end` failure
  and no prior PASS lost. Production build completes in **2m05s**.
- Production qualification: **464 executions, 452 PASS / 12 existing FAIL**.
  Only `css/CSS2/bidi-006.xht` and `css/CSS2/bidi-text/bidi-006a.xht`
  change, each **1299 differing pixels -> 0**, with zero tolerance and
  unchanged references. Other 462 complete result objects are unchanged;
  no previous PASS is lost. Target batches 368/376/5593 are 7/8, 8/8, 8/8.
  Receipt: `target/wpt-targeted/nowrap-anonymous-inline-v1-comparison.json`.
  Binary SHA256:
  `851c7df04273a637e1f8c650e421a50c07f810dc86e2e4e8231d7048c5107f58`.
  These focused executions are not a new full-6548 remaining-failure count
  or a completed pixel-parity acceptance.

### Positioned shrink-fit margin boundary (after 0da29e3)

- Current index 372 (`css/CSS2/bidi-010.xht`) actual/reference layout
  dumps show identical internal text coordinates, but the positioned
  painted container is 372.82813px wide versus 308.82813px in the float
  reference: its two 32px horizontal margins are included in its own
  assigned width. Dumps are retained under `target/wpt-targeted/` as
  `bidi-010-current-layout.log` and `bidi-010-reference-layout.log`.
- New unit `absolute_shrink_fit_border_box_excludes_its_own_horizontal_margins`
  yields valid RED: outer max-content is 160px, but the assigned-width
  helper returns 160px instead of the expected 96px. The first Absolute
  iteration fails; Fixed and layout assertions are not claimed RED-executed.
- Both preferred and min-content border-box bounds now exclude own
  horizontal margins for Absolute/Fixed as for inline/float boxes.
  GREEN build completes in **3m10s**, with Absolute/Fixed, constrained
  available width and computed border-box assertions all passing.
  A focused runtime post-fix sample is **12/12**; no before/after unit
  comparison is claimed. Receipt:
  `target/wpt-targeted/absolute-margin-runtime-neighbors-after.json`.
  Production build completes in **2m22s**. Initial batches 368/376/384
  are all **8/8**, including the three bidi-010 targets. Final qualification
  is **464 executions, 455 PASS / 9 existing FAIL**, with no prior PASS lost.
  Only bidi-010 and bidi-010a/b change, each **10240 differing pixels -> 0**;
  other 461 complete result objects are unchanged. No reference or tolerance
  is changed. Receipt:
  `target/wpt-targeted/absolute-margin-shrink-fit-v1-comparison.json`.
  Binary SHA256:
  `1e5892f6023244f5cc178b06e434409f84b603c3c0879c884cd92186aae4614c`.
  This focused receipt is not full-6548 acceptance or a global remaining count.

### Inline override text-node boundary (after 51ca233)

- Discovery batches 408/416/424/432/440 are each 8/8. Batch 448 is 6/8:
  direction-applies-to-008 differs by 716 pixels; direction-applies-to-012
  by 100. Actual/reference dumps and raw frames are retained in
  `target/wpt-targeted/direction-{452,454}-{actual,ref}*`.
- Direct `Element::set_text_content` fixtures passed but were not RED:
  this method stores text on the element, unlike parsed XML's separate
  Text child. The corrected Text-child DOM fixture fails with SSAP SSAP
  instead of PASS PASS. The XML-parser/CSS-compiler fixture also fails
  before the runtime font-provider stage; compiled CSS retains override.
- Candidate v1 preserves non-normal bidi wrapper boundaries before visual
  lowering. DOM and XML new units pass, but the 19-case sample regresses
  `nested_bidi_overrides_shape_as_one_passive_inline_run` (17/19).
  Its receipt is retained; this candidate is not qualified for publication.
- Candidate v2 also clears successfully consumed principal bidi controls,
  retaining direction for alignment. New DOM unit passes; the existing
  sample returns to 18/19 with no prior PASS lost, including nested override.
  The existing RTL inline-block unit failure remains. Receipts:
  `inline-override-dom-neighbors-after.json` and
  `inline-override-dom-neighbors-v2-after.json` under `target/wpt-targeted/`.
  Current production build completes in **2m04s**. Batch 448 becomes 7/8:
  direction-applies-to-008 is **716 differing pixels -> 0**, with unchanged
  reference and zero tolerance; direction-applies-to-012 remains 100 pixels.
  Final focused qualification: **520 executions, 510 PASS / 10 existing
  FAIL**, with no prior PASS lost. Only direction-applies-to-008 changes;
  other 519 complete result objects are unchanged. Receipt:
  `target/wpt-targeted/inline-override-v2-comparison.json`.
  Production binary SHA256:
  `eeb33719f83a48f7508caee52a045dcb5038460d60f17f95e262319c4ac45768`.
  This is not full-6548 acceptance or a global remaining-failure count.
  The RTL inline-block pixel difference is a 1px horizontal shift, not a
  missing glyph; it remains an independently tracked open failure.

### Generated inline text line width (after ebd1433)

- Open index 454 (`direction-applies-to-012.xht`) has 100 strict differing
  pixels: a 50px black square is shifted right by 1px. Raw difference bounds
  are x=58..108, y=51..100. Layout shows the text at x=108 with width=0,
  versus the reference image at x=58 with width=50.
- DOM generates a 100% text-line width for the definite-width inline-block,
  but `to_taffy_style` correctly ignores authored non-replaced inline widths.
  `leaf_taffy_size` previously reused that auto width for the generated line.
- New unit `generated_inline_line_width_survives_leaf_auto_sizing` gives
  valid RED: the unmarked authored-width assertion passes (auto), but a
  marked generated line still returns auto instead of 100%.
- Candidate marks the DOM-generated text-line constraint and honors its
  semantic width only for that marked Inline Text leaf. Unmarked authored
  widths remain auto. GREEN build completes in **2m53s**, and the new
  unit passes both assertions. The focused runtime sample is **12/12**
  (`generated-line-width-runtime-neighbors-after.json`). Production build
  completes in **2m03s**. Index 454 changes **100 differing pixels -> 0**,
  with the original reference and zero tolerance. The actual text layout
  changes from x=108/width=0 to x=8/width=100; its black glyph paints at the
  reference's inline-end position. Initial batches 448/368/376/384 are 8/8.
  Final focused qualification is **520 executions, 511 PASS / 9 existing
  FAIL**, no prior PASS lost. Only index 454 changes; other 519 complete
  result objects are unchanged. Receipt:
  `target/wpt-targeted/generated-line-width-v1-comparison.json`.
  DOM bidi/RTL sample remains 18/19 with the same existing unit failure and
  no prior PASS lost (`generated-line-width-dom-neighbors-after.json`).
  Binary SHA256:
  `c9140d5fdfba5805dd487b16cde8f5bb0bd8ca4d90ff422d49990ec18c72f39c`.
  This focused receipt is not full-6548 acceptance or a global remaining count.

### Collapsed cell content-height minimum (after dd97e01)

- Discovery batches 464/472 are 8/8; batch 480 is 4/8. Four
  border-applies-to-001..004 cases each differ by 832 pixels. Index 484
  actual/reference layouts and frames are retained as `border-484-*` under
  `target/wpt-targeted/`. Actual table height is 96px, reference 104px;
  both declared 48px cell rows become 46px in the actual layout.
- New real-DOM unit
  `collapsed_group_border_preserves_declared_cell_content_heights`
  gives valid RED, 96px versus expected 104px, with actual Text children.
- The collapsed-cell stretch minimum subtracted padding/half-border insets
  from ContentBox height. Candidate instead adds these insets when converting
  to Taffy's border-box minimum; the explicit BorderBox branch is unchanged.
  GREEN compilation passed (one exact test). The isolated table/collapsed
  unit sample is 77/83: five failures match previously recorded baseline
  names, but `anonymous_table_wrapper_collapses_section_boundaries` now
  reports 116px instead of 68px. This candidate is not qualified.
- The first isolated Chromium anonymous-wrapper diagnostic omitted DOCTYPE:
  its 68px result is quirks-mode evidence, not standards-mode evidence.
  Repeating with `<!doctype html>` verifies CSS1Compat, wrapper 92px and
  28px group heights at y=12/40/68px (including 8px body margin).
  The original direct-IR content-box unit's 68px expectation was therefore
  not a standards-mode oracle; it now explicitly expects 92px/28px groups.
- Root cause: fixed table tracks switch the actual Taffy cell to border-box,
  whereas anonymous/auto tracks retain content-box. Candidate v2 converts
  the declared minimum between authored and actual sizing only when they
  differ, avoiding double-counted insets on content-box tracks.
  Exact anonymous-wrapper GREEN passed after a 2m31s compilation. Replaying
  the same 83 isolated table/collapsed units gives 78 PASS and the five
  previously recorded baseline failures, with no new failed names.
  Receipts: `collapsed-cell-sizing-v2-anonymous.log`,
  `collapsed-cell-sizing-v2-unit-neighbors.json` and
  `collapsed-cell-browser-sizing-modes.json` in `target/wpt-targeted/`.
  This includes the new real-DOM height unit; it is not full-module green.
  Production build passed in 2m06s. Batch 480 is now 8/8: all four
  border-applies-to-001..004 targets have zero different pixels and zero
  maximum difference, using unchanged references and zero tolerances.
  The 552-execution comparison against the published 520-execution receipt
  plus discovery batches 456/464/472/480 completed: 543 PASS, nine existing
  failures. Only the four targets change FAIL to PASS; the other 548
  complete result objects are unchanged and no old PASS is lost.
  Receipt: `collapsed-cell-sizing-v2-comparison.json`, layout blob
  `4203fb75ebed34794954eb00515a0544330da48f`, production SHA256
  `3e5be3c05e08d86186d92930364a7c59a582f441b27f3fa355f341e5be63bfd2`.
  This qualifies the focused repair, not full6548 acceptance. References
  and zero tolerances remain unchanged; clean-SHA replay is still pending.

### Column border conflict inputs (after 34b3936)

- Discovery batch 488 is 5/8. Border-applies-to-005/006 differ by
  1987/2015 pixels; border-applies-to-012 differs by one pixel.
- Native column/group tables are 96px high with no outer half-border
  inset. Isolated standards-mode Chromium confirms 104px height, 2px
  grid inset and 50px rows. Geometry receipt:
  `target/wpt-targeted/border-columns-488-489-chromium-geometry.json`.
  The collapsed-border layout conflict collector currently includes rows,
  row groups and table borders, but omits column/group borders.
- Added `collapsed_column_borders_participate_in_cell_height_minimums`
  to reproduce the 104px minimum for both column and column-group inputs;
  exact RED reproduced 96px versus 104px on the individual-column input.
  The group iteration was not reached after that assertion failed.
  Candidate now collects column/group edge ranges and merges them into
  boundary cells before existing adjacent-grid conflict resolution.
  Exact GREEN compilation completed but the individual-column unit still
  fails: height improved from 96px to 102px, not the required 104px.
  The group iteration remains unexecuted after that first assertion.
  This candidate is not qualified; investigate the remaining row/grid
  settlement before production pixel qualification. No commit/push.
- The 2px deficit traces to post-layout auto-height settlement: its
  original-tree bottom-half query ignores column borders even after the
  layout clone has resolved them onto cells. Candidate v2 adds column/group
  block edges to the table outer-half query and includes those part types
  in the fast-layout resolved projection trigger. Exact GREEN passed after
  2m29s: both individual-column and column-group iterations reach 104px.
  Isolated table/collapsed sample is 79/84, with the same five baseline
  failed names and no new failures. Receipt:
  `target/wpt-targeted/collapsed-column-border-v2-unit-neighbors.json`.
  Production build and pixel qualification are pending; not full-module
  or full6548 acceptance, and no candidate commit/push yet.
- Production build completed in 2m00s. Batch 488 remains 5/8: column-group
  target improves 1987 to 16 differing pixels; column target 2015 to 17.
  Frames show sixteen common missing green pixels, four 2x2 outer corner
  patches at x=8/9 and 110/111, y=51/52 and 153/154. Individual-column
  target has one additional white-text pixel at (111,57). No zero-diff
  target or regression qualification is claimed; investigate collapsed
  border corner replay before commit/push.
- Added `paint_artifact::tests::collapsed_column_edges_cover_outer_corner_quadrants`
  for both column and column-group displays. It checks the union of border
  rectangles at all four outer half-border corner quadrants; exact RED
  reproduced the missing column corner (8.5,51.5). The column-group loop
  was not reached after that first assertion. Candidate extends the top
  and bottom border rectangles to the adjacent inline half-border outer
  edges; exact GREEN passed after 3m17s compilation, both displays executed.
  Replaying 84 table tests yields the same five failed names. Running all
  36 isolated PaintArtifact tests yields 34 PASS and two newly observed
  failures: `auto_positioned_subtree_paints_after_later_normal_flow_content`
  and `inline_fragment_clip_keeps_layout_rect_and_clips_only_paint`.
  No matching modification-before receipt was found, so they are not
  classified as confirmed baseline failures or proven regressions.
  Combined receipt: `collapsed-column-corners-v1-unit-neighbors.json`
  (113/120). Production pixel acceptance and regression qualification
  remain pending; no commit/push yet.
- Production build completed in 2m25s. Strict batch 488 now gives 6/8:
  column-group case 005 improves 1987 pixels to zero; individual-column
  006 improves 2015 to one white-text pixel, and inline-block 012 remains
  one pixel. Both remaining cases are still FAIL at zero tolerance.
  Receipt: `batch-488-column-corners-v1-pixel-check/results.json`.
  The 560-execution comparison will qualify this partial geometry/corner
  repair without claiming the two text-ink failures are fixed or the
  full6548 objective is complete.
- The 560-execution comparison completed: 549 PASS, 11 FAIL, no old PASS
  lost. Only 005 changes FAIL to PASS and 006 improves 2015 to one pixel
  while remaining FAIL; the other 558 complete test objects are unchanged.
  Receipt: `collapsed-column-border-v3-comparison.json`, layout blob
  `4425bacda1d80dce80edb5360a90399da1b3285e`, paint blob
  `03d3ccfe4b8e1470282e883e19b1ce1b5e00a448`, production SHA256
  `f8e036223346415877727967d982e507db21e13717d29aeee702f9c73889dc7b`.
  This qualifies the partial column geometry/corner repair only; the two
  strict text-ink failures remain open. Clean-SHA replay is pending.
- Chromium column-group/reference screenshots have zero pixel difference;
  individual-column/reference has one differing pixel at (111,57), RGBA
  (5,130,5,255) versus green (0,128,0,255). Native at that point is
  (20,138,20,255), so this is not a native parity proof. Screenshots are
  `border-005-chromium.png` / `border-006-chromium.png` in the artifact folder.
- The separate inline-block case's sole pixel is (11,107), native RGBA
  (20,138,20,255) versus reference (0,128,0,255). Its second white text
  `b` starts at x=12,y=103.2 adjacent to the left green border. This is
  evidence for a text-ink investigation, not permission to clip overhang,
  replace fonts, alter references or relax zero tolerance.
- Isolated Chromium actual/reference screenshots for the same inline-block
  case also differ by one pixel: (11,105), (5,130,5,255) versus green
  (0,128,0,255). Retained as `border-012-chromium.png` and
  `border-001-ref-chromium.png` in `target/wpt-targeted/`. This does not
  establish native parity (different y/color), nor turn the strict native
  failure into a PASS. The original zero-tolerance requirement is intact.

### Physical border style and invalid widths (after aa9a8ac)

- Discovery batches 496/504/512/520 each pass 8/8. Batch 528 is 5/8:
  border-bottom-style-001/002 and border-bottom-width-001 each differ by
  2352 pixels. References and zero tolerances are unchanged.
- Valid RED units in CSSStyleDeclaration reproduce a none edge becoming
  3px after a width declaration, and invalid -1px replacing valid 5px.
  Candidate accepts physical side-style longhands, computes visibility
  independently from later width declarations, rejects negative/nonfinite
  widths and leaves an invalid side-width assignment unapplied.
- First 12 isolated border units passed, but a new restoration RED finds
  explicit 0px becoming provisional 3px after none then solid. Candidate
  v2 resolves the last valid side-width declaration independently (including
  uniform and side shorthands); exact restoration GREEN passed for 0px/5px.
  Its 5px iteration was not reached in RED after the 0px assertion failed.
  This is not production pixel qualification, and no commit/push yet.
- Isolated CSSStyleDeclaration sample is 42/43. The sole observed failure
  `negative_margin_and_character_relative_lengths_remain_valid` expects
  Em(4.0) for 4ch but receives Ch(4.0), before testing the negative margin.
  Its dimension parser is not modified by this border change; no matching
  modification-before execution was obtained, so this is not full-module
  green or a confirmed baseline classification. Receipt:
  `target/wpt-targeted/side-border-style-v2-css-style-tests.json`.
  All thirteen isolated border-specific tests pass. Production build and
  strict batch 528 pixel qualification are pending.
- Production build passed in 2m05s. Strict batch 528 is now 8/8:
  border-bottom-style-001/002 and border-bottom-width-001 each improve
  2352 differing pixels to zero (maximum difference also zero).
  The 600-execution comparison against published 560 records plus
  discovery batches 496/504/512/520/528 is running, with CSS source blob
  checks added to the existing DOM/layout/paint/binary/suite seals.
  This is targeted pixel proof, not completed regression qualification or
  full6548 acceptance. No candidate commit/push yet.

- The 600-execution comparison completed: 589 PASS, 11 existing FAIL,
  no old PASS lost. Only the three batch-528 targets change FAIL to PASS;
  the other 597 complete result objects are unchanged. Receipt:
  `side-border-style-v2-comparison.json`, CSS source blob
  `78fd6db00630ea56613954bdc7da58a607d8a1e7`, production SHA256
  `4fbf43eb5029aade085a6576b46cbdf37994626a4651cc216c4c83be9aaeac73`.
  This qualifies the focused repair, not full6548 acceptance. Clean-SHA
  replay is pending; the known text-ink FAILs remain open.

### Invalid relative border width finalization (after c1b53c8)

- Discovery batches 544/552/560 each pass 8/8. Batch 568 is 7/8; index
  571, border-bottom-width-067, differs by 636 pixels. Its negative -1em
  declaration must leave the solid edge's initial medium width unchanged.
  Native actual first inline text has height 0 versus reference 19px.
- Document relative-length finalization reintroduced -20px after the
  CSSStyleDeclaration validity check. Real-DOM test
  `computed_style_cache_tests::invalid_relative_border_width_preserves_the_valid_computed_width`
  reproduces -20px versus 3px. The initial cargo invocation selected the
  wrong test module and ran zero tests; `invalid-relative-border-width-red.log`
  is excluded as RED evidence. `invalid-relative-border-width-real-red.log`
  runs one exact test and fails correctly. Later em/rem/ex and prior-valid
  iterations were not reached after its first assertion.
- Candidate selects the last valid border length declaration before
  relative-unit conversion, rejects negative/nonfinite converted widths,
  and retains earlier valid lengths. Exact GREEN passed, executing all six
  em/rem/ex and prior-valid combinations. The isolated DOM/CSS border
  neighbor sample is 23/23; receipt:
  `invalid-relative-border-width-v1-unit-neighbors.json`.
  References, tolerances and the open font-ink failures are unchanged.

- Production build passed in 2m11s. Strict batch 568 is now 8/8; target
  571 improves 636 different pixels to zero and maximum difference zero.
  The 640-execution comparison against the published 600-execution records
  plus discovery batches 536/544/552/560/568 passed qualification: 629 PASS,
  11 existing FAIL, one FAIL-to-PASS change and no lost PASS. The other 639
  complete ordered result objects are unchanged. Receipt:
  `relative-border-width-v1-comparison.json`; document blob
  `0c326798f2f5f7cfec135c5f0a78e7d1b261b161`, production binary SHA256
  `76f500ef40d9b9967be2c9f11594c696a07f7a22816f6f9615c05737bb4bf5e6`.
  This is a targeted execution sample, not 640 unique cases or full6548
  acceptance. Scoped commit/push is authorized; remote main still matches
  c1b53c8 after fetch. Clean-SHA replay remains required before push.

### Hidden collapsed-border conflict discovery (after 980a6e3)

- Published main is `980a6e35357a7a1d4a899a08098b1c3680a7c93d`.
  Clean-SHA replay batches 568/488/5593 retained complete result objects:
  8/8, 6/8 and 8/8. Receipt:
  `relative-border-width-980a6e3-clean-replay.json`; normal main push and
  remote ref verification succeeded.
- New strict discovery batches 576/584/592/600 each pass 8/8. Batch 608
  is 6/8: border-color-applies-to-006 and -012 each fail by one pixel,
  respectively (111,57) and (11,107), actual RGBA (20,138,20,255) versus
  (0,128,0,255). These match the coordinates/colors of the recorded
  border-applies font-ink failures; they remain FAIL, not tolerated away.
- Batch 616 is 4/8. Indices 620/621/622/623, border-conflict-style-101
  through -104, fail by 2448/2436/2436/2436 pixels. Upstream fixtures apply
  `border-style: hidden` to table row/column-group/column/row-group while
  cells declare red solid borders. The reference contains no table ink.
- Root-cause inspection: CSSStyleDeclaration reduces both none and hidden
  to the same false visibility and zero width; shared Style carries widths
  and colors but no border-style identity. PaintArtifact conflict resolution
  compares widths, so a hidden zero-width track cannot suppress a solid
  cell edge. The native index620 dump records a 205x203 cell grid:
  `border-conflict-620-980a6e3-layout.log` and its actual frame.
  This is failure/root-cause evidence, not a repair or RED/GREEN unit proof.
  Next repair must preserve hidden identity through DOM-to-layout/paint and
  resolve suppression before width comparison, including geometry; merely
  erasing red ink would not close the general border-conflict behavior.
- No production source edits or new commit in the discovery step. The
  parent repository size check passed but excludes vendor and is not a
  W3COS size qualification. Full6548 acceptance remains unproven.

### Hidden border identity retention (unpublished candidate after 980a6e3)

- Exact CSSStyleDeclaration test
  `css_style::tests::hidden_border_style_retains_distinct_conflict_identity`
  runs one test and correctly fails: hidden and none produce equal Style
  values. Receipt: `hidden-border-identity-real-red.log`. Seven property
  iterations were not all reached after the first failing assertion.
- Candidate adds serializable `BorderLineStyle` identity in physical
  top/right/bottom/left order, independently of used widths. Optional edges
  default to unspecified for old native numeric styles. Style paint-effect
  equality now compares this identity. CSSStyleDeclaration retains the last
  valid style declaration through global/side shorthands and physical styles.
- Exact identity GREEN passes all seven property iterations. Added edge
  cascade test covers mixed global styles, side override, later width,
  invalid style and later shorthand reset. Real-DOM computed-style test
  passes for table/tbody/tr/colgroup/col/td. These are computed-style proofs,
  not DOM-to-PaintArtifact or pixel acceptance. The isolated DOM/CSS border
  neighbors pass 26/26; `hidden-border-identity-v1-unit-neighbors.json`.
- Layout and paint conflict collectors still compare widths and do not yet
  consume the retained hidden identity. Explicit inherit/pseudo propagation
  and wire compatibility coverage also remain to verify. Production runner
  has not been rebuilt; indices 620..623 are not claimed fixed. No commit or
  push of this partial candidate; the active full6548 goal remains open.

- Subsequent exact runtime layout RED:
  `layout::tests::hidden_table_parts_suppress_collapsed_cell_layout_widths`
  runs one test and fails at TableRow top edge, 3px versus required 0px.
  Receipt: `hidden-border-layout-real-red.log`; the later row-group/column/
  column-group iterations were not reached. Candidate layout collection now
  carries width plus hidden state, resolves hidden before maximum width and
  retains it across column/group/row/shared/table-edge merges. Fast geometry
  projection also triggers for zero-width hidden parts. Exact GREEN build is
  running; no GREEN, runtime neighbor or production pixel claim yet. Paint
  conflict consumption remains unimplemented. No commit/push.

- Layout exact GREEN is now terminal PASS, reaching all four table-part
  cases. Receipt: `hidden-border-layout-green-v1.log`. The isolated runtime
  border/collapsed-layout/paint neighbors pass 46/46:
  `hidden-border-layout-v1-runtime-neighbors.json`. This selection does not
  include every layout/paint test or establish whole-module green.
- Added exact PaintArtifact regression test
  `paint_artifact::tests::hidden_table_parts_suppress_solid_cell_paint_edges`,
  with real PaintNode table/part/row/cell ownership for the same four parts.
  Its RED compilation is live; no RED outcome is claimed before terminal
  execution. Layout source is frozen during that build. Native production
  pixels remain unverified and this candidate remains unpublished.

- Paint exact RED is now terminal and runs one failing test: TableRow
  leaves all four cell edge widths at 3px versus expected 0px. Later part
  iterations were not reached. Receipt: `hidden-border-paint-real-red.log`.
  Native index621 structure also confirms an empty colgroup is an implicit
  column, without explicit col children:
  `border-conflict-621-980a6e3-layout.log` and actual frame.
- Candidate PaintArtifact conflict resolution now transfers hidden identity
  to boundary cells for row/group and explicit/implicit column/group parts,
  propagates suppression across adjacent cell edges and protects it during
  table boundary width comparisons. Column coverage uses nearest-table and
  parent ownership; only hidden column edges enter this added projection.
  Exact paint GREEN is compiling; no success or production pixel outcome is
  claimed yet. No source mutation during that build, commit or push.

- Paint exact GREEN is terminal PASS and reaches all four part iterations:
  `hidden-border-paint-green-v1.log`. The same isolated runtime border/
  collapsed neighbor selection again passes 46/46, with no lost prior PASS:
  `hidden-border-paint-v1-runtime-neighbors.json`. The new paint test's name
  does not match that neighbor selection and is proved separately by its
  exact execution; do not count it as one of the 46.
- Production runner rebuild is live:
  `hidden-border-v1-production-build.log`. Source is frozen. Next required
  evidence is strict batch616 acceptance plus ordered-result regression
  comparison against the published binary. Four WPT fixes, production pixel
  acceptance and full6548 closure remain unclaimed. No candidate commit/push.

- Production build is terminal PASS in 2m05s. Strict target batch616 is now
  8/8: indices620..623 improve 2448/2436/2436/2436 differing pixels to zero,
  with maximum channel difference zero and both allowances unchanged at
  zero. Receipt: `batch-616-hidden-border-v1/results.json`.
- The 688-execution qualification is live, comparing the previous 640
  complete records plus discovery batches576/584/592/600/608/616. It also
  freezes the shared Style source blob along with DOM/CSS/layout/paint,
  production binary and pinned suite fingerprints. Target pixel proof does
  not establish full qualification or full6548 acceptance. Source remains
  frozen and no candidate commit/push has occurred.

- While the same 688 comparison remains live, the existing shared Style
  test selection `style::tests::` passes 2/2 (33 filtered):
  `hidden-border-v1-std-style-tests.log`. This is not a new wire round-trip
  test or whole-std/module compatibility proof. Static inspection confirms
  explicit border inherit/pseudo copy paths still need coverage for the new
  identity; no broadened compatibility claim or source edit during the
  frozen production qualification. `git diff --check` passes.

- Qualification is terminal PASS: 688 executions, 675 PASS, 13 retained
  FAIL. Only the four batch616 targets change FAIL to PASS; the other 684
  complete ordered objects are unchanged, with no lost PASS. Receipt:
  `hidden-border-v1-comparison.json`. Binary SHA256:
  `3ac46dc750515b7dfcfca7b4053216b95f7c1cf77719f341856fc1822f4d1a8d`.
  Shared Style blob `55eac2f796ecf55569a94ebb065c962c20db7ea7` is sealed
  alongside DOM/CSS/layout/paint blobs in that receipt. These executions
  overlap and do not represent 688 unique cases or full6548 closure.
- User authorizes scoped small commits and normal main push. Fetch confirms
  remote main still equals base980a6e3. This repair is ready for scoped commit
  and clean-SHA replay before push; unverified inherit/pseudo/wire boundaries
  and the 13 strict failures are not converted into completion claims.

### CSS initial border line styles (candidate after 4e817e0)

- Published main `4e817e0c7ba2b8fd3dda2fc4152be1ed37b6cac0` is verified.
  Clean-SHA batches616/488/5593 replay 8/8, 6/8, 8/8 with complete ordered
  objects unchanged; `hidden-border-4e817e0-clean-replay.json`. Normal main
  push completed. Original vendor checkout and parent/submodule pin remain
  untouched.
- Discovery batch624 is 6/8. Index626 border-conflict-style-107 fails by
  52740 pixels (equal-width color owner precedence across table parts), and
  index630 border-left-003 fails by 8320 pixels. The latter declares a blue
  left shorthand, left solid style and global width5px; other edge styles
  remain initial none. Production actual frame/layout retained:
  `border-left-630-4e817e0.frame` and layout log.
- Exact CSS RED runs one test, producing [5,5,5,5] versus [0,0,0,5]:
  `initial-border-none-real-red.log`. Candidate initializes empty CSS styles
  with none identity, retains positive numeric native borders and resets
  omitted shorthand line styles to none. Target unit GREEN is terminal PASS:
  `initial-border-none-green-v1.log`. Numeric-native wrapping guard passes.
- Initial isolated border neighbors are 27/28:
  `initial-border-none-v1-unit-neighbors.json`. The old negative-width unit
  expected computed5px without a line style; this is an incorrect oracle,
  not claimed an unchanged prior PASS. Isolated standards Chromium141 proves
  declared5px remains after invalid -1px, computed0px with initial none, and
  computed5px after solid. Receipt: `initial-border-none-chromium-oracle.json`.
  The unit now preserves the declaration5px assertion and separately checks
  none0px and solid5px. Its rebuilt exact test is live; no completed revised
  neighbor or pixel qualification is claimed yet. No commit/push of candidate.

- Corrected-oracle exact test is now terminal PASS, and the revised isolated
  border neighbors pass 28/28: `initial-border-none-v2-unit-neighbors.json`.
  This includes the explicitly corrected fixture and is not described as
  28 unchanged baseline tests. Production runner build is live; index630
  zero-pixel acceptance and ordered pixel regression remain pending. Equal
  color-owner precedence failure626 remains open. Source frozen during build.

- Production build is terminal PASS in 2m09s. Target batch624 now passes
  7/8; border-left-003 improves 8320 differing pixels to zero with maximum
  difference zero and both allowances still zero. Equal-color owner
  precedence failure626 remains strict FAIL. Receipt:
  `batch-624-initial-border-none-v1/results.json`.
- The same frozen-source candidate is now in a 696-execution qualification
  against the published688 records plus batch624 discovery. Required target
  is left-003; hidden-conflict and earlier border/bidi batches are retained.
  Complete comparison, clean-SHA replay and full6548 closure remain pending.
  No candidate commit/push has occurred.

- Production color-owner build is terminal PASS in2m06s. Strict batch488
  remains6/8 with all eight complete results unchanged from the published
  initial-none records. Batch624 remains7/8; only failure626 improves
  52740 to43940 differing pixels, still strict FAIL. Pure red pixels drop
  from13850 to0, but this is partial ink evidence, not case acceptance.
  Receipts: `batch-488-collapsed-color-owner-v1/results.json` and
  `batch-624-collapsed-color-owner-v1/results.json`.
- Same-binary actual/reference layout dumps additionally prove top-margin
  mismatch: actual paragraph y24 and first float y59.2, reference paragraph
  y16 and green square y51.2. Thus the earlier Chromium top-position
  difference also affects the native reftest, not merely browser typography.
  Actual/ref frame and logs: `border-color-owner-626-v1` and `-v1-ref`.
  Flow grouping, margin collapse and BR clear remain to diagnose/fix. No
  completed WPT qualification, candidate commit or push is claimed.

- While that qualification remains live, index626 diagnostic evidence shows
  multiple independent gaps, not only color-owner precedence. Actual frame
  and layout receipt `border-color-owner-626-initial-none-v1-layout.log`
  retain the current production binary's geometry. Native first floating
  table starts at (8,59.2), and subsequent BR/clear float groups do not form
  four rows of four. Isolated standards Chromium141 at800x600 places all16
  tables at x8/58/108/158, y50/100/150/200, each50x50:
  `border-color-owner-626-chromium-layout.json`. This is an authoritative
  diagnostic comparison, not native acceptance or a completed fix. Next
  repair must independently close color ownership and floating-table/BR
  clearance geometry. Current candidate source remains frozen.

- Initial-border-none qualification is terminal PASS: 696 executions,
  682 PASS, 14 retained FAIL. Only border-left-003 changes FAIL to PASS;
  the other 695 complete ordered result objects are unchanged, no lost PASS.
  Receipt: `initial-border-none-v1-comparison.json`; CSS source blob
  `fcf0bde508037e4bafbdedd276b5745022c6112b`, binary SHA256
  `d1dc5942bb1d0947331fde59d6af4e71ed1e61bae644e2c413022c805025c047`.
  The explicitly corrected unit oracle is disclosed separately above and
  not substituted for unchanged production WPT results. Executions overlap;
  this is not 696 unique cases or full6548 acceptance.
- User authorizes small scoped commit/normal main push. Fetch confirms
  remote main still equals4e817e0. Clean-SHA replay is required before push;
  failure626 and existing font/ink failures remain open.

### Collapsed color-owner precedence (candidate after 467bfee)

- Published main is `467bfeed9cf58f4ee7b8b8a4ee776453e1c06237`.
  Clean-SHA replay624/616/5593 is 7/8, 8/8, 8/8 with complete objects
  unchanged: `initial-border-none-467bfee-clean-replay.json`. Normal push
  and remote ref verification completed; current worktree was clean.
- Exact PaintArtifact RED runs one failing test:
  `paint_artifact::tests::collapsed_column_color_defers_to_cell_and_row_owners`.
  The column retains independent unspecified side widths (falling back to
  authored25px) instead of zero after ownership transfer; later cell-owner
  iteration was not reached. Receipt: `collapsed-color-owner-real-red.log`.
- Candidate includes visible columns/groups in the existing shared boundary
  resolution and orders table parts by row, row-group, column, column-group
  precedence. Equal-width edges retain the existing higher-priority cell or
  part color; cells own the resulting ink and source rings are cleared.
  The GREEN test now covers column/column-group versus cell/row ownership.
  Exact GREEN build is live; no GREEN or production pixel claim yet.
- Floating-table/BR-clear geometry failure in626 remains independently open,
  along with font/ink debt. No commit/push of the current candidate. Source
  is frozen during its exact test build; full6548 acceptance remains unproven.

- Exact color-owner GREEN is terminal PASS, reaching all four column/group
  and cell/row combinations: `collapsed-color-owner-green-v1.log`.
  Expanded isolated runtime border/collapsed/hidden neighbors pass48/48;
  the prior46 passing names remain passing:
  `collapsed-color-owner-v1-runtime-neighbors.json`. This is not whole-module
  green or native WPT pixel proof. Production build is now live:
  `collapsed-color-owner-v1-production-build.log`. Source remains frozen;
  recent column corner pixels and failure626 need production verification.
  No candidate commit/push has occurred.

- Flow investigation adds real DOM test
  `document::image_component_tests::clear_breaks_between_floating_tables_keep_their_clearance_identity`.
  All three clear-both breaks and16 floating-table identities survive
  lowering. The first cargo selector used the wrong module and ran0 tests;
  `floating-table-clear-identity-red.log` is not RED/PASS evidence. After
  exe `--list` verification, the exact real test runs1 and PASS:
  `floating-table-clear-identity-real-test.log`. This excludes identity loss
  in that minimal shape, not every styled/coalesced DOM path.
- Added layout test with four50px floats per anonymous group and three
  clear-both forced-break markers. Expected grid positions follow the
  isolated Chromium50px clearance geometry; exact test compilation is live:
  `floating-group-clear-real-test.log`. No outcome or fix is claimed before
  terminal execution. Margin-collapse mismatch remains independently open.

- Clearing-group exact test is now terminal RED, runs1 test and fails at
  row1 column0: y69.2 versus required50. Later rows were not reached.
  Receipt: `floating-group-clear-real-test.log`. Candidate clear handling
  constrains the forced-break line bottom against active float bottoms,
  deriving its floor from preceding normal-flow content rather than the
  synthetic group height. Ordinary break strut height is retained. Exact
  GREEN compilation is live: `floating-group-clear-green-v1.log`. No GREEN,
  neighbor, production pixel or case626 completion claim yet. No commit/push.

- Clearing-group exact GREEN is terminal PASS and reaches all16 grid
  positions: `floating-group-clear-green-v1.log`. Expanded isolated runtime
  float/clear/strut/border/collapsed selection is130/131; all48 prior passing
  names still pass. The initial selection omitted one prior hidden-paint
  name; that was a verifier coverage mismatch, not a lost execution PASS.
  The missing exact test was executed and merged before sealing:
  `floating-group-clear-v1-runtime-neighbors.json` has no lost prior PASS.
- Newly observed failing unit
  `leading_float_margin_does_not_collapse_with_its_containing_block` reports
  y8 versus80. Its fixture has no clear declarations, so the changed clearing
  branch is not directly implicated; no before execution was captured, so
  neither baseline failure nor introduced regression is claimed. Assertion
  remains unchanged and this is not whole-module green.
- Production runner build is live:
  `floating-group-clear-v1-production-build.log`. Current source remains
  frozen for actual626 and neighbor pixel verification. Margin collapse,
  the unit above and full6548 closure remain open. No candidate commit/push.

- Clearing-group production build is terminal PASS. Batch624 stays7/8;
  failure626 still43940 differing pixels, so the all-group unit GREEN did
  not establish real-page improvement. Same-binary dump shows BR26 moves
  y109.2 to90, while following direct floating tables remain y128.4; later
  clear markers also move but tables retain wrong bands. Receipts:
  `batch-624-floating-group-clear-v1/results.json` and
  `border-color-owner-626-clear-v1-layout.log`/actual frame.
- Added separate layout test for the observed mixed shape: one anonymous
  group, a clear-both break and four direct floating tables carrying the
  after-inline source marker. Exact compilation is live:
  `direct-floating-table-clear-real-test.log`; no outcome claimed yet.
  Source is frozen. No completed pixel qualification or candidate push.

- Mixed direct-floating-table exact test is terminal RED, runs1 test and
  fails at the first direct table; receipt:
  `direct-floating-table-clear-real-test.log`. Candidate restricts the
  after-inline exemption to non-clearing/non-forced prior inline content.
  A forced-break marker already owns a full line strut, so the following
  float no longer adds text half-leading to that marker. Exact GREEN build
  is live: `direct-floating-table-clear-green-v1.log`. No GREEN or real-page
  pixel improvement is claimed yet. Other margin/flow gaps remain open;
  source frozen during build and no candidate commit/push.

- The first mixed-table candidate is terminal FAIL: `(200, 48.4)` instead
  of `(0, 50)`, unchanged from RED. The diagnostic rerun also fails and
  records the clearing break at y30.8, height19.2 (bottom50), while all
  four subsequent direct floats remain at y48.4. Receipts:
  `direct-floating-table-clear-green-v1.log` and
  `direct-floating-table-clear-diagnostic-v1.log`. This proves the break
  clearance itself is correct in this minimal shape; imported-group mode
  skips the subsequent ordinary float flow-floor settlement. The next
  candidate enables that settlement after a clearing forced break, without
  adding ordinary-text half-leading to the break. Exact validation pending;
  no production pixel improvement or candidate publication claimed.

- The second mixed-table candidate is terminal GREEN: exactly1 test passed,
  all four direct tables meet the unchanged `(column*50, 50)` assertion.
  Receipt: `direct-floating-table-clear-green-v2.log` (build2m31s). This is
  minimal layout evidence, not a WPT pixel PASS. The132-test isolated
  neighbor replay and production runner build are started; source frozen.

- Second-candidate neighbors finish132 executions,131PASS and the same
  previously observed leading-float-margin failure; no prior PASS lost.
  Production build succeeds, but624..631 remain7PASS/1FAIL with every
  complete result object unchanged (`floating-group-clear-v2-pixel-comparison.json`).
  Actual626 dump shows empty Inline Text nodes27/52 following clearing
  breaks26/51. They overwrite the normal-flow predecessor used by the
  minimal fix. The test now covers both absence and presence of this empty
  text, with unchanged position assertions; RED validation started.
  No candidate publication or real-page closure claimed.

- Empty-text variant is terminal RED at the first direct table:
  `(200,48.4)` versus `(0,50)`, while the no-empty-text variant passed
  first in the same test. Receipt: `direct-floating-table-empty-text-red.log`
  (build2m32s). Candidate reuses `inline_line_has_in_flow_content` to keep
  collapsed empty inline content from replacing float flow predecessors.
  Nonempty/preserved text, clearing nodes and internal float struts retain
  their existing path. Exact GREEN compilation started; not yet qualified.

- Empty-text candidate exact test is terminal GREEN: no-empty and empty
  variants both satisfy all four unchanged table positions. Receipt:
  `direct-floating-table-empty-text-green.log`, build2m38s. Isolated
  neighbors remain132executions/131PASS, the same previously observed
  leading-float-margin failure and no prior PASS lost:
  `floating-group-clear-v3-runtime-neighbors.json`. Production runner
  build started, source frozen; actual626 pixel result still pending.

- Third-candidate production624..631 remain7PASS/1FAIL, but actual626
  different_pixels43940→23940; the other7 complete objects are unchanged,
  no lost PASS. Receipt: `floating-group-clear-v3-pixel-comparison.json`.
  Dump `border-color-owner-626-clear-v3-layout.log` confirms direct rows
  now start109.2 and159.2. The final anonymous float group still starts
  147.6, above its preceding clearing-line bottom209.2; its floats collide
  with the third row. The mixed minimal test now adds a final float group
  after another clearing break; unchanged earlier assertions are retained
  and a new100px row assertion is added. RED validation started. Parent
  paragraph/reference margin mismatch is still a separate open gap.

- Trailing-group variant is terminal RED: empty-text case last-row first
  float `(200,88.399994)` instead of `(0,100)`; no-empty-text case passed
  first. Receipt: `direct-floating-table-trailing-group-red.log`,2m33s.
  Candidate applies the already computed anonymous-group static flow
  position in both directions, rather than silently dropping downward
  settlement. Exact GREEN compilation started; no production claim.

- Trailing-group exact validation is terminal GREEN (2m31s): both variants
  retain the direct-row50px assertions and meet the final-group100px
  assertions. Receipt: `direct-floating-table-trailing-group-green.log`.
  Isolated132-neighbor replay remains131PASS, the same previously observed
  leading-float-margin failure, no old PASS lost:
  `floating-group-clear-v4-runtime-neighbors.json`. Production build is
  started; actual pixel improvement is not yet known, source frozen.

- Fourth-candidate production624..631 remains7PASS/1FAIL; actual626
  different_pixels23940→7140, other7 complete objects unchanged and no
  lost PASS (`floating-group-clear-v4-pixel-comparison.json`, compared
  against the earlier43940 baseline). Actual dump confirms all four rows
  at59.2/109.2/159.2/209.2, now properly aligned. Remaining paragraph
  actualy24 versus referencey16 shifts the entire square by8px. The actual
  body enters mixed-inline/block Flex fallback, which prevents native
  first-child margin collapse; reference stays block layout. Added a
  minimal plain-versus-mixed leading paragraph margin test; RED started,
  no completion or publication claimed.

- Leading-margin exact test is terminal RED: plain variant passes at16,
  mixed variant fails at24 (`mixed-float-leading-margin-red.log`,2m37s),
  reproducing the actual/reference discrepancy. Candidate promotes the
  first in-flow block's top margin into the mixed fallback parent's top
  margin using positive/negative margin collapse, then removes that child's
  duplicate internal top margin. Float/positioned/overflow/BFC and genuine
  Flex/Grid parent boundaries are excluded, as are top border/padding and
  clearing first children. Exact GREEN compilation started; source frozen.

- Leading-margin exact GREEN completes2m31s, plain and mixed paragraph
  bothy16 (`mixed-float-leading-margin-green.log`). Isolated neighbors
  finish133executions/132PASS, the same previously observed leading-float
  margin failure and no prior PASS lost:
  `floating-group-clear-v5-runtime-neighbors.json`. Production build is
  live, source frozen; actual626 strict pixels remain to be verified.

- Fifth-candidate production624..631 is terminal8PASS/0FAIL; actual626
  different_pixels7140→0 and max_difference0 under zero tolerances, the
  other7 complete objects unchanged. Receipt:
  `floating-group-clear-v5-pixel-comparison.json`. This closes the focused
  owner/float-clear/empty-inline/leading-margin chain at production pixels,
  not the6548-suite goal. The696-execution sealed directed qualification
  against published initial-border-none-v1 starts with target624; sources
  remain frozen, no candidate commit/push before qualification finishes.

- While696 qualification remains live, four separate next8 discovery
  batches632/640/648/656 each finish8PASS/0FAIL with the same candidate
  production binary and frozen suite; receipts:
  `batch-{632,640,648,656}-collapsed-color-clear-v1-discovery/results.json`.
  These32 executions are additional focused evidence, not part of the696
  old-PASS qualification or a6548 completion claim. No source mutation or
  candidate publication during qualification.

- Next discovery batches664/672/680/688 each finish8PASS/0FAIL, another32
  focused executions with the same production binary during source-frozen
  qualification. Combined next8 discovery632..695 is64PASS/0FAIL; it is
  additional coverage, not a replacement for the696 old-PASS comparison.
  No candidate commit/push before qualification and clean-SHA replay.

- Candidate qualification is terminal696executions/683PASS/13FAIL, only
  actual626 FAIL→PASS and all other695 complete objects unchanged; no
  prior PASS lost. Receipt: `collapsed-color-clear-v1-comparison.json`,
  base467bfee, binarySHA256
  `b4f856078c1c5d874ff44df06bd01815aad9638c0d636179119151e7f104f098`.
  Five source blobs and frozen suite hash stay sealed through completion.
  The13 strict font/ink failures are this selected sample, not the current
  remaining count of6548. Parent `pnpm files:size:check` passes but excludes
  vendor W3COS; it does not prove renderer file-size compliance. Scoped
  normal commit is authorized by the user's small-step commit/push request;
  clean-SHA focused replay is still required before normal main push.

- Publishedb4b8aa5 next8 discovery696/704/712/720 and728/736/744/752/760/
  768/776/784/792/800/808/816 each finishes8PASS/0FAIL (128executions).
  The next824 batch finishes7PASS/1FAIL and discovery stops immediately:
  actual826 `border-right-width-095.xht`,7494different_pixels/max255.
  Sealed receipts `discovery-{728,792}-b4b8aa5-sealed.json` and individual
  batches retain fixed suite, source and binary evidence. Actual826 dump
  shows bodyy16, paragraphy16, squarey52; reference body/rectangle/paragraph
  y8, squarey44. Actual available width668 reflects the body's96px right
  border and20px padding; this alone does not prove the div's inherited
  border, and inheritance failure is not established. Suspected remaining
  root is reference leading-float/normal-flow margin settlement; browser
  oracle and minimal RED still needed. No next candidate patch or commit.

- Isolated Chromium141/DPR1/800x600 loads the original XHTML files directly:
  actual body/p y16 and divy52 with divcomputed right border96px solid;
  reference body/rectangle/p y16 and squarey52. Receipt:
  `border-right-inherit-826-chromium-oracle.json`. Native actual positions
  match this oracle; native reference body/rectangle/p y8 and squarey44
  do not. Added minimal leading-float/first-in-flow margin-collapse test
  with unchanged browser coordinates; exact RED compilation started.
  No border-inheritance parser change or production fix claimed.

- Leading-float minimal test is terminal RED at bodyy8 versus16;
  subsequent assertions were not reached. Receipt:
  `leading-float-inflow-margin-red.log`. Candidate extends first-in-flow
  top-margin promotion to a leading float/anonymous float-group prefix,
  retaining the same authored border/padding/BFC eligibility checks, and
  removes the first in-flow child's internal top margin in ordinary Block
  as well as mixed fallback paths. Exact GREEN build started; no pixel
  closure or candidate publication claimed yet.

- Leading-float exact GREEN completes2m31s: body/leading float/paragraph
  all16, trailing float52 (`leading-float-inflow-margin-green.log`).
  Isolated134-neighbor replay finishes133PASS, the same previously observed
  containing-block leading-float-margin failure and no prior PASS lost:
  `floating-group-clear-leading-float-v1-runtime-neighbors.json`. Production
  runner build started, source frozen; strict actual826 pixels still pending.

- Leading-float production824..831 completes8PASS/0FAIL: actual826
  different_pixels7494→0/max0 under zero tolerances, other7 complete objects
  unchanged, no lost PASS (`leading-float-margin-v1-pixel-comparison.json`).
  The896-execution sealed directed qualification now starts with824,
  covering prior696 plus the200 next-discovery executions632..831, against
  their exact published/candidate-baseline receipts. This is not a6548 run.
  Sources frozen; no candidate commit/push before terminal qualification.

- First896 qualification attempt terminates with verifier ENOENT, not a
  rendering PASS regression: discovery baseline override erroneously also
  catches high core start1358. The override is now bounded632..824. Resume
  validates all source/binary/suite SHA256 values against the focused pixel
  receipt before reusing completed same-candidate batches; missing batches
  execute normally. Original error log retained, resume log separate.
  Source unchanged, qualification not yet complete; no candidate publication.

- Same frozen candidate runner next8 discovery832..839 finishes8PASS/0FAIL
  while resumed qualification continues. Receipt:
  `batch-832-leading-float-margin-v1-discovery/results.json`. This is
  extra directed coverage, not part of896 old-PASS comparison or suite
  completion. Production sources remain unchanged.

- Next8 discovery840 stops at6PASS/2FAIL; shorthand002 has25088 differing
  pixels, shorthand00312544. Sealed receipt:
  `discovery-840-leading-float-margin-v1-sealed.json`. Viewed actual002 PNG
  is a red bar where the reference is green. Higher-specificity border/
  border-top shorthand omits color and should reset it to currentColor;
  lower-specificity border-color:red is incorrectly retained. This next
  root is read-only analysis while896 qualification runs; no color patch,
  minimal RED or next candidate publication yet.

- Next-root Chromium141 oracle directly loads original XHTML: shorthand002
  has all four borders green/solid16px; shorthand003 topgreen/solid16px
  while the other3 remain red/none0px. Receipt:
  `border-shorthand-color-840-841-chromium-oracle.json`. This binds the
  missing-color reset and single-side scope without altering fixtures or
  tolerance. Current896 qualification remains live; production sources
  frozen, next color RED/patch not started.

- Resumed qualification is terminal896executions/883PASS/13FAIL, only826
  FAIL→PASS, other895 complete objects unchanged and no prior PASS lost.
  Receipt: `leading-float-margin-v1-comparison.json`. Fixed suite and five
  source blobs/binary stay sealed, interrupted verifier error remains
  separately retained. The13 strict failures belong to this sample, not
  the6548 remaining count. Scoped commit authorized by small-step request;
  clean-SHA focused replay remains required before normal main push.

- After published0a09e37, added CSSStyleDeclaration regression for omitted
  shorthand color: initial textblue and borderred, apply border or any
  physical-side shorthand, then change textgreen. Affected edges mustgreen;
  unaffected edges must remainred. This tests final currentColor rather
  than a snapshot at shorthand application. Exact RED compilation started;
  no parser/compute patch or candidate publication yet.

- Omitted-color CSSStyleDeclaration test is terminal RED: border top red
  instead of finalgreen (`border-shorthand-current-color-real-red.log`,
  18.62s). Candidate resolves winning physical color declarations and valid
  shorthand omitted/currentColor against final text color, with a shared
  shorthand color classifier; Document repeats resolution after inherited
  font/color facts are known, supporting relative widths. First GREEN
  attempt fails compilation E0506 (no tests executed); corrected by
  computing all four colors before mutating Style. GREENv2 compilation
  started; no production pixel qualification or candidate publication yet.

- GREENv2 finishes17.77s, exactly1 test PASS: global and all four physical
  shorthand cases resolve affected borders to finalgreen, unaffected edges
  remainred. Receipt: `border-shorthand-current-color-green-v2.log`.
  Stylesheet/inherited-color/invalid-value neighbors and production840/841
  zero pixels are still required; no broad success or publication claimed.

- Added stylesheet/inherited-green/relative1em regression with uniform
  fallback and physical-side scope assertions. Initial attempt fails at
  inheritedgreen before border assertions: raw body style-attribute setup
  did not populate computed declarations in this direct Document test.
  Corrected to register a body stylesheet rule, matching neighboring
  direct Document tests; all expected colors/widths remain unchanged.
  Corrected validation started; no inherited-color root failure claimed.

- Corrected stylesheet test fails at uniform fallback red instead ofgreen
  after inherited text and physical topcolor assertions pass; receipt:
  `border-shorthand-inherited-color-v2.log`. Candidate now resolves uniform
  fallback from the last winning global color/shorthand as well, retaining
  single-side scope. GREENv3 completes17.90s, exactly1 inherited/relative
  stylesheet test PASS (`border-shorthand-inherited-color-green-v3.log`).
  Existing omitted-color CSSStyle and invalid-shorthand no-partial-mutation
  tests each independently PASS on the same executable. Broader directed
  neighbors and production840/841 strict pixels remain unverified.

- Omitted-color directed DOM neighbors finish30/30PASS: all28 prior
  initial-border-none neighbor names still PASS plus the2 new regressions,
  invalid-shorthand test included, no lost PASS. Receipt:
  `border-shorthand-current-color-v1-unit-neighbors.json`. Production runner
  build started; all production sources frozen until focused840..847
  strict pixel verification. No candidate commit/push yet.

- Current-color production840..847 is terminal8PASS/0FAIL. Target840
  different_pixels25088→0, target84112544→0, bothmax0 at zero tolerances;
  other6 complete objects unchanged, no lost PASS. Receipt:
  `border-shorthand-current-color-v1-pixel-comparison.json`. Sealed912
  directed qualification starts with840, covering prior896 plus832/840
  discovery against exact baselines. Sources frozen, no candidate
  commit/push until terminal qualification and clean-SHA replay.

- Current-color qualification stops at batch600 after672 executions
  (659PASS/13FAIL): border-color-011.xht and border-color-012.xht lost
  their prior PASS, different_pixels4932 and4620 respectively. This is
  a partial failed qualification, not a completed912 receipt. Both fixtures
  inherit an omitted border color from `border: none` and expect currentColor
  to resolve on the receiving green element. Candidate eager resolution to
  parent red is the suspected regression; preserving inherited keyword
  provenance requires a minimal regression test before changing code.
  No candidate commit/push. Separate sealed848..911 discovery64/64PASS
  does not override this regression. Receipt:
  `border-shorthand-current-color-v1-comparison.json`.

- Inherited-currentColor minimal unit is real RED: a green block child
  incorrectly receives red from parent `border: none`; receipt
  `inherited-border-current-color-real-red.log`. Candidate preserves a
  per-edge computed keyword mask independently of used RGBA in Style,
  with serde default None for native styles lacking CSS provenance and
  equality including the mask. Document derives each winning edge's
  keyword and resolves inherited currentColor on the receiving element.
  Undeclared initial edges are not made authored physical color owners.
  GREENv1 passes block/inline child variants. Additional GREENv2 test
  passes mixed currentColor/explicit-blue edges through two generations,
  including global shorthand inheritance (18.66s). All32 isolated DOM
  neighbors PASS, prior28 PASS retained, no lost PASS; receipt
  `border-shorthand-current-color-v2-unit-neighbors.json`. Productionv2
  build started. Production600/840 strict pixels and qualification remain
  unverified; no candidate commit/push.

- CurrentColorv2 production build completes2m05s. Strict batch600..607
  is8PASS/0FAIL, all8 complete result objects identical to published
  leading-float-margin-v1, restoring both lost inheritance PASS with zero
  pixels/max0. Strict840..847 is8PASS/0FAIL: target84025088→0 and
  target84112544→0 retained, other6 objects unchanged, no lost PASS.
  Receipts: `inherited-border-current-color-v2-pixel-comparison.json`,
  `border-shorthand-current-color-v2-pixel-comparison.json`. Production
  sources frozen for fresh912 qualificationv2, with600/840 run first.
  No candidate commit/push before terminal qualification and clean-SHA
  replay. No full6548 zero-failure evidence yet.

- CurrentColorv2 additional848..911 replay is64PASS/0FAIL; all64 complete
  objects identical to v1 discovery, no lost PASS. Separate durable
  comparison: `border-current-color-v2-extra64-comparison.json`.
  Subsequent912..975 bounded discovery executes64,61PASS/3FAIL, stopping
  at968: border-width-011/012 each102504 pixels, border-width-applies-to-012
  one pixel. Seal: `discovery-912-border-shorthand-current-color-v2-sealed.json`.
  This is newly discovered coverage, not proof of candidate regressions.
  Read-only original-file Chromium141 oracle sees body none/hidden used0,
  child inherited width0, while fixed reference has32px; thus this browser
  itself does not match these updated inheritance reftests. Current editor
  draft border-width has absolute-length computed values and only used
  width0 for none/hidden (https://drafts.csswg.org/css-backgrounds-3/#border-width).
  Native969 dump shows body179.2 high and childx40/y56, consistent with
  hidden initial body borders leaking into geometry after relative width
  finalization. This remains a hypothesis pending minimal RED after the
  current frozen qualification/publication boundary. Oracle:
  `border-width-inherit-969-970-chromium-oracle.json`; dump:
  `border-width-969-layout.log`. No production source changed during912
  qualificationv2, no candidate commit/push yet.

- Width969 read-only reference dump confirms body/p x8/y16 andp784x83.2,
  while actual bodyx8/y8,p x40/y56,720x83.2. ActualPNG inspected: unrequested
  black32px bodyframe around greenpframe. CSSStyleDeclaration::to_style
  masks nonvisible physical widths to0, but Document relative-width
  finalization subsequently overwrites those physical widths with32;
  runtime layout consumes numeric physical widths directly. This closes
  the source/geometry chain for the bodyframe leak, but remains untested
  as a fix; production source stays frozen for current-color qualification.
  Reference dump: `border-width-969-ref-layout.log`.

- CurrentColorv2 qualification is terminal912 executions899PASS/13FAIL.
  Exactly840/841 FAIL→PASS; other910 full ordered result objects unchanged,
  no lost PASS. Additional sealed64 remain64PASS/full objects unchanged.
  These are976 directed executions, not6548 remaining-failure statistics.
  Receipt: `border-shorthand-current-color-v2-comparison.json`.
  Qualified binarySHA256:
  `d0f069378566e4f34ad9870d83648efcd22fc98a9c10ec850dd5c7b23bf3fb9e`.
  Preparing scoped4-file commit under user's small-step commit/push
  authorization; clean-SHA48 replay and actual push still pending.

- CurrentColor fix published as `4f772eb61496de43d16492739dbcdd6d51d561ce`.
  Normal push0a09e37→4f772eb terminalsuccess; remote main verified exactSHA.
  Clean-SHA48 replay840/824/624/488/608/5593 has all full result objects
  identical qualification; additional600 clean8/8 identical,56 directed
  executions total. Five source blobs and binary/suite hashes sealed.
  Receipts: `border-shorthand-current-color-4f772eb-clean-replay.json`,
  `border-shorthand-current-color-4f772eb-extra-600-clean-replay.json`.
  No full6548 current acceptance claim. Next uncommitted small step adds
  runtime unit `nonvisible_css_border_widths_do_not_enter_box_geometry`
  checking none/hidden used offsets0 versus solid/native-unspecified32,
  and preserving original computed width32. RED run started, terminal
  result pending; no fix yet.

- Nonvisible-border geometry test is real RED: child(32,32) versus(0,0)
  for Some(None); terminal1FAIL/1473filtered on published-current-color
  production sources plus new test. Receipt: `nonvisible-border-box-real-red.log`.
  Candidate shares Style::resolve_used_border_widths across owned layout
  and paint snapshots, physical none/hidden widths0, uniform width0 only
  when all edges nonvisible; source computed width stays32 and line-style
  identity retained. Layout normalization precedes collapsed-table conflict
  resolution, so hidden still participates. New paint snapshot regression
  covers none/hidden/solid/native-unspecified; GREEN compile started.
  Production968 strict pixels, directed regression and performance remain
  unverified; no candidate commit/push.

- Nonvisible-border GREENv1 geometry and paint snapshot tests both PASS.
  Runtime136 directed neighbors135PASS/1 previously recorded failure, no
  prior PASS lost (`floating-group-clear-nonvisible-border-v1-runtime-neighbors.json`).
  Independent49 collapsed-border/paint neighbors allPASS/no lost PASS
  (`nonvisible-border-paint-v1-neighbors.json`). Avoiding an unconditional
  extra whole-tree clone: layout now creates the extra used snapshot only
  when a known none/hidden edge has nonzero width (including masked global
  fallback), otherwise borrows original root as before. This adds a
  read-only predicate walk; latency impact not benchmarked. GREENv2 compile
  started on this final-source variant. Production968 remains unverified.

- Conditional-copy GREENv2 is terminal1PASS/1474filtered. Same final
  production sources:136 runtime neighbors135PASS/1 previously recorded
  failure/no prior PASS lost, and49 collapsed-border/paint neighbors49PASS
  (`floating-group-clear-nonvisible-border-v2-runtime-neighbors.json`,
  `nonvisible-border-paint-v2-neighbors.json`). Production build started
  under `nonvisible-border-box-v1`; source frozen for968..975 strict
  pixel verification and1040 directed qualification. No candidate commit
  or push before terminal qualification and clean-SHA replay.

- Nonvisible-border production build completes2m22s; strict968..975
  terminal6PASS/2FAIL. Both969/970 targets102504→0/max0, but972
  border-width-014 loses priorPASS (23040pixels),975 one-pixel failure
  unchanged. Receipt: `nonvisible-border-box-v1-pixel-comparison.json`.
  No larger qualification started, no candidate commit/push. Fixture972
  inherits both border-width and border-style across two generations;
  Document previously copies widths but has no border-style inheritance,
  so used-value masking exposes the missing line-style inheritance that
  formerly passed via numeric-only border rendering. Minimal DOM test
  `border_style_inherit_preserves_line_style_across_generations` added;
  real RED run started, terminal pending.

- Border-style inheritance unit is realRED: four None styles instead of
  Solid (`border-style-inherit-real-red.log`). Candidate copies each
  winning global/physical/shorthand inherited style from corresponding
  parent edge, restoring independent authored widths before used masking.
  Separate default-medium/explicit0/7px width test PASS. First GREEN attempt
  still fails inherited line styles: fixture's grandparent relative-width
  shorthand itself lost Solid in early parsing (`border-style-inherit-green-v1.log`).
  Finalization now validates relative/absolute shorthand width using shared
  classifier after font metrics and restores its line style; invalid tokens
  do not acquire a line style. GREENv2 running, terminalpending. No candidate
  commit/push or larger pixel qualification; production968 must be rerun.

- Border-style inheritance GREENv2 is terminal1PASS/455filtered on final
  source including relative shorthand line-style finalization. All34
  directed DOM neighbors PASS/no lost PASS (prior32 plus2 inheritance
  regressions); receipt `nonvisible-border-box-v2-unit-neighbors.json`.
  Final-source runtime GREENv3 rebuild started; prior runtime/paint receipts
  predate this DOM inheritance fix and are not substituted as final-source
  proof. Production968 v2 replay still pending; v1 lostPASS972 retained
  explicitly in failed receipt. No candidate commit/push.

- Final-source runtime GREENv3 is terminal1PASS/1474filtered. Runtime136
  directed neighbors135PASS/1 previously recorded failure/no lost PASS,
  and49 independent collapsed-border/paint neighbors49PASS/no lost PASS:
  `floating-group-clear-nonvisible-border-v3-runtime-neighbors.json`,
  `nonvisible-border-paint-v3-neighbors.json`. These bind the DOM line-style
  inheritance correction, unlike older v1/v2 runtime receipts. Production
  `nonvisible-border-box-v2` build started, source frozen;968 v2 strict
  pixel replay pending. No candidate commit/push.

- Nonvisible-border productionv2 build terminal2m07s. Strict968..975
  terminal7PASS/1FAIL: targets969/970102504→0/max0;972 inherited-style
  reftest recoveredPASS, complete object identical published baseline;
  other6 complete objects unchanged,975 one-pixel failure retained. No
  lost PASS. Receipt: `nonvisible-border-box-v2-pixel-comparison.json`.
  Fresh1040 directed qualificationv2 starts with968, compares prior912
  against current-color-v2 and848..975 against exact sealed discovery
  baselines. Sources frozen until terminal result; no candidate commit
  or push before qualification plus clean-SHA64 replay. No full6548 proof.

- Bounded next discovery976..983 stops after8 executions3PASS/5FAIL:
  border-width-shorthand-002/003/004 diffs1344/22512/22432; groove-default
  andridge-default mismatch tests incorrectly produce identical image to
  their notref (0pixels). These two require a DIFFERENT image, not a
  zero-diff target. Seal: `discovery-976-nonvisible-border-box-v2-sealed.json`.
  No earlier baseline in this coverage, so these are discovered failures,
  not proven candidate regressions. Current1040 qualification live and
  sources frozen; next width-shorthand/3D-border repairs remain separate.

- Next width-shorthand diagnosis is read-only while1040 qualification
  runs. Original-file Chromium141 oracle confirms physical978 widths
  [3,10,3,10],979[3,10,30,10],980[3,10,25,50]. Native979div height102
  despite96content, consistent with four3px borders instead of top3/bottom30.
  CSSStyleDeclaration border-width initially expands edges correctly, but
  subsequent global border-style setter uses declared_uniform_border_width
  and overwrites all four numeric edges with the first width. Existing
  declared_side_border_width already resolves per-edge shorthand widths.
  Minimal RED and correction deferred until current frozen publication
  boundary. Oracle: `border-width-shorthand-978-980-chromium-oracle.json`;
  dump: `border-width-979-layout.log`. No production source changed.

- Next3D border diagnosis is read-only. RenderSkia edge path paints each
  edge as a solid rectangle and ignores line-style shading. Original-file
  Chromium141 oracle captures groove topouter5 pixels RGB154 andinner5
  RGB238; ridge reverses them, while solid notref staysblack. Both retain
  computed border color black, so painting fallback/provenance matters.
  Oracle with four edge profiles and native browser PNGs:
  `groove-ridge-982-983-chromium-oracle.json`. Blink BoxBorderPainter uses
  outer/inner inset/outset halves and contrast-aware shade selection:
  https://chromium.googlesource.com/chromium/src/+/d8a622d252ff81a0e14c49c3afdba8cd92b123bb/third_party/blink/renderer/core/paint/box_border_painter.cc
  A later Chromium feature changes currentColor fallback/shading; it is
  not substituted for our observed141 oracle. Exact141.0.7390.37 source
  lookup failed (not treated as proof). No3D fix/source mutation yet;
  source stays frozen for current1040 qualification.

- Nonvisible-border qualificationv2 terminal1040 executions1026PASS/14FAIL.
  Only969/970 FAIL→PASS; other1038 complete ordered objects identical
  baselines, no lost PASS. These14 failures are selected-coverage residuals,
  not the remaining6548-suite count. Receipt:
  `nonvisible-border-box-v2-comparison.json`. Under user's explicit small-step
  commit/push authorization, preparing scoped6-file commit. Clean-SHA64
  replay968/840/600/824/624/488/608/5593 and actual push still pending.
  No full6548 zero-failure proof; next978..980 shorthand and982/9833D
  mismatch failures remain separately discovered, not included as repaired.

- Nonvisible-border fix published as `1f6b32841a5dc90e08539d8e906850fcf878fcb0`,
  normal push4f772eb→1f6b328 terminalsuccess, remote main verified exactSHA.
  Clean-SHA64 replay968/840/600/824/624/488/608/5593 all full objects
  identical qualification,5 source blobs and binary/suite hashes sealed:
  `nonvisible-border-box-1f6b328-clean-replay.json`. No full6548 closure.
  Next widths mini-step realRED: declaration3px10px thenborder-style solid
  yields[3,3,3,3] versus[3,10,3,10],1FAIL/456filtered; receipt
  `border-style-physical-widths-real-red.log`. Regression covers 2/3/4
  values, both declaration orders, none/hidden→solid, explicit0 and
  physical-edge overrides. Candidate resolves winning width independently
  per physical edge in global border-style setter, reusing existing
  declared_side_border_width; unused uniform-only helper removed. GREENv1
  started, no candidate commit/push or production978..980 proof yet.

- Width-preservation GREENv1 terminal1PASS/456filtered; all35 isolated
  directed DOM neighborsPASS/no prior PASS lost (prior34 plusnewwidth
  regression). Receipt: `border-style-physical-widths-v1-unit-neighbors.json`.
  Production build started with source frozen for976..983 strict pixels.
  No claim that3D groove/ridge shading is repaired. No candidate commit
  or push until directed qualification and clean-SHA replay.

- Width-preservation production build terminal2m32s. Strict976..983
  terminal6PASS/2FAIL: targets978/979/980 diffs1344/22512/22432→0,
  allmax0/zero tolerances; other5 full result objects unchanged, no lost
  PASS. Groove/ridge mismatch failures remain unchanged, not repaired.
  Receipt: `border-style-physical-widths-v1-pixel-comparison.json`.
  Fresh1048 directed qualificationv1 begins with976, compares prior1040
  exact nonvisible-border-box-v2 baselines plus976 sealed discovery.
  Production sources frozen; no candidate commit/push before terminal
  qualification and clean-SHA replay. No full6548 acceptance claim.

- Next3D repair now has strict full-browser baseline, not just WPT
  mismatch status. Current width-preservation candidate's actual groove
  andridge PNGs each differ from original Chromium141 screenshots by4400
  pixels/max238, including400 cornerpixels. Native PNGs are byteidentical
  to each other, demonstrating missing groove/ridge distinction. Image
  SHA256 and current focused7-file source/binary/suite seal retained in
  `3d-border-browser-baseline.json`. Detached-canvas read-only PNG decode,
  no image edits, fixture or tolerance changes. Future3D fix must verify
  browser image/corner parity as well as mismatch success; source remains
  frozen while1048 qualification runs.

- Width-preservation qualificationv1 terminal1048 executions1032PASS/16FAIL.
  Only978/979/980 FAIL→PASS; other1045 full ordered result objects identical
  baselines, no lost PASS. Receipt:
  `border-style-physical-widths-v1-comparison.json`. These16 failures are
  selected-coverage residuals, not remaining6548 statistics. Preparing
  scoped2-file commit under user's small-step commit/push authorization;
  clean-SHA72 replay976/968/840/600/824/624/488/608/5593 and actual push
  pending. Groove/ridge remain unrepaired; full6548 closure unproven.

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
