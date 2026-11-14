# How should an orchestrator brief a Gate agent?

[← Gate](../README.md)

The OpenCode agent sees only the `task` text and the repository. It never sees the orchestrator's conversation.
A one-line task leaves the agent to guess scope, edge cases and what "done" means, and it stops when the work *looks* done.
A brief replaces those guesses with checks the agent can run.

Gate sends the brief structure to every MCP client when it connects, in its server
[`INSTRUCTIONS`](<code:src/mcp/server.rs#const INSTRUCTIONS: &str>), so Claude Code and Codex both receive it.
This page explains each heading and gives examples.

## Headings

| Heading | What goes in it | Why |
| --- | --- | --- |
| Goal | The outcome and why it matters, in one to three sentences | An objective is the first thing a subagent needs [1] |
| Context | Files to read first, a file whose pattern to copy, decisions already made; for a bug, reproduction steps, expected versus actual result, and error output | The agent starts with an empty context; naming the file and the symptom saves a round of corrections [2] |
| Requirements | Numbered, testable statements with concrete values, in the form "WHEN trigger, component SHALL result" | EARS-style criteria are unambiguous and map directly to tests [3][4] |
| Edge cases | Boundaries, empty and invalid input, failures and timeouts, permissions, concurrency, Unicode, each with its expected result | Agents handle the happy path unprompted; they miss these [5] |
| Out of scope | Files and behaviour to leave alone; no new dependencies unless named | Boundaries stop a local fix from turning into a rewrite [4][1] |
| Tests | What to add at each level that applies, and the test framework and files | A test the agent can run turns "looks done" into "is done" [2] |
| Quality | Follow AGENTS.md and nearby code, mirror a named file, keep the diff small | Durable conventions belong in AGENTS.md; the brief points at the closest example [6][7] |
| Verify | The exact commands that must pass; for UI, each state at each width | "Done when" is one of the four parts of a good Codex task [6] |
| Report | Files changed, each command with its result, assumptions, open questions | Evidence is faster to check than a claim that tests pass [2] |

### Tests by level

| Level | Covers | Add when |
| --- | --- | --- |
| Unit | One function or module, with other parts stubbed | Always, one test per requirement and per edge case |
| Integration | Real boundaries: database, HTTP, filesystem, another module | The change crosses a boundary |
| Smoke or end-to-end | The main user path through the real entry point (CLI, server, page) | The change affects a user-facing path |
| Regression | The reported failure, written before the fix | Every bug fix: the test must fail first [8] |

Many fast unit tests and few end-to-end tests keep the suite fast and stable [9].

### UI work

UI requirements also list:

- Every state: loading, empty with a next action, error with retry, success, and disabled.
- The layout at mobile (375px), tablet (768px) and desktop (1280px) widths.
- Keyboard access and focus order, labels for controls without visible text, and contrast of at least 4.5:1 (WCAG AA). Colour must not be the only signal.
- The components and design tokens to reuse, and the exact copy text.

The Verify step runs the app and checks each state at each width, with a screenshot when the agent has a browser tool [10][2].

## Durable rules

Put rules that hold for every task in `AGENTS.md` at the repository root: build, test and lint commands, directory notes, and patterns new code must not copy.
OpenCode and Codex read it before they start. The brief then says only what is specific to this task [6][7].

## Examples

A backend feature:

```text
Goal: Rate-limit POST /api/login to stop password guessing.
Context: Routes live in src/routes/auth.ts. Copy the middleware pattern in src/middleware/requestId.ts. Redis is already a dependency (src/redis.ts).
Requirements:
1. WHEN one IP sends a 6th login request within 60s, the API SHALL return 429 with a Retry-After header.
2. WHEN the window expires, the API SHALL accept requests from that IP again.
Edge cases: Redis unavailable -> allow the request and log a warning. X-Forwarded-For is trusted only from the proxy IPs in config.trustedProxies.
Out of scope: Other routes. No new dependencies.
Tests: Unit tests for the counter with a fake clock. One integration test against the test Redis from docker-compose.test.yml.
Quality: Follow AGENTS.md. Mirror requestId.ts.
Verify: npm run lint && npm run typecheck && npm test
Report: Files changed, command output, any assumption about proxy headers. Commit as "Rate-limit login".
```

A bug fix:

```text
Goal: Exported CSV files lose non-ASCII names, so customers outside the US get broken reports.
Context: Reproduce with `cargo run -- export --account 42 > out.csv`: the name "Zoë" becomes "Zo?". Expected "Zoë". The writer is src/export/csv.rs.
Requirements: 1. WHEN a name contains non-ASCII characters, the export SHALL write them as UTF-8 unchanged.
Edge cases: Emoji, right-to-left text, and a name that is empty.
Out of scope: The column order and the date format.
Tests: First add a failing regression test in tests/export.rs with "Zoë" and an emoji, then fix it.
Verify: cargo fmt --check && cargo clippy -- -D warnings && cargo test
Report: The root cause, files changed, and command output.
```

A UI change:

```text
Goal: Show each project's open invoices on the dashboard so owners see what is unpaid.
Context: Page: web/src/pages/Dashboard.tsx. Copy the table and card layout from web/src/components/OrdersCard.tsx. Data comes from GET /api/invoices?status=open.
Requirements:
1. WHEN the page loads, the card SHALL show up to 5 open invoices, newest first, with number, customer, amount and due date.
2. WHEN an invoice is overdue, its row SHALL show an "Overdue" badge in the danger token color, with that text visible as well.
States: loading skeleton; empty "No open invoices" with a "Create invoice" button; error "Couldn't load invoices" with Retry.
Layout: one column at 375px, card spans two columns at 1280px; the table scrolls sideways at 375px.
Accessibility: rows reachable by Tab, the badge text is read by screen readers, contrast at least 4.5:1.
Out of scope: The invoices API and other dashboard cards.
Tests: Component tests for each state with mocked fetch. One Playwright test for the populated card.
Verify: pnpm lint && pnpm test && pnpm exec playwright test dashboard; open the page and check each state at 375, 768 and 1280px.
Report: Files changed, command output, and screenshots if a browser tool is available.
```

## Sources

1. Anthropic, [How we built our multi-agent research system](https://www.anthropic.com/engineering/multi-agent-research-system): each subagent needs an objective, an output format, guidance on tools and sources, and clear task boundaries.
2. Anthropic, [Best practices for Claude Code](https://code.claude.com/docs/en/best-practices): name the file, the symptom, and what "fixed" looks like; give the agent a check to run; ask for evidence.
3. Wikipedia, [Easy Approach to Requirements Syntax](https://en.wikipedia.org/wiki/Easy_Approach_to_Requirements_Syntax).
4. Factory, [Coding agent task specifications that work](https://factory.com/articles/coding-agent-task-specifications): each criterion needs a trigger, an expected result, and detail that separates success from a plausible approximation.
5. ProdPad, [Acceptance criteria examples](https://www.prodpad.com/blog/acceptance-criteria-examples/), and GitHub, [Best practices for Copilot coding agent tasks](https://docs.github.com/copilot/how-tos/agents/copilot-coding-agent/best-practices-for-using-copilot-to-work-on-tasks).
6. OpenAI, [Codex best practices](https://learn.chatgpt.com/guides/best-practices): goal, context, constraints, and "done when".
7. OpenAI, [Custom instructions with AGENTS.md](https://developers.openai.com/codex/guides/agents-md).
8. Digital Applied, [Give a coding agent a bug report it can actually verify](https://www.digitalapplied.com/blog/coding-agent-bug-report-evidence).
9. Momentic, [Software testing pyramid](https://momentic.ai/blog/software-testing-pyramid-guide).
10. Addy Osmani, [frontend-ui-engineering skill](https://github.com/addyosmani/agent-skills/blob/main/skills/frontend-ui-engineering/SKILL.md).
