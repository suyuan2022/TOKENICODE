# Implementation Plan: Fix CLI SDK Protocol Bugs + E2E Harness

**Task**: `04-08-fix-cli-sdk-protocol-e2e-harness`
**Related PRD**: `./prd.md`
**Status**: Draft (awaiting research agent completion for root cause section)
**Author**: main agent (suyuan session)

---

## 0. TL;DR (for user waking up)

**What this plan does**: Permanently fix the recurring Claude CLI SDK control
protocol bug cluster (#57 #49 #44 #39 #30 #27) and stand up a test harness that
prevents regressions of this bug class.

**What it does not do**: Touch unrelated modules, push anything upstream, or
delete the owner's existing code without a test proving the replacement is
correct.

**Strategy in one sentence**: bootstrap tests first (`cargo test` + `vitest` +
fake Claude CLI fixture), write failing tests for each bug, fix the root
cause(s), ship as a single branch `fix/cli-sdk-protocol-e2e-harness` for
review.

---

## 1. Forensic Context (what's been tried before)

TOKENICODE's CHANGELOG shows this class of bugs has been patched **at least
five times** without sticking:

| Version | Commit | Attempted fix | Outcome |
|---|---|---|---|
| 0.8.6 | — | `stdinId` ownership verification + per-stdinId listener isolation + orphan process cleanup (TK-329) | Recurred |
| 0.8.8 | — | `desk_*` temp ID pollution fix + `stdinToTab` map leak fix + startup cleanup | Recurred |
| 0.9.2 | — | `rAF flush` fallback to `selectedSessionId` + auto-repair `stdinToTab` mapping (#57) | Recurred (user confirmed on 0.10) |
| 0.9.6 | `191f7ff` | Rust stdout IO error handling + consecutive emit failure counter + frontend flush-before-background-exit (#64) | Recurred (user confirmed on 0.10) |
| (various) | — | "Bug C fix (#27)" in useStreamProcessor.ts L1842-1846: clear stuck `pendingCommandMsgId` on exit | Still stuck on focus-loss mid-execution |

**Pattern**: every prior fix patches a **symptom site** (one listener, one
buffer, one event handler) instead of establishing a **routing invariant** that
the whole system must obey. The next refactor always breaks the patch.

**Implication**: the fix must either:
- (a) **collapse all routing logic into one primitive** with exhaustive tests, OR
- (b) **add runtime assertions** that fail loudly when the invariant is violated,
      so regressions surface immediately

The research agent will decide between (a) and (b) based on the code structure.

---

## 2. Root Cause Findings (from Research Agent, 2026-04-08)

Research Agent partially confirmed the main-agent hypothesis: routing IS the
shared fault zone for #57 and #27, but each bug collapses to a concrete
code-local cause. **Six bugs → four root causes → three fixes** (+ one
deferred).

### 2.1 Bug #57 — Stream text stops rendering (two sub-causes)

**#57-A — Missing `content_block_delta` case in background handler**

`handleBackgroundStreamMessage` (`src/hooks/useStreamProcessor.ts:213-691`)
only handles stream deltas wrapped in `stream_event`. The foreground handler
has a top-level `case 'content_block_delta'` fallback at
`useStreamProcessor.ts:1945-1954`. When the CLI emits a bare
`content_block_delta` (without `stream_event` wrapper) for a tab that is
currently in background (`ownerTabId !== selectedSessionId`), the event routes
to `handleBackgroundStreamMessage` at line 731, the switch has no matching
case, and the delta is **silently dropped**. Explains the user's symptom
"reply exists in backend, restart shows full reply" — the reply is later read
from disk JSONL on session reload, bypassing the lost deltas.

**#57-B — rAF orphan buffer wipe**

`_scheduleStreamFlush` (`useStreamProcessor.ts:58-88`) resolves
`tabId = getTabForStdin(stdinId) || selectedSessionId` inside its rAF callback.
If the `unregisterStdinTab` call from `process_exit` (line 1927) fires BEFORE
the in-flight rAF, `getTabForStdin` returns undefined. If `selectedSessionId`
is also null (session was removed or user is in an empty state), lines 72-76
execute and **wipe `buf.text`/`buf.thinking` with no flush, no warning, no
persistence**. The 0.9.2 "fallback to selectedSessionId" fix (CHANGELOG.md:112)
closed the common path but not this residual window.

### 2.2 Bug #27 — `/compact`, `/context` stuck in spinner after session switch

`handleBackgroundStreamMessage` `case 'result'` (`useStreamProcessor.ts:539-627`)
handles status, meta, token usage, pending drain, and title generation but
**never touches `pendingCommandMsgId`**. Compare the foreground `result`
handler at `useStreamProcessor.ts:1594-1606` which explicitly clears it and
marks `commandCompleted: true`. When the user switches tabs mid-`/compact`,
the subsequent `result` event routes to the background handler and the
spinner message stays `commandCompleted: false` forever. The "Bug C fix (#27)"
at `useStreamProcessor.ts:1842-1847` only patches the `process_exit` path, not
the `result` path during background.

### 2.3 Bug #39 — Sub-agent running → input locked

Two layers contribute:

**Rust side** (`src-tauri/src/lib.rs:1557-1591`, the `can_use_tool` branch):
emits `tokenicode_permission_request` with `request_id, tool_name, input,
description, tool_use_id` but **strips `parent_tool_use_id` and `agent_id`**.
The `protocol.rs:49` `CanUseTool::agent_id` field is defined but `lib.rs`
doesn't read it from the raw request JSON.

**Frontend side** (`src/hooks/useStreamProcessor.ts:743-872`, the
`tokenicode_permission_request` handler): unconditionally calls
`setActivityStatus(tabId, { phase: 'awaiting' })` at line 871 for every
permission request, regardless of which agent owns it. `InputBar.tsx:358`
derives `isAwaiting = isRunning && activityPhase === 'awaiting'`,
`InputBar.tsx:1462` uses this to disable the Send button. Even a parallel
Task sub-agent's tool permission freezes the outer input because the phase is
set on the main tab id.

### 2.4 Bugs #49, #44, #30 — deferred

Detail exists only in the sibling Her-Desktop repo which we don't have source
for. Research agent's assessment: **HIGH probability** these are variations
already covered by F1+F2+F3 based on symptom keywords in CHANGELOG.md:236-238
("desk_* 临时 ID 污染", "stdinToTab 映射泄漏") and CHANGELOG.md:254-256
("多会话串流污染 TK-329"). Treatment: ship F1+F2+F3, ask user to retest all
three after the fix lands. If any still reproduce, request upstream commit
hashes or Her-Desktop issue bodies.

### 2.5 Unified architectural invariant

All fixes converge on one invariant worth documenting in
`.trellis/spec/backend/event-emission.md`:

> **The background and foreground stream handlers must implement the same set
> of cases, distinguished only by what "render" means (direct store write vs.
> cache write). Any `case` added to one handler MUST be added to the other,
> enforced by a lint rule, a shared dispatch function, or a test that asserts
> both handlers respond to the same input types. Silent drops are forbidden —
> unknown message types must log a warning, not be ignored.**

Applied to the concrete bugs: #57-A and #27 are both instances of
"foreground handler has a case the background handler lacks". Codifying the
invariant prevents the next drift.

---

## 3. Phase Breakdown

### Phase 0: Test Infrastructure Bootstrap

**Goal**: Make `pnpm test` and `cargo test` both work and both run something.

**Files to create / modify**:

| File | Change |
|---|---|
| `package.json` | Add `"test": "vitest run"`, `"test:watch": "vitest"`, `"test:coverage": "vitest run --coverage"` to `scripts` |
| `vitest.config.ts` (new) | Minimal vitest config: jsdom environment, globals enabled, setup file path |
| `src/test/setup.ts` (new) | Mock `@tauri-apps/api/core` `invoke` and `@tauri-apps/api/event` `listen` / `emit` |
| `src/test/mocks/tauri-bridge.ts` (new) | Typed mock for `src/lib/tauri-bridge.ts` — lets tests intercept calls |
| `src-tauri/Cargo.toml` | Add dev-deps: `tokio-test`, `assert_cmd`, `predicates`, `serde_json` (if not already in main deps) |
| `src-tauri/tests/` (new dir) | Standard Cargo integration test dir |
| `src-tauri/tests/common/mod.rs` (new) | Shared test helpers: spawn fake CLI, setup temp dir, assert event received |
| `src-tauri/tests/fixtures/` (new dir) | Where fake CLI fixture lives |
| `src-tauri/tests/fixtures/fake_claude_cli/` (new) | Cargo workspace member: a Rust binary that replays NDJSON fixtures |
| `src-tauri/tests/fixtures/fake_claude_cli/Cargo.toml` (new) | Cargo package manifest |
| `src-tauri/tests/fixtures/fake_claude_cli/src/main.rs` (new) | Main binary: parse `--scenario <name>`, read stdin for control protocol, emit stdout according to scripted scenario |
| `src-tauri/tests/fixtures/scenarios/*.ndjson` (new) | One file per bug scenario |

**Smoke check**: After Phase 0, `pnpm test` should exit 0 with "no tests found
is not a failure" message, `cargo test` should build the fake CLI and run an
empty test suite.

### Phase 1: Reproduce Bugs (Failing Tests)

**Goal**: Write tests that fail on the current v0.10 code and map 1:1 to each
reported bug.

**Test files to create** (names are targets; research agent refines):

| Bug # | Test file | Assertion |
|---|---|---|
| #57 | `src/hooks/useStreamProcessor.test.ts` + `src-tauri/tests/stream_reliability.rs` | Send N text deltas through fake CLI while switching tabs mid-stream; assert all text reaches the originating tab's cache |
| #49 | `src/stores/sessionStore.test.ts` + integration | Spawn two sessions, send to A, verify B's cache is untouched |
| #44 | `src-tauri/tests/stdin_routing.rs` | stdinId→tabId map survives rapid registerStdinTab / promoteDraft cycles |
| #39 | `src/stores/chatStore.test.ts` + integration | Sub-agent spawn does not lock parent input; queued messages flush |
| #30 | `src-tauri/tests/multi_session_isolation.rs` | N concurrent sessions each get only their own events |
| #27 | `src/hooks/useStreamProcessor.test.ts` | `/compact` during tab switch: `pendingCommandMsgId` cleared on result arriving after refocus |

**Exit criteria for Phase 1**: Every test in the table fails with a clear
assertion message when run against current code. Tests must be deterministic
(no `setTimeout(..., 100)` flakiness — use fake timers + controlled fake CLI).

### Phase 2: Fix Root Cause

**Goal**: Make all Phase 1 tests pass with the smallest possible diff.

Three concrete fixes (F1+F2+F3), described in detail below. F4 is deferred
pending user clarification on #49/#44/#30.

#### Fix F1 — Background handler: add `content_block_delta` + result-clears-pendingCmd

**Layer**: Frontend only.
**Files**: `src/hooks/useStreamProcessor.ts`.
**Lines**: `213-691` (the `handleBackgroundStreamMessage` switch).

**Changes**:
- Add a top-level `case 'content_block_delta'` and `case 'thinking_delta'`
  mirroring the foreground bare-delta handling at lines 1945-1954. Write
  directly into the target tab's `partialText` / `partialThinking` via
  `updatePartialMessage` / `updatePartialThinking` (not via the rAF buffer —
  background tab is not visible, rAF optimization is pointless).
- Inside the background `case 'result'`, after the existing status/meta/token
  handling, read `tab.sessionMeta.pendingCommandMsgId`; if set, mark that
  message `commandCompleted: true` with `commandData.output = msg.result || ''`
  and clear `pendingCommandMsgId`. Mirror the foreground logic at lines
  1088-1098 and 1594-1606 exactly.

**Fixes**: Bug #57-A (silent delta drop for background tabs), Bug #27
(`/compact` spinner stuck after tab switch).

**Risk**: LOW. Extract a shared helper `applyResultCompletion(tabId, msg)` to
prevent foreground/background drift in the future. Not required for the fix
itself, but listed as a follow-up in the risk register.

#### Fix F2 — Orphan queue for rAF stream flush

**Layer**: Frontend only.
**File**: `src/hooks/useStreamProcessor.ts`.
**Lines**: `58-88` (`_scheduleStreamFlush`) and `72-76` (the silent wipe).

**Changes**:
- Replace the silent wipe at lines 72-76 with an orphan queue entry. Keep a
  module-level `Map<stdinId, { text, thinking, expiresAt }>` for orphaned
  buffers.
- On `registerStdinTab(stdinId, tabId)` in `sessionStore`, emit a custom event
  or call a setter in the stream processor that drains any orphan entry for
  that stdinId into the target tab.
- Add a 5-second TTL per orphan entry. If still unclaimed after TTL, log
  `console.error` with stdinId + char-count (not silent). Cap total orphan
  queue size at 10 MB to prevent runaway growth.
- Add a `console.warn` when moving buffer content to the orphan queue, so
  regressions show up in DevTools during dev.

**Fixes**: Bug #57-B (residual silent-wipe window).

**Risk**: LOW. Orphan queue is bounded both per-stdinId (1 MB cap) and total
(10 MB cap) with TTL. Memory concerns are negligible — kilobytes typical.

#### Fix F3 — Sub-agent permission gate (frontend-only variant)

**Layer**: Frontend only (Research Agent's recommended variant).
**Files**: `src/hooks/useStreamProcessor.ts`, `src/components/chat/InputBar.tsx`.
**Lines**: `useStreamProcessor.ts:743-872`, `InputBar.tsx:358`, `InputBar.tsx:1462`.

**Changes**:
- Add a `subAgentDepth: number` field to the permission-card `ChatMessage`
  type (mirroring the existing `subAgentDepth` pattern used for other agent
  cards around line 1121 of useStreamProcessor.ts).
- In the `tokenicode_permission_request` handler, use the existing
  `resolveAgentId(msg.parent_tool_use_id, agents)` helper to determine
  whether the request came from a sub-agent. If `subAgentDepth > 0`, still
  add the permission card, but **skip** the
  `setActivityStatus(tabId, { phase: 'awaiting' })` call at line 871.
- In `InputBar.tsx`, change the `isAwaiting` derivation (line 358) to also
  require `activePermission?.subAgentDepth === 0` (fall back to `undefined →
  treat as main agent` for fail-safe behavior). Same treatment at line 1462's
  Send button disable expression.
- Default to "main agent" (lock input) when `parent_tool_use_id` or
  `subAgentDepth` is missing. Fail-safe is current behavior.

**Fixes**: Bug #39 (sub-agent permission locking main input).

**Risk**: MEDIUM. If a main-agent permission is mislabeled as sub-agent, input
stays unlocked during a real lock. Mitigation: default-to-main fail-safe.
Telemetry log counts parent_tool_use_id field rate during rollout so we know
if the CLI is consistently populating it.

#### Fix F4 — Deferred

Bugs #49, #44, #30 are deferred until user retests after F1+F2+F3 land. If any
still reproduce, user provides the Her-Desktop commit hash or issue body and a
follow-up task is created.

#### Ordering

1. F1 (smallest diff, highest leverage — fixes 2 bugs in one change)
2. F2 (needs F1's test harness to verify orphan queue drain)
3. F3 (independent of F1/F2; can be parallelized by a different agent but
   recommend serial execution to keep the diff reviewable)

**Constraints** (unchanged):
- No drive-by refactors of unrelated modules
- No renaming or restructuring beyond what's required for the fix
- No touching `providerStore`, `fileStore`, `agentStore`
- Every changed function gets a comment pointing at the bug number it fixes

**Exit criteria for Phase 2**: All Phase 1 tests pass. `pnpm run build` passes.
`cd src-tauri && cargo check && cargo clippy` passes clean. No new clippy
warnings.

### Phase 3: E2E Harness (AI-drivable)

**Goal**: Let Claude Code (an AI agent, not a human) autonomously run the
full user flow: launch TOKENICODE → new session → send message → verify
response → tear down.

**Winner**: **Approach #4 — Custom debug-IPC + Rust test CLI (`tokenicode-test`)**.

**Why this approach won** (from Research Agent's ranking):

| # | Approach | Setup | Coverage | macOS | Maint | CI |
|---|---|---|---|---|---|---|
| 1 | tauri-plugin-webdriver (OSS) | 3-5 days | 60% | **Blocked** (no WKWebView driver) | M | L |
| 2 | danielraffel/tauri-webdriver (OSS) | 3-5 days | 60% | Partial | M | L |
| 3 | CrabNebula @crabnebula/tauri-driver | 2-3 days | 80% | Yes but proprietary | L | M |
| **4** | **Custom debug-IPC + Rust test CLI** | **1-2 days** | **95%** | **Full** | **L** | **H** |
| 5 | HKUDS CLI-Anything autogen | 2-4 days | 50% (unknown) | Untested | H | L |
| 6 | Skip E2E, integration+unit only | 0 | 70% | Full | L | H |

Official `tauri-driver` cannot drive WKWebView on macOS (Apple ships no
WebDriver). Community OSS drivers have partial or unknown support. Commercial
options are proprietary. Approach #4 piggybacks on Tauri's existing IPC —
~90% of the plumbing already exists — and adds a small, gated test surface.

**Architecture**:

```
┌────────────────────────────────────────────────────────┐
│  tokenicode-test  (new Rust binary, src-tauri/src/bin/)│
│  ├─ parses YAML scenario                                │
│  ├─ spawns TOKENICODE with --features test-harness      │
│  ├─ issues __test_* Tauri commands in sequence          │
│  ├─ asserts expected state after each step              │
│  └─ exits 0 on success                                   │
└─────────────────────────────┬──────────────────────────┘
                              │  Tauri IPC (local)
                              ▼
┌────────────────────────────────────────────────────────┐
│  TOKENICODE with `test-harness` feature flag            │
│  (gated via #[cfg(feature = "test-harness")])           │
│  ├─ __test_set_working_directory(path)                  │
│  ├─ __test_set_input(text)                              │
│  ├─ __test_click_send()                                 │
│  ├─ __test_read_messages(tab_id) -> Vec<ChatMessage>    │
│  ├─ __test_wait_for_event(pattern, timeout_ms)          │
│  ├─ __test_get_active_tab_id()                          │
│  ├─ __test_kill_all_sessions()                          │
│  └─ existing Tauri runtime + React UI (unchanged)       │
└─────────────────────────────┬──────────────────────────┘
                              │  stdin/stdout (NDJSON)
                              ▼
┌────────────────────────────────────────────────────────┐
│  fake_claude_cli  (from Phase 0 fixture)                │
│  scenario: a_happy / b_partial_drop / c_compact_long /  │
│            d_subagent_perm / e_compact_during_switch    │
└────────────────────────────────────────────────────────┘
```

**Files to create**:

| File | Content |
|---|---|
| `src-tauri/src/test_commands.rs` (new) | `#[cfg(feature = "test-harness")]` module with 8 `__test_*` command handlers, all thin wrappers over existing Tauri commands and store state |
| `src-tauri/src/bin/tokenicode-test.rs` (new) | Main binary: parse YAML scenario, drive app via Tauri IPC, assertion DSL |
| `src-tauri/Cargo.toml` | Add `[features] test-harness = []` and `[[bin]] name = "tokenicode-test"` pointing at the new main.rs |
| `src-tauri/src/commands/cli_resolver.rs` | Tiny patch: at top of `find_binary`, honor `CLAUDE_BIN_OVERRIDE` env var if set (so tests can inject `fake_claude_cli`) |
| `e2e/scenarios/smoke.yaml` (new) | First E2E scenario: launch → set input "hello" → send → wait turn_complete → read messages → assert |
| `e2e/scenarios/repro_bug_57.yaml` (new) | Repro scenario for bug #57 to catch regressions at the E2E layer |
| `e2e/scenarios/repro_bug_27.yaml` (new) | Repro scenario for bug #27 |
| `e2e/scenarios/repro_bug_39.yaml` (new) | Repro scenario for bug #39 |
| `scripts/e2e-run.sh` (new) | Shell wrapper: `scripts/e2e-run.sh <scenario_name>` |

**Build command**:

```bash
# Build test-harness-enabled binary (once, or on code change)
cd src-tauri && cargo build --features test-harness

# Run a scenario
./scripts/e2e-run.sh e2e/scenarios/smoke.yaml
```

**Total additional code**: ~400 LOC Rust (test_commands.rs +
tokenicode-test main + bin bootstrap) + ~60 LOC YAML scenarios. Estimated
setup: 1-2 days.

**Exit criteria for Phase 3**:
- `cargo build --features test-harness` succeeds
- `./scripts/e2e-run.sh e2e/scenarios/smoke.yaml` launches the app, runs the
  scenario end-to-end, verifies the response, and exits 0
- Claude Code can run the same command with zero human intervention
- Release builds (`cargo build --release` without the feature flag) do NOT
  include any `__test_*` commands

**Fallback**: If the Tauri `#[cfg(feature = ...)]` approach hits unexpected
complications (e.g. shared state access from a separate binary), downgrade
Phase 3 to "Rust integration tests only" + a manual-smoke checklist in
`.trellis/spec/frontend/testing.md`. Bugs #57, #27, #39 are all already
covered by Phase 1 vitest hook tests, so Phase 3 is a nice-to-have for
catching wire-format drift, not strictly blocking.

### Phase 4: Spec Documentation + Regression Guard

**Goal**: Encode the routing invariants and test conventions into project spec
so the next AI agent doesn't re-break this class of bugs.

**Files to create / update**:

| File | Content |
|---|---|
| `.trellis/spec/backend/testing.md` | Fill in from Phase 0 fixture design + Phase 1 integration test examples |
| `.trellis/spec/backend/event-emission.md` (new) | Routing invariants, "what `stdinId → tabId` MUST and MUST NOT do" |
| `.trellis/spec/backend/process-management.md` (new) | `ProcessManager` / `StdinManager` contracts |
| `.trellis/spec/frontend/testing.md` | Fill in from Phase 0 setup + Phase 1 unit test examples |
| `.trellis/spec/frontend/state-management.md` | Cross-reference routing invariants from backend spec |
| `CLAUDE.md` | Add "Testing" subsection pointing at the new spec files + example commands |
| `.trellis/spec/backend/index.md` | Update to list all new files with "filled" status |

**Exit criteria for Phase 4**: A new AI agent starting a session and reading
CLAUDE.md can find the testing convention in under 2 clicks, and can find the
routing invariants in under 3 clicks.

---

## 4. Execution Model

**Dispatch plan** — how the above phases map to Trellis agents:

| Phase | Agent | Tool | Dispatch timing |
|---|---|---|---|
| Phase 0 | `implement` | Write/Edit files | Can run immediately after this plan is approved |
| Phase 1 | `implement` | Write test files + run `pnpm test` / `cargo test` to confirm they fail | After Phase 0 completes |
| Phase 2 | `implement` | Write fix + re-run tests | After Phase 1 completes and all tests confirmed failing |
| Phase 3 | `implement` | Add tooling + 3-5 smoke tests | After Phase 2 completes, gated on research agent's winner pick |
| Phase 3 fallback | `implement` | Manual-smoke doc + Rust integration coverage | If Phase 3 winner is "skip E2E" |
| Phase 4 | `implement` | Write spec files | After Phase 2 (doc can lag code slightly) |
| Any phase | `check` | Review diff against spec | After each `implement` agent finishes |
| Final | main agent | `git commit` on branch `suyuan` | User approval |

**Worktree vs in-place**: Because the scope is large and blast radius is high,
recommend running Phase 2 and Phase 3 in a git worktree via
`python3 ./.trellis/scripts/multi_agent/start.py .trellis/tasks/04-08-fix-cli-sdk-protocol-e2e-harness`.
Worktree isolates the fix from user's other work on `suyuan` branch.

---

## 5. Risk Register

Refined with research agent's findings:

| # | Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|---|
| R1 | **CONFIRMED** root cause is 4 distinct issues, not 1 shared routing primitive. Plan updated to reflect this. | — | — | N/A — already adapted. Each fix is scoped to its specific lines. |
| R2 | F1 adds cases to background handler but next refactor adds new foreground-only case, re-creating drift | Med | High | Extract a shared `applyResultCompletion(tabId, msg)` helper. Add a unit test that asserts both handlers respond to the same message type set. Document the invariant in `.trellis/spec/backend/event-emission.md` Phase 4. |
| R3 | F2 orphan queue grows unbounded under rapid session churn | Low | Med | Cap per-stdinId at 1 MB, total at 10 MB, TTL at 5 s. Log ERROR when cap is exceeded (not silent). |
| R4 | F3 frontend-only variant misclassifies a main-agent permission as sub-agent | Med | Med | Default to "main agent, lock input" when `parent_tool_use_id`/`subAgentDepth` is missing — fail-safe is current behavior. Add telemetry count so we know the fielding rate. |
| R5 | Fake CLI fixture can't reproduce timing races | Med | Med | Rust fixture uses `tokio::time::sleep` for controlled delays; scenarios include `e_compact_during_switch` specifically for timing repro. If still flaky, extract `process_stdout_line` as a pure function for deterministic unit testing. |
| R6 | E2E custom debug-IPC harness breaks on Tauri upgrade | Low | Med | Feature-flagged, isolated from production. If Tauri 2.x → 3.x upgrade breaks it, the harness can be rebuilt in days. |
| R7 | Rust test fixture + new bin crate adds perceptible build time to `cargo build` | Low | Low | Fake CLI is its own workspace member, not compiled during main `cargo build`. `tokenicode-test` bin and `test_commands.rs` are gated behind `test-harness` feature flag. |
| R8 | F1 fix changes background behavior in a way that a user relies on (unlikely but possible) | Low | Low | Silent drops are never desired behavior — no user can "rely" on them. |
| R9 | Fix diff exceeds 500 LOC (F1+F2+F3 combined) | Med | Med | Split into 3 commits per fix + 1 for Phase 0 infra + 1 for Phase 1 tests + 1 for Phase 4 specs = 6 commits on the fix branch, each reviewable independently. |
| R10 | Bugs #49/#44/#30 still reproduce after F1+F2+F3 ship | Low | Low | User retest. Spawn follow-up task with Her-Desktop source if needed. Non-blocking. |
| R11 | `CLAUDE_BIN_OVERRIDE` env var patch in `cli_resolver` introduces a security hole (user env var → binary path → arbitrary code execution) | Low | High | Gate the env var check behind `#[cfg(debug_assertions)]` OR `#[cfg(feature = "test-harness")]` so release builds do NOT honor it. |

---

## 6. Open Questions (from research agent)

**Zero blocking questions.** Research agent was instructed to auto-decide
everything possible, and it did. Two **non-blocking** items for a clarification
pass AFTER F1+F2+F3 land:

### 6.1 Bugs #49, #44, #30 — retest after F1+F2+F3

These three issues point at Her-Desktop's private issue tracker; we can't see
the symptom detail. Research agent's assessment is that F1+F2+F3 **very likely
subsume them** based on CHANGELOG keyword overlap ("desk_* 临时 ID 污染",
"stdinToTab 映射泄漏", "多会话串流污染 TK-329").

**Default action**: Ship F1+F2+F3, then ask user to retest all three. If any
still reproduce, user provides the Her-Desktop commit hash or issue body and
a follow-up F4 task is spawned.

**Blocking anything now**: No.

### 6.2 Fix F3 variant — frontend-only vs Rust+Frontend

Research agent recommended the **frontend-only variant** (simpler, uses
existing `resolveAgentId` helper). The Rust+Frontend variant would wire
`protocol.rs:49 agent_id` end-to-end but touches two layers and needs user
retest to confirm the CLI actually populates the field consistently.

**Default action**: Implement the frontend-only variant. If telemetry
indicates `parent_tool_use_id` is frequently missing from the CLI's
permission requests, spawn a follow-up task to wire the Rust side.

**Blocking anything now**: No.

---

## 7. Hand-off Checklist (for user waking up)

When you (suyuan) read this in the morning:

- [ ] Read `./prd.md` (5 minutes)
- [ ] Read Section 2 "Root Cause Findings" of this file (3 minutes)
- [ ] Read Section 6 "Open Questions" — if empty, skip
- [ ] Decide: proceed with full plan (Phase 0-4) or pause
- [ ] If proceed: run `python3 ./.trellis/scripts/multi_agent/start.py .trellis/tasks/04-08-fix-cli-sdk-protocol-e2e-harness`
- [ ] Or: tell main agent "go" and it will dispatch implement agents in-place

**Total morning read time**: ~10 minutes to decide whether to proceed.

---

## 8. Non-negotiables

These are things implementation MUST NOT do, even if a sub-agent thinks it's a
good idea:

1. **No `git push`** to any remote. Work stays local on branch
   `fix/cli-sdk-protocol-e2e-harness`.
2. **No deleting the owner's existing "fix" code** until a test proves the
   replacement is correct. Mark-and-sweep only.
3. **No switching test framework**. Use vitest (already installed) and cargo
   test (native). No jest, no playwright-only setups, no `cargo-nextest`
   requirement.
4. **No rewriting `sessionStore` or `chatStore` from scratch**. Incremental
   fixes only. The "v2 rewrite" path the owner took in Her-Desktop is the same
   trap — it produces a new surface with its own new bugs.
5. **No touching the release pipeline** (`latest.json`, `updater JSON`,
   `src-tauri/tauri.conf.json` updater endpoints) as part of this task.
6. **No installing global dependencies**. Everything local to the repo.
7. **No blocking on user input** during autonomous execution. Fail fast, log
   clearly, move on to the next item.

---

*This plan will be updated in-place as the research agent returns findings. See
git history for revisions.*
