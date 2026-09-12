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
