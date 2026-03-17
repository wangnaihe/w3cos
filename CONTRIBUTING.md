# Contributing to W3C OS

This project welcomes contributions from both humans and AI agents.

## For Humans

1. **File Issues** with clear requirements using our templates
2. **Review PRs** — especially security-sensitive changes
3. **Architecture decisions** — propose via GitHub Discussions
4. **Label Issues** as `ai-ready` when they are well-defined enough for AI to implement

## For AI Agents

### Getting Started

1. Read `ARCHITECTURE.md` to understand the system
2. Read the relevant `AGENTS.md` in the crate you're working on
3. Pick an Issue labeled `ai-ready`
4. Submit a PR with tests

### Rules

- All code must be in **Rust** (for runtime/compiler) or **TypeScript** (for examples/spec)
- No `unsafe` without explicit justification
- All public functions must have doc comments
- Tests are required for new functionality
- Do not modify `ARCHITECTURE.md` without human approval

### Code Style

- `cargo fmt` and `cargo clippy` must pass
- Follow existing patterns in the codebase
- Prefer simplicity over cleverness

## Token / Model Costs

This project does **not** provide AI compute or API keys.
- If you use an AI agent to contribute, you supply your own API key
- Local models (Llama, Qwen, etc.) work fine for most tasks
- The project provides the rules and interfaces; you provide the AI

## Pull Request Process

1. Fork the repository
2. Create a feature branch
3. Make your changes with tests
4. Ensure `cargo test` and `cargo clippy` pass
5. Submit PR with a clear description of what and why
6. Wait for CI + human review
