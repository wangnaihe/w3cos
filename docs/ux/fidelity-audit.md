# Shell fidelity audit

Side-by-side captures compare the HTML mockups in `docs/ux/screenshots/` with the CPU renderer under Xvfb (`--no-default-features --features cpu-render`, `W3COS_LOCALE=zh-CN`). Desktop shots use `W3COS_VIEW=1200x800`. The phone shot uses `390x760`. Mockups are 1440×900 (phone 390×844) and are scaled to the same height in the pairs below. `W3COS_SCREEN=confirm|permission|receipts|lock` opens those scenes. Receipt rows in the receipts shot are seeded by that hook so the layout is visible; a real session starts empty.

Each pair is mockup, then the shell.

| Screen | Pair |
| --- | --- |
| Desktop home | ![desktop home](screenshots/fidelity/desktop-home.png) |
| Confirm | ![confirm](screenshots/fidelity/desktop-confirm.png) |
| Permission | ![permission](screenshots/fidelity/desktop-permission.png) |
| Receipts | ![receipts](screenshots/fidelity/desktop-receipts.png) |
| Lock | ![lock](screenshots/fidelity/desktop-lock.png) |
| Phone home | ![phone home](screenshots/fidelity/phone-home.png) |

## Findings

| Difference | Cause | Evidence | Fix |
| --- | --- | --- | --- |
| Wallpaper was a flat `#24506e` fill. The mock is two radials plus a linear gradient. | Shell, then a small parser gap. | `WALLPAPER` is the mock stack. `explicit_radial_size_and_negative_position_are_kept` and `layered_wallpaper_gradient_is_not_a_flat_fill`. Home corner is about `(33, 59, 74)`. | Shell sets `WALLPAPER`. The parser keeps explicit `(rx, ry)` and unclamped percentages. CPU, GPU, and Skia all use both axes. GPU/Skia stretch a unit-circle gradient by `(rx, ry)` (`ellipse_local_matrix_maps_the_rim_to_the_unit_circle`). |
| Squircle inset highlight plus the drop shadow. | Runtime. A style held one `BoxShadow`. | `box_shadow_list_keeps_inset_and_outer_layers`. `inset_shadow_stays_inside_the_box_and_outer_shadow_does_not` and `blurred_inset_shadow_stays_inside_the_box`. | `Style.box_shadow` is the first layer and `box_shadows` holds the rest. Outer layers paint before the background. Inset layers paint after it and are clipped to the rounded box on CPU, GPU, and Skia. Squircles use `inset 0 1px 0 rgba(255,255,255,0.16)` plus `0 10px 20px`. Windows, sheets, the lock card, and the receipt drawer add the faint inset on top of the large drop. |
| CPU shadow blur was a stack of expanding rects. | Runtime. | `draw_box_shadow` used to step `blur_radius / 2`. | Blur above ~0 uses a separable gaussian. σ is `blur_radius / 2`, matching the existing GPU shadow, and is capped at 24. Blur 0 stays the hard fill. `gaussian_outer_shadow_is_soft_and_backdrop_mixes_the_seam`. |
| Dock and intent had no backdrop blur. | Runtime. | No `backdrop-filter` field. `backdrop_filter_blur_is_the_standard_deviation`. | `Style.backdrop_blur` is the CSS `blur()` standard deviation. CPU copies the border box plus the kernel pad, blurs premultiplied pixels, and writes back only inside the rounded rect. GPU crops the scene so far and runs the existing separable filter pass. Skia uses `SaveLayerRec::backdrop`. The dock uses `blur(18px)` and the intent bar `blur(16px)`, with the mock rgba kept dark enough to read. |
| Text weight, CJK advance, and antialiasing differ from the browser mock. | Runtime. Only Inter Regular and a CJK subset are bundled. | `text_shadow_paints_beside_the_glyph`. `snapped_glyph_advance` lifts a CJK advance below 0.85 em up to one em. | Icon labels and the intent hint use `text-shadow: 0 1px 2px rgba(0,0,0,0.45)`. CPU blurs that mask. Skia blurs a text layer. GPU draws a few offset taps. Weight ≥ 600 overstrikes by about 0.6px because there is no bold face. Glyph coverage uses a mild gamma (`pow 0.85`). This is closer to the mock, not a browser text stack. |
| Receipt timestamps and utterances. | Shell data. | `receipt_log_tail` still has no clock or utterance. | Left open. The drawer layout is unchanged. |

## What was fixed in this pass

Backdrop blur, a comma-separated box-shadow list (inset and outer), gaussian CPU shadows, elliptical radials on GPU and Skia, clipped inset blur on GPU and Skia, and a practical text-shadow plus semibold/CJK paint tweak. The shell dock and intent bar use real blur. Squircles and the large cards use an inset highlight plus the drop shadow.

## Performance

Blur runs only when a shadow, backdrop, or text-shadow asks for it. σ is capped at 24 (kernel radius ≤ 72). Shadows blur one alpha plane and colorize after. Backdrop blurs only the border box plus that pad, not the whole framebuffer. σ ≥ 16 blurs a 4× smaller plane and σ ≥ 8 a 2× smaller plane, then the result is bilinearly scaled back; smaller blurs stay at full resolution. `dock_sized_backdrop_and_window_shadow_stay_interactive` bounds both under 750ms. In a debug build the 1200×120 σ18 backdrop took about 45ms and the 640×400 `blur(60px)` window shadow about 53ms. The same full-resolution passes were about 1.4s and 0.9s. Hot paths with blur 0 or no effect property are unchanged.

Animation frames do not recompute that gaussian when the pixels underneath are unchanged. Backdrop strips are cached per 64px tile, and a damage rect reblurs only the dirty tiles plus the kernel margin (`unchanged_backdrop_is_a_copy_and_a_dirty_tile_matches_a_full_blur`). Shadow sprites are cached by shape and subpixel phase, so a translated dock reuses the mask. The dock and intent bar also keep a chrome snapshot of the framebuffer after blur, shadow, and background. The next full frame with the same underlay copies that snapshot. `dock_and_intent_bar_reuse_cached_chrome_under_4ms_when_release` paints a 1200×800 frame (wallpaper, dock `blur(18px)` plus `shadow-sm`, intent bar `blur(16px)` plus `shadow-sm`). Release (`--release`, cpu-render): cold frame 24.33ms, warm frame 0.47ms. Debug warm was 5.56ms. A later damage repaint of a corner that does not touch the bars leaves the dock pixel unchanged.

## Left open

Receipt timestamps and utterances. Text is still fontdue (CPU) or the platform face (Skia/GPU) rather than Inter plus WenQuanYi in a browser. GPU text-shadow is a short tap stack; CPU and Skia use a real gaussian.
