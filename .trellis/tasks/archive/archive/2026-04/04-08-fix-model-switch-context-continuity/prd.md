# Mid-Session Model Switch Must Preserve Conversation Context

**Slug**: `fix-model-switch-context-continuity`
**Created**: 2026-04-08
**Discovered**: during manual GUI testing of `04-08-fix-input-state-bugs` Round 3 fix
**Priority**: P0 (blocks daily use; user cannot switch models mid-conversation)
**Status**: completed (PR #77 fix/model-switch-thinking-strip)

---

## Problem Statement (verbatim from user)

> 我选择一个模型发消息过去之后，再选另外一个模型发消息，它虽然不阻塞了，但实际上是相当于开了一个新的会话来处理。
>
> 也就相当于：
> 1. 发送消息到新选择的模型时，根本没有延续当前的会话
> 2. 用户在实际使用时，是需要在同一个会话里面切换到别的模型继续使用的
>
> 这根本不行，这里出了问题

**Translation**: After switching model mid-session, the next message silently starts a FRESH CLI process with no prior conversation history. The new model sees only the current message, not the earlier conversation. This is functionally a new session disguised as a continuation. The user expects to continue the SAME conversation with a different model.

---

## Root Cause of the Regression

This bug was introduced by Fix A2 in the `04-08-fix-input-state-bugs` task (commit pending, in worktree `fix/cli-sdk-protocol-e2e-harness`).

**Fix A2's intent**: the old behavior was to pass `resume_session_id` (the previous CLI UUID) to `bridge.startSession` after a model switch. The new CLI process would `--resume` the old JSONL, which contains thinking blocks with the OLD model's cryptographic signatures. The NEW model rejects those signatures → Anthropic API 400 → deadlocked thinking bar (the original "Bug A stuck").

**Fix A2's trade-off**: skip `resume_session_id` entirely when `modelSwitched || providerSwitched` is true. This avoids the 400, but starts a fresh CLI process with no history. **Both Codex models (5.4 + 5.2) flagged this trade-off in Round 1 review as "损失 CLI 上下文" but Round 2 shipped it anyway** because there was no alternative wired through the existing `bridge.startSession` API.

**User's actual requirement**: the new model must see the full prior conversation. Fresh-spawn-without-history is NOT acceptable.

---

## What Fix A2 Actually Does (so the next implementer doesn't redo the wrong thing)

Location: `src/components/chat/InputBar.tsx:942-947` (line numbers as of Round 3 commit in worktree).

```ts
const rawSessionId = getActiveTabState().sessionMeta.sessionId;
const wasModelSwitched = getActiveTabState().sessionMeta.modelSwitched;
const wasProviderSwitched = getActiveTabState().sessionMeta.providerSwitched;
const existingSessionId = rawSessionId && !rawSessionId.startsWith('desk_')
  && !wasModelSwitched && !wasProviderSwitched
  ? rawSessionId
  : undefined;
```

Then at `bridge.startSession({ resume_session_id: existingSessionId, ... })`, when `existingSessionId` is `undefined`, the Rust side spawns `claude` WITHOUT `--resume`, so the process gets no history.

**When the fix fires**: the original thinking-block cleanup at `InputBar.tsx:894-899` (part of Round 2) strips thinking messages from the frontend `tab.messages` array BEFORE the spawn. Codex pointed out this is cosmetic — `startSession` does not read frontend messages to construct CLI context; it only passes `prompt`, `resume_session_id`, and `model`. So the frontend cleanup is dead code WRT CLI state.

---

## Why Simply Reverting Fix A2 Doesn't Work

Reverting = always pass `resume_session_id` → `--resume` → CLI loads JSONL → JSONL has Opus thinking blocks → Sonnet rejects signatures → 400 error → deadlock UI. **The original Bug A.**

The auto-retry regex (Round 1 Fix A1) + broadened regex (Round 2) + `switchedFlag` hard gate (Round 2) IS supposed to catch this 400 and retry without `--resume`, but per the original Codex review, the retry ALSO loses context because it spawns fresh. So even if we remove Fix A2 and rely solely on the retry path, the context is still lost after the retry fires — the user just gets their text through after a brief error round-trip instead of a silent fresh-spawn.

**No fix that relies on `bridge.startSession`'s current API can preserve context across models.** The API only accepts `prompt` and `resume_session_id`. A real fix requires expanding the API or working around it.

---

## Candidate Solutions (for the next session to evaluate)

### Option 1: Strip thinking blocks from the JSONL file before resume

Modify the session JSONL file at `~/.claude/projects/<encoded>/sessions/<uuid>.jsonl` to remove lines containing thinking blocks, THEN pass `resume_session_id` to spawn. The new model loads a history without thinking signatures.

**Pros**:
- Minimal API change
- Preserves user/assistant text messages (the valuable context)
- Reversible (backup JSONL before mutation)

**Cons**:
- Touches disk state that Claude CLI owns — CLI may reject mutated JSONL
- Needs to identify which lines are thinking blocks (parse NDJSON, filter)
- Thinking blocks on resume are actually useful context for the same model — stripping them always is wasteful when switching back to the same model

**Risk**: requires understanding the JSONL schema well enough to strip without corrupting.

### Option 2: Add a `history` parameter to `bridge.startSession`

Extend the Rust side to accept a `history: Vec<UserMessage>` field. Frontend extracts user+assistant text from `tab.messages`, sends as history. Rust writes these as initial messages to the new process via stdin before the user's actual prompt.

**Pros**:
- Clean API boundary — no disk mutation
- Frontend stays in control of what gets replayed
- Works for arbitrary cross-model switches

**Cons**:
- Requires new Rust command or new field in `StartSessionParams`
- The replayed messages are "user messages" to the new process, not true continuation — the new model sees them as one-shot context, not a multi-turn conversation
- Token cost: the full history gets re-prompted every switch

**Risk**: Claude CLI may not support stdin-injected historical messages; needs testing.

### Option 3: Use `--replay-user-messages` flag

The existing CLI invocation pattern in ARCHITECTURE.md mentions `--replay-user-messages` is already passed. Investigate whether this flag, combined with `--resume`, instructs the CLI to replay only user messages (stripping thinking blocks automatically).

**Investigation needed**: read `src-tauri/src/lib.rs::start_claude_session` to confirm the flag is set, then test empirically whether `--resume` + `--replay-user-messages` handles the thinking signature case gracefully. If yes, Fix A2 can be reverted AND the 400 auto-retry path can stay as a safety net.

**Pros**:
- Might be a one-line fix if the flag already does what we need
- Leverages CLI-native behavior, no frontend hacks

**Cons**:
- Unknown until tested; may not actually work

**Start here** — this is the cheapest investigation.

### Option 4: Kill + start fresh with a "handover prompt" that summarizes prior context

Instead of `--resume`, spawn fresh with a synthesized first prompt like:

```
You are continuing a conversation. Previous context:

USER: <prior message 1>
ASSISTANT: <prior response 1>
USER: <prior message 2>
ASSISTANT: <prior response 2>
...

USER's current message: <actual prompt>
```

**Pros**:
- No disk mutation, no new Rust API
- Works universally across providers/models

**Cons**:
- The "handover prompt" is visible in the new session's first user message, which pollutes the chat UI
- Context window consumption — the handover eats tokens
- Not multi-turn aware (the new model thinks the history is one giant preamble)

### Option 5: Summarize-before-switch

Before spawning the new model, ask the OLD model for a summary. Pass the summary as the new model's first prompt context. Loses detail but preserves gist.

**Pros**:
- Low token cost
- Clean user-facing UX

**Cons**:
- Adds a round-trip latency to every model switch
- Summary quality depends on old model's behavior
- Not what the user explicitly asked for (they want "continue the conversation", not "start with a summary")

---

## Recommended Investigation Order (for next session)

1. **Option 3 first** (cheapest): read `lib.rs::start_claude_session` and confirm whether `--replay-user-messages` is already set. If yes, write a manual smoke test: kill session, spawn with `--resume <uuid> --model <new>`, check behavior. If CLI handles thinking signature stripping automatically, we're done with a 5-line fix.

2. **Option 1** (if Option 3 fails): investigate JSONL schema, write a pure function `strip_thinking_blocks(jsonl_path) -> Result<()>` in Rust, call it before spawn in the model-switch path.

3. **Option 2** (if Option 1 is too risky): add `history` param to `StartSessionParams`, wire through frontend.

4. **Options 4/5** only if 1-3 all fail.

---

## Acceptance Criteria

- [ ] Manual repro: in an active session, send 3-5 messages, switch model, send another message → the new model's response references the earlier conversation (proves context is preserved).
- [ ] Double-direction: Opus → Sonnet → Opus, each switch preserves context.
- [ ] Provider switch: same behavior for provider change (not just model within same provider).
- [ ] No regression on the original Bug A: thinking bar does NOT deadlock.
- [ ] No regression on Round 2/Round 3 fixes in `04-08-fix-input-state-bugs`: regex gate, unregister ordering, test suite 28/28 still pass.
- [ ] Codex double review on the fix verifies no new hidden bugs.

---

## Out of Scope

- Fixing the "dead-code thinking-block frontend cleanup" at `InputBar.tsx:894-899` — can be cleaned up as a P3 hygiene item in a separate task.
- Implementing a handover-prompt summarization UX (Option 5).
- Changing how Claude CLI handles thinking signatures (upstream concern).

---

## Context From Previous Session (to brief the next one)

### What's already done in `fix/cli-sdk-protocol-e2e-harness` worktree

- F1/F2/F3 core fixes for the SDK control protocol bugs (#57 #49 #44 #39 #30 #27) — committed as `db30599`
- Bug B (file drop text leak) fixed via `handleDrop` in TiptapEditor — **Codex ✅, manually verified ✅**
- Bug A Round 1/2/3 fixes — Codex approved (SHIP_READY for routing race, regex tightening, helper extraction, test quality) BUT the user-facing behavior is wrong per this task
- 28/28 vitest tests pass
- `pnpm build` green, `cargo check` green, `cargo test perm_payload_tests` 6/6

### What to NOT touch in the next session

- The Round 3 state machine fixes (reorder `kill → unlisten → unregister` at 7 sites) — those are CORRECT structurally and should survive into the new fix
- The regex helper `isThinkingSignatureError` + `THINKING_SIGNATURE_ERROR_REGEX` + `MISMATCH_INDICATOR_REGEX` — useful as a safety net
- The Phase 3 E2E harness scaffold in the worktree — out of scope

### What likely needs to be reverted in the next session

- **Fix A2** at `InputBar.tsx:942-947`: the `wasModelSwitched/wasProviderSwitched` guard that strips `existingSessionId`. After the real fix (Option 1/2/3) works, this guard should be removed so `--resume` is always attempted.
- The thinking-block cleanup at `InputBar.tsx:894-899` may also need to be removed or re-purposed.

### Dead-code finding (Codex confirmed)

`InputBar.tsx:894-899` filters `m.type !== 'thinking'` from `tab.messages` during model switch. **This does NOT affect CLI resume** — `bridge.startSession` doesn't read frontend messages. Can be removed in any future cleanup.

---

## Key File Pointers (for the next session)

- `src/components/chat/InputBar.tsx:855-1115` — the entire model-switch + spawn flow
- `src/hooks/useStreamProcessor.ts:1720-1845` — auto-retry logic (Round 1 A1 + Round 2 regex helper)
- `src-tauri/src/lib.rs::start_claude_session` — where `--resume` and `--replay-user-messages` CLI flags are added to the spawn command
- `~/.claude/projects/<encoded-path>/sessions/<uuid>.jsonl` — session JSONL file format (for Option 1 investigation)
- `.trellis/tasks/04-08-fix-input-state-bugs/prd.md` — the task that introduced the regression
- `.trellis/tasks/04-08-fix-input-state-bugs/FIX-LOG.md` — detailed Round 1/2/3 fix log

---

## Meta Note

This task exists because the current session context is too long for productive further work. Per user instruction: **create this task, run `/trellis:finish-work` to record progress on the current worktree, then STOP.** Start fresh session for this task.
