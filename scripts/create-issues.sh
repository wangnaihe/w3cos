#!/bin/bash
# Create Good First Issues on GitHub.
# Prerequisites: gh auth login
# Usage: ./scripts/create-issues.sh

set -e

REPO="wangnaihe/w3cos"

echo "Creating Good First Issues for $REPO..."
echo ""

gh issue create --repo "$REPO" \
  --title "[Good First Issue] Implement CSS transition animation playback" \
  --label "good first issue,ai-ready,enhancement" \
  --body "$(cat <<'EOF'
## Module
`w3cos-runtime/src/window.rs`

## Description
The transition data structure and easing functions already exist in `w3cos-std/src/style.rs` (`Transition`, `Easing`). Implement the frame loop in the window event loop that interpolates style values over time when they change.

## Acceptance Criteria
- When a style property changes and has a `transition` defined, animate the change over the specified duration
- Support easing functions: `Linear`, `EaseIn`, `EaseOut`, `EaseInOut`
- Smooth 60fps animation using the existing winit event loop

## Difficulty
Medium

## Relevant Files
- `crates/w3cos-std/src/style.rs` — Transition and Easing types
- `crates/w3cos-runtime/src/window.rs` — Event loop where animation should happen
EOF
)"

echo "Created: CSS transition animation"

gh issue create --repo "$REPO" \
  --title "[Good First Issue] Implement Image component (PNG/JPEG)" \
  --label "good first issue,ai-ready,enhancement" \
  --body "$(cat <<'EOF'
## Module
`w3cos-std` + `w3cos-runtime/src/render.rs`

## Description
Add an `Image { src }` component kind. Decode PNG/JPEG images and render them using tiny-skia.

## Acceptance Criteria
- New `Component::image(src, style)` constructor
- Support PNG and JPEG formats
- Render the image within the layout bounds respecting width/height styles
- Add an example app that displays an image

## Difficulty
Low-Medium

## Relevant Files
- `crates/w3cos-std/src/component.rs` — Add Image variant
- `crates/w3cos-runtime/src/render.rs` — Render the image pixels
- `crates/w3cos-runtime/src/layout.rs` — Layout for image nodes
EOF
)"

echo "Created: Image component"

gh issue create --repo "$REPO" \
  --title "[Good First Issue] Add --strip and --lto flags for smaller binaries" \
  --label "good first issue,enhancement" \
  --body "$(cat <<'EOF'
## Module
`w3cos-cli/src/main.rs`

## Description
Pass strip/LTO/codegen-units options to `cargo build` for smaller release binaries. Currently output is ~2.4MB, this could bring it down to ~1MB.

## Acceptance Criteria
- Add `--strip` CLI flag (or enable by default in `--release`)
- Generate Cargo.toml profile with `strip = true`, `lto = true`, `codegen-units = 1`
- Document the size reduction in output

## Difficulty
Low

## Relevant Files
- `crates/w3cos-cli/src/main.rs` — CLI argument handling
- `crates/w3cos-compiler/src/codegen.rs` — Cargo.toml generation (`generate_cargo_toml`)
EOF
)"

echo "Created: Strip/LTO flags"

gh issue create --repo "$REPO" \
  --title "[Good First Issue] Add more example applications" \
  --label "good first issue,documentation" \
  --body "$(cat <<'EOF'
## Module
`examples/`

## Description
Create additional example applications to showcase W3C OS capabilities and help new contributors understand the component system.

## Suggested Examples
- Calculator
- Login form
- Settings panel
- Weather app UI
- Chat UI
- Music player
- File browser

## Acceptance Criteria
- Each example in its own `examples/<name>/app.ts` file
- Each example builds successfully with `w3cos build`
- Uses a variety of components (Column, Row, Text, Button) and styles

## Difficulty
Low — great for first-time contributors!
EOF
)"

echo "Created: More examples"

gh issue create --repo "$REPO" \
  --title "[Good First Issue] Add w3cos init scaffolding command" \
  --label "good first issue,enhancement" \
  --body "$(cat <<'EOF'
## Module
`w3cos-cli/src/main.rs`

## Description
Add a `w3cos init <project-name>` command that generates a new W3C OS project with a template `app.ts` and README.

## Acceptance Criteria
- `w3cos init myapp` creates a directory `myapp/` with:
  - `app.ts` — template application
  - `README.md` — basic instructions
- Template app should be a simple "Hello World" style app
- Error if directory already exists

## Difficulty
Low

## Relevant Files
- `crates/w3cos-cli/src/main.rs` — Add `init` subcommand
EOF
)"

echo "Created: w3cos init"

gh issue create --repo "$REPO" \
  --title "[Good First Issue] Add annotated screenshot rendering for AI agents" \
  --label "good first issue,ai-ready,enhancement" \
  --body "$(cat <<'EOF'
## Module
`w3cos-ai-bridge/src/screenshot.rs`

## Description
Draw numbered circles on interactive elements in the screenshot PNG. Map numbers to DOM node IDs. This enables AI agents (like Claude Computer Use) to interact with W3C OS apps via screenshots.

## Acceptance Criteria
- Generate a screenshot with numbered labels on each interactive element
- Return a mapping: `{ 1: "node-id-123", 2: "node-id-456", ... }`
- Numbers should be readable (contrasting background circle)
- Works with the existing `capture_screenshot` function

## Difficulty
Medium

## Relevant Files
- `crates/w3cos-ai-bridge/src/screenshot.rs` — Current screenshot implementation
- `crates/w3cos-runtime/src/render.rs` — Rendering primitives
EOF
)"

echo "Created: Annotated screenshot"

echo ""
echo "All Good First Issues created successfully!"
echo "View them at: https://github.com/$REPO/issues"
