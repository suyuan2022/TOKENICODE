# PRD: Fix CLI SDK Control Protocol Bugs + Bootstrap E2E Test Harness

**Task**: `04-08-fix-cli-sdk-protocol-e2e-harness`
**Created**: 2026-04-08
**Author**: suyuan (main session)
**Status**: planning → research → implement

---

## 1. Goal

Permanently eliminate the recurring class of "Claude CLI via SDK control protocol" bugs in TOKENICODE v0.10+ **and** establish a reproducible test infrastructure so the class of bugs cannot silently regress again.

Secondary goal: set up a harness that lets Claude Code (AI agent) autonomously drive TOKENICODE end-to-end — without human in the loop for verification — so future fixes can be validated without asking the user to manually click the GUI.

---

## 2. Context & Motivation

### 2.1 The pattern

Six GitHub issues (#57, #49, #44, #39, #30, #27) have been reported, closed as "fixed", and independently confirmed by user `suyuan2022` to still reproduce on v0.10.0 (the latest release at time of writing).

| # | Reported symptom | Owner's stated fix | Still broken? |
|---|---|---|---|
| 57 | Stream text stops after a few chars, reply exists in backend, restart shows full reply | "rAF flush fallback to selectedSessionId + auto-repair stdinId→tabId" | YES (user confirmed) |
| 49 | Multi-session bleed-over | "Fixed in Her-Desktop feat/session-isolation-v2, will sync" | YES (never synced) |
| 44 | "Same as Her-Desktop#96" (no details captured in this repo) | "Closed — sync later" | YES |
| 39 | Sub-agent running → input locked; Agent Team message handling | "Error classification mechanism" | Partial — sub-agent input lock confirmed |
| 30 | "Same fix as Her #64" | Not synced | YES |
| 27 | `/compact`, `/context` during window/session switch → spinner stuck forever, never recovers on refocus | "Same as Her #61" | YES — focus-loss drops completion event |

### 2.2 Meta-pattern diagnosis

- User is **not** the repo owner. Owner `yiliqi78` maintains a sibling repo "Her-Desktop" where fixes land first. Claims of "synced to TOKENICODE" do not hold up under testing.
- Even the one in-repo fix (#57 rAF flush fallback) is a patch on a symptom, not a fix on the root cause. User re-reported repro.
- Every one of the 6 bugs routes through a single fragile layer: **`stdinId ↔ tabId` session routing** (Rust side event emission + `sessionStore.stdinToTab` map + `chatStore` per-tab cache + `useStreamProcessor` foreground/background decision). Symptoms differ; mechanism is the same.

### 2.3 Why a test harness is not optional

TOKENICODE today:
- **Zero** `.test.ts` / `.spec.ts` files
- **Zero** Rust `_test.rs` or `#[cfg(test)]` modules in critical paths
- `package.json` has **no** `test` script (vitest 4.1.1 is installed but idle)
- `Cargo.toml` dev-deps only has `tempfile = "3"`

A fix without a regression test will silently rot the moment the next refactor touches `useStreamProcessor.ts` or `lib.rs`. The class of bugs has already demonstrated this — they've been reported, fixed, and broken again, repeatedly.

---

## 3. Scope

### 3.1 In scope

**Test infrastructure bootstrap (Phase 0)**:
- Add `"test"` and `"test:watch"` scripts to `package.json` (vitest)
- Set up `src-tauri/tests/` structure for cargo integration tests
- Build a **fake Claude CLI fixture binary** that scripts can use to replay NDJSON sequences with controlled timing (reproducing each of the 6 bug scenarios)
- Add a minimal test/fixture directory convention to `.trellis/spec/`

**Reproduction (Phase 1)**:
- For each of the 6 bugs, write a failing automated test that reproduces the exact user-reported behavior. Tests must be deterministic and runnable via `pnpm test` + `cargo test`.

**Fix (Phase 2)**:
- Fix the root cause(s) identified by Research Agent. Must make the Phase 1 tests pass. No cosmetic patches on symptoms — fixes must address the routing layer directly.

**E2E harness (Phase 3)**:
- Pick the highest-ROI mechanism for Claude Code to autonomously drive TOKENICODE's GUI. Candidate approaches have been pre-evaluated; Research Agent will pick the winner with justification.
- Implement a minimum harness that supports: launch app → create session → send message → wait for response → verify → tear down.
- 3-5 smoke tests covering the happy path + top 2 bug scenarios.

**Regression guard (Phase 4)**:
- `make test` (or equivalent) target that runs all layers
- Document the harness usage in `.trellis/spec/` so future AI agents know to add tests for any new `useStreamProcessor.ts` or `lib.rs` session-routing changes

**Spec documentation**:
- Write `.trellis/spec/frontend/testing.md` and `.trellis/spec/backend/testing.md` (new layer) describing test conventions, fake-CLI usage, and routing-layer invariants

### 3.2 Out of scope

- Any bug not in {#57, #49, #44, #39, #30, #27}
- Upstream PRs to Her-Desktop (fix lands on user's local `suyuan` branch; syncing upstream is a separate decision)
- Full GUI redesign or architectural rewrite
- Touching provider system, CLI install wizard, file explorer, rewind, etc — unless directly blocking one of the 6 bugs
- Production CI setup (local-dev harness only for Phase 3/4; CI can be added later)
- Windows-specific testing — macOS first, Windows when/if it's easy
- Fixing bugs in Her-Desktop (sibling repo)

---

## 4. Acceptance Criteria

### 4.1 Functional

- [ ] **Bug #57**: Automated test reproduces "stream text stops after a few chars under tab switch + draft promotion race". Test passes after fix. Manual smoke on running `pnpm tauri dev` confirms no regression.
- [ ] **Bug #49**: Automated test reproduces the multi-session isolation bug. Test passes after fix.
- [ ] **Bug #44**: Automated test reproduces whatever the Her-Desktop#96 class is (Research Agent will define based on code reading). Test passes.
- [ ] **Bug #39**: Automated test reproduces sub-agent input lock. Test passes. Agent Team message handling regression test exists and passes.
- [ ] **Bug #30**: Automated test reproduces. Test passes.
- [ ] **Bug #27**: Automated test reproduces "slash command during window/session switch → UI stuck". Test passes. Fix includes re-check-on-focus logic.

### 4.2 Test infrastructure

- [ ] `pnpm test` runs frontend vitest suite and exits 0
- [ ] `cd src-tauri && cargo test` runs backend tests and exits 0
- [ ] Fake Claude CLI fixture supports at least 6 pre-recorded scenarios mapping 1:1 to the 6 bugs
- [ ] Test run time end-to-end < 60 seconds on an M-series Mac (so it's actually runnable during dev)
- [ ] At least one E2E test exists that launches the real TOKENICODE binary, creates a session, sends a message through the fake CLI fixture, and verifies the response renders

### 4.3 Documentation

- [ ] `.trellis/spec/frontend/testing.md` exists with: test layer conventions, fake-CLI usage, examples, anti-patterns
- [ ] `.trellis/spec/backend/testing.md` exists (creating the `backend` spec layer) with: cargo test conventions, integration test patterns, fake-CLI fixture docs
- [ ] Cross-layer routing invariants documented: "What `stdinId → tabId` MUST and MUST NOT do"
- [ ] Implementation plan (`implementation-plan.md` in this task dir) captures the phase breakdown, decisions, and rationale
- [ ] CLAUDE.md gains a "Testing" section pointing at the new spec files

### 4.4 Code quality

- [ ] `pnpm run build` (frontend typecheck + vite build) passes clean
- [ ] `cd src-tauri && cargo check && cargo clippy` passes clean
- [ ] No new lint warnings introduced by the fix
- [ ] Fix diff fits within the routing layer; no drive-by refactors of unrelated modules

---

## 5. Constraints & Environment

- **Platform**: macOS 14.7.6 (arm64 / M-series). Dev-mode only — production builds are not part of this task.
- **User presence**: User is asleep tonight and has explicitly granted full autonomy. No decisions escalate to user during execution. Any ambiguity is resolved by: (1) existing code conventions, (2) CLAUDE.md / ARCHITECTURE.md, (3) the safer / smaller-blast-radius choice.
- **Destructive safety**: No `git push`, no `git reset --hard`, no branch deletion, no file system writes outside the repo, no uninstalling tools. Self-commits allowed on branch `suyuan` only.
- **Tool access**: Research Agent (background, opus) is already running. Implement Agent and Check Agent will be dispatched by a follow-up `/trellis:parallel` invocation (either from user or from this session when the research handoff is ready).
- **Zero TDD-hook constraint**: If a TDD hook is present that requires a test file for every new `.ts`, any new test files we create must cover any new source files. Rust side has no hook (cargo test is natively co-located), so Rust-side bootstrapping is unblocked.

---

## 6. Decisions Already Made (do not re-open)

| Decision | Rationale |
|---|---|
| Fix lands on branch `suyuan` first | User is not repo owner; upstream PR is a separate conversation |
| Rust-side tests use vanilla `cargo test` with fake CLI fixture | No new test framework dependency; zero hook interference; bugs are mostly backend |
| Frontend tests use the already-installed `vitest ^4.1.1` | Zero new devDeps on frontend; vitest is the obvious intended choice |
| Fake Claude CLI fixture is a compiled **Rust binary** (pending Research Agent confirmation) | Deterministic, cross-platform, no Python/shell dependency, can be reused by CI later |
| PRD language: English | Matches existing `.trellis/spec/frontend/index.md` convention |
| Task lives on branch `suyuan` (no worktree) unless Research Agent recommends otherwise | Avoids worktree overhead for a single-contributor task |
| `.trellis/spec/backend/` is a new spec layer | Currently only `frontend` exists; Rust backend needs its own conventions doc |

---

## 7. Open Risks

1. **Root cause may be multiple interacting bugs**, not a single `stdinId → tabId` issue. Research Agent must confirm or refute the single-root hypothesis.
2. **macOS E2E tooling is immature**. Official `tauri-driver` does not support macOS. Fallbacks exist (tauri-plugin-webdriver, CrabNebula, custom debug IPC) but each has trade-offs. Research Agent picks the winner; if the winner requires adding a Rust dependency, we accept that trade.
3. **Fake CLI fixture might not reproduce timing-sensitive races** exactly. Mitigation: add a "chaos mode" in the fake CLI that introduces controlled delays and emits out-of-order events.
4. **Fix for bug #57 might be bigger than anticipated** — if it requires changes in `sessionStore`, `chatStore`, `useStreamProcessor`, AND `lib.rs` simultaneously, the PR will be large. Mitigation: split into sub-PRs per layer if diff > 400 LOC.
5. **Claude Code autonomous E2E run** depends on being able to build and launch TOKENICODE in a scriptable way. If `pnpm tauri dev` can't be driven headlessly on macOS, Phase 3 downgrades from "AI-driven GUI E2E" to "Rust integration tests + manual smoke script". This is the explicit fallback.

---

## 8. Sequencing

Phase sequencing is described in detail in `implementation-plan.md` (written after Research Agent completes). High-level order:

```
Phase 0: Test infra bootstrap
  ├─ package.json scripts
  ├─ Cargo dev-deps
  ├─ Fake Claude CLI fixture
  └─ Test directory layout + fixture format

Phase 1: Reproduce (write failing tests)
  ├─ One test per bug, mapped to specific routing-layer invariant
  └─ All tests must fail on current 0.10 code

Phase 2: Fix root cause(s)
  ├─ Minimum diff to make Phase 1 tests pass
  └─ No drive-by refactors

Phase 3: E2E smoke harness
  ├─ Pick tooling (Research Agent decides)
  ├─ 3-5 smoke E2E tests
  └─ Document how AI agents invoke it

Phase 4: Spec + regression guard
  ├─ .trellis/spec/frontend/testing.md
  ├─ .trellis/spec/backend/testing.md (new layer)
  ├─ Routing-layer invariants doc
  └─ CLAUDE.md Testing section
```

---

## 9. Hand-off Instructions

When user wakes up, this PRD and the companion `implementation-plan.md` should be the first things reviewed. To kick off actual implementation, the user can:

**Option A** — resume this `/trellis:parallel` session and ask main agent to dispatch implement/check agents (risky for large diffs).

**Option B** — run `python3 ./.trellis/scripts/multi_agent/start.py .trellis/tasks/04-08-fix-cli-sdk-protocol-e2e-harness` to spawn a worktree agent (safer, isolated diff).

**Option C** — cherry-pick individual phases manually, running one implement agent per phase.

Main agent's recommendation after research completes will appear at the end of `implementation-plan.md`.
