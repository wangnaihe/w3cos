# AI-Driven Development Model

W3C OS uses an **AI-first development model**: AI agents are the primary developers; humans provide requirements, review, and governance.

## How It Works

```
Human                          AI (Management)                AI (Contributor)
  │                                  │                              │
  ├─ Files Issue ──────────────────► │                              │
  │                                  ├─ Triages, labels, assigns ──►│
  │                                  │                              ├─ Reads issue + codebase
  │                                  │                              ├─ Implements solution
  │                                  │                              ├─ Writes tests
  │                                  │                              ├─ Opens PR ──────────►│
  │                                  ├─ Reviews PR (CI + AI) ◄──────┤                      │
  │  ◄─ Notified for final review ──┤                              │                      │
  ├─ Approves / requests changes ──►│                              │                      │
  │                                  ├─ Merges PR ─────────────────────────────────────────┘
```

## Two Kinds of AI

### Management AI

The **Management AI** is the project maintainer. It runs automatically via GitHub Actions and OpenClaw Lobster workflows.

Responsibilities:
- **Triage**: When a new Issue is filed, classify it (bug/feature/task), estimate difficulty, assign labels
- **Assignment**: Route `ai-ready` issues to the next available Contributor AI
- **Code Review**: Run CI checks, verify test coverage, check for style/architecture violations
- **Merge Control**: Auto-merge PRs that pass all checks and have human approval
- **Release Management**: Tag releases, generate changelogs

Trigger: Runs on `issues.opened`, `pull_request.opened`, `pull_request_review.submitted`

### Contributor AI

**Contributor AIs** are the developers. They pick up issues and write code.

Workflow:
1. Receive an assigned issue (via GitHub Actions dispatch or OpenClaw)
2. Read the issue description, acceptance criteria, and relevant codebase
3. Create a branch, implement the solution, write tests
4. Open a PR with a clear description
5. Respond to review feedback and iterate

Tools: Cursor Agent, OpenClaw + Lobster, GitHub Copilot Workspace, or any AI coding tool

## Funding Model

AI agents consume API tokens (LLM inference). The community funds these tokens through sponsorship.

### Where the Money Goes

```
Sponsors (GitHub Sponsors / Open Collective)
    │
    ▼
Token Pool
    ├── 60%  Contributor AI tokens (coding, PR generation)
    ├── 25%  Management AI tokens (triage, review, merge)
    └── 15%  Infrastructure (CI runners, hosting)
```

### How to Sponsor

- **GitHub Sponsors**: Click the "Sponsor" button on the repo page
- **Open Collective**: https://opencollective.com/w3cos

Every dollar goes to AI compute. No human salaries. Fully transparent spending.

### Sponsorship Tiers

| Tier | Amount | What It Funds |
|------|--------|---------------|
| Byte | $5/mo | ~1 AI-implemented issue per month |
| Kilobyte | $25/mo | ~5 AI-implemented issues per month |
| Megabyte | $100/mo | ~20 AI-implemented issues per month |
| Gigabyte | $500/mo | Sustained AI development capacity |

## Issue Lifecycle

```
1. [opened]     Human or AI files an Issue
2. [triaged]    Management AI labels: bug/feature, priority, difficulty, module
3. [ai-ready]   Management AI confirms the issue is well-defined for AI
4. [assigned]   Contributor AI is dispatched
5. [in-progress] AI creates branch and starts coding
6. [pr-opened]  AI opens PR with implementation
7. [review]     Management AI + CI check the PR
8. [approved]   Human maintainer approves
9. [merged]     Management AI merges
```

## Labels

| Label | Meaning |
|-------|---------|
| `ai-ready` | Well-defined, AI can pick up immediately |
| `ai-assigned` | An AI agent is working on this |
| `ai-pr` | PR was authored by an AI agent |
| `needs-human` | Requires human decision (architecture, security, policy) |
| `good first issue` | Easy task, suitable for new contributor AIs or humans |
| `sponsored` | Funded by a specific sponsor |

## For Humans

Your role is critical:

- **File Issues**: Describe what you want. The clearer, the better the AI output.
- **Review PRs**: AI-generated code needs human judgment for architecture and security.
- **Governance**: Decide project direction, approve breaking changes, set priorities.
- **Sponsor**: Fund the AI tokens that keep development moving.

You don't need to write code (but you can).

## For AI Agents

To contribute as an AI agent:

1. Look for issues labeled `ai-ready`
2. Read [ARCHITECTURE.md](ARCHITECTURE.md) to understand the codebase
3. Read the issue's acceptance criteria carefully
4. Create a branch: `ai/<issue-number>-<short-description>`
5. Implement the solution with tests
6. Open a PR using the [PR template](.github/pull_request_template.md)
7. Prefix your PR title with `[AI]`
8. Wait for review and iterate

### Quality Bar

AI-submitted PRs must:
- Pass all CI checks (`cargo check`, `clippy`, `fmt`, `test`)
- Include tests for new functionality
- Not introduce new clippy warnings
- Follow existing code patterns and architecture
- Have a clear PR description explaining the changes

## OpenClaw Integration

W3C OS uses [OpenClaw](https://github.com/openclaw) + [Lobster](https://github.com/clawdbot/lobster) for AI workflow orchestration.

OpenClaw provides:
- **MCP Gateway**: Connects AI agents to GitHub, CI, and other tools
- **Lobster Pipelines**: Deterministic multi-step workflows with approval gates
- **Token Management**: Track and allocate API token spending

See [.openclaw/](.openclaw/) for workflow configurations.

## Transparency

All AI activity is public:
- Every AI-authored PR is labeled `ai-pr`
- Token spending reports are published monthly (when sponsorship is active)
- Management AI decisions are logged in issue comments
- No hidden AI activity — everything goes through GitHub
