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
