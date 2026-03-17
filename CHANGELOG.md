# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- README badges (CI, License, Rust version)
- CODE_OF_CONDUCT.md (Contributor Covenant v2.1)
- SECURITY.md (vulnerability reporting policy)
- PR template and Issue templates (Bug Report, Feature Request)
- GitHub Actions workflow for ISO builds (manual + tag trigger)
- ISO build instructions in README

### Fixed
- README screenshot now renders as inline image instead of text link

## [0.1.0] - 2025-03-17

### Added
- **w3cos-std**: Component, Style, Color, Dimension (rem/em/vw/vh), BoxShadow, Transform2D, Transition, Easing
- **w3cos-dom**: W3C DOM API — Document, Element, Node arena, Events (click/mouse/key/focus/scroll), querySelector, classList, CSSStyleDeclaration
- **w3cos-a11y**: Accessibility tree generation from DOM (ARIA roles, AI-friendly flatten)
- **w3cos-ai-bridge**: AI agent interface — DOM access, a11y API, annotated screenshot, permission system
- **w3cos-compiler**: TypeScript/JSON parser with Rust code generation (Column, Row, Text, Button, Box)
- **w3cos-runtime**: Layout engine (Taffy 0.9 — Flexbox, Grid, Block, position), 2D rendering (tiny-skia), native windowing (winit), mouse event handling
- **w3cos-cli**: `w3cos build` and `w3cos run` commands
- 4 example applications: hello, counter, dashboard, showcase
- Buildroot configuration for bootable x86_64 ISO
- QEMU run script
- Dockerfile (multi-stage build)
- DevContainer configuration (Codespaces support)
- ARCHITECTURE.md, ROADMAP.md, CONTRIBUTING.md, ISSUES.md
- CI workflow (cargo check, clippy, test, fmt)
