# Contributing to W3C OS

W3C OS uses an **AI-first development model**: AI agents write the code, humans provide direction and governance. See [AI_DEVELOPMENT.md](AI_DEVELOPMENT.md) for the full model.

## For Humans

You are the product managers, architects, and reviewers.

1. **File Issues** — Describe what you want. Use our [templates](.github/ISSUE_TEMPLATE/). The clearer the issue, the better the AI output.
2. **Review PRs** — AI-generated code needs human judgment, especially for security and architecture.
3. **Governance** — Propose architecture changes via GitHub Discussions. Approve breaking changes.
4. **Sponsor** — Fund the AI tokens that keep development moving. Click "Sponsor" on the repo page or see [AI_DEVELOPMENT.md](AI_DEVELOPMENT.md#funding-model).

You don't need to write code (but you can).

## For AI Agents

You are the developers. Pick up issues and ship code.

### Getting Started

1. Read [ARCHITECTURE.md](ARCHITECTURE.md) to understand the system
2. Look for Issues labeled `ai-ready`
3. Create a branch: `ai/<issue-number>-<short-description>`
4. Implement with tests
5. Open a PR — prefix title with `[AI]`

### Quality Bar

- `cargo check --workspace` must pass
- `cargo clippy --workspace -- -D warnings` must pass
- `cargo fmt --all -- --check` must pass
- `cargo test --workspace` must pass
- New functionality must have tests
- No `unsafe` without explicit justification
- Follow existing code patterns

### Rules

- All runtime/compiler code in **Rust**, examples in **TypeScript**
- All public functions must have doc comments
- Do not modify `ARCHITECTURE.md` or `AI_DEVELOPMENT.md` without human approval
- Prefix AI-authored PRs with `[AI]` in the title

## Token Costs & Sponsorship

AI agent tokens are funded by community sponsors:

- **GitHub Sponsors**: Click the "Sponsor" button on the repo
- **Open Collective**: https://opencollective.com/w3cos

100% of sponsorship goes to AI compute (tokens, CI). See [AI_DEVELOPMENT.md](AI_DEVELOPMENT.md#funding-model) for the breakdown.

Individuals using their own API keys to contribute are also welcome.

## Pull Request Process

1. Fork the repository (or create branch if you have access)
2. Create a feature branch (`ai/<issue>-<desc>` for AI, `feat/<desc>` for humans)
3. Make changes with tests
4. Ensure all CI checks pass
5. Submit PR using the [template](.github/pull_request_template.md)
6. Management AI runs automated review
7. Human maintainer gives final approval
8. Management AI merges

## Code of Conduct

All participants — human and AI — must follow our [Code of Conduct](CODE_OF_CONDUCT.md).
