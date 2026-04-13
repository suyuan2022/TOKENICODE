# PRD: Fix stale process_exit race condition + interrupt recovery + empty message guard

**Task**: `04-11-fix-stdinid-race`
**Created**: 2026-04-11
**Author**: suyuan
**Status**: completed
**Consolidates**: `04-10-fix-haiku-cli-streaming` (partial), `04-11-test-discovered-bugs` Bug 2 & Bug 3

---

## Root Cause Analysis

### Core Bug: Stale `process_exit` / `result` events clobber new session state

When a CLI process is killed (stop button, model switch, provider switch) and a new process is immediately spawned, the old process's `process_exit` event arrives **after** the new session starts. The handler unconditionally overwrites the tab's state:

```
Stop → kill(stdinId=A) → setSessionMeta({ stdinId: undefined }), setStatus('completed')
User sends new msg → spawn(stdinId=B) → setStatus('running')
Old process_exit(A) arrives → setStatus('idle')              ← overwrites 'running'!
                            → setSessionMeta({ stdinId: undefined }) ← clears B!
```

**Impact**: New process produces output but frontend thinks no session is active. Messages silently lost.

**Affected flows**:
- Bug 2 from `test-discovered-bugs`: stop → re-send → message silently dropped (100% repro)
- Haiku issue from `fix-haiku-cli-streaming`: model switch → new process state clobbered by old exit
- Any model switch or provider switch mid-session

### Secondary Bug: Empty message can be sent (Bug 3)

`InputBar.tsx` line 694 has `if (!text) return;` but TipTap editor can produce invisible Unicode characters (zero-width spaces, BOM) that survive `trim()`.

---

## Fix Plan

### Fix 1: Stale stdinId guard in `useStreamProcessor.ts`

**Location**: `src/hooks/useStreamProcessor.ts`

In both `result` and `process_exit` handlers, add an ownership check before modifying session state:

```typescript
// At the start of 'result' case (after sub-agent break):
const currentStdinId = useChatStore.getState().getTab(tabId)?.sessionMeta.stdinId;
if (msgStdinId && currentStdinId && msgStdinId !== currentStdinId) {
  // Stale result from a killed/replaced process — clean up listener only
  if ((window as any).__claudeUnlisteners?.[msgStdinId]) {
    (window as any).__claudeUnlisteners[msgStdinId]();
    delete (window as any).__claudeUnlisteners[msgStdinId];
  }
  if (msgStdinId) useSessionStore.getState().unregisterStdinTab(msgStdinId);
  break;
}

// Same guard at the start of 'process_exit' case.
```

### Fix 2: Empty message guard in `InputBar.tsx`

**Location**: `src/components/chat/InputBar.tsx` line ~694

Replace simple `if (!text) return;` with a regex strip of invisible characters:

```typescript
text = text.replace(/[\u200B-\u200D\uFEFF\u00A0]/g, '').trim();
if (!text) return;
```

### Fix 3: Background `result` handler also needs the same guard

**Location**: `useStreamProcessor.ts` background handler — same stale check needed for the `result` case in `handleBackgroundStreamMessage`.

---

## Acceptance Criteria

- [ ] After stop → re-send, new message gets a response (not silently dropped)
- [ ] After model switch → send, new model's response appears correctly
- [ ] Empty editor content cannot produce a user message bubble
- [ ] Stale `process_exit` from killed process does NOT clobber active session state
- [ ] No regression: normal send/receive flow works unchanged
- [ ] TypeScript compiles without errors

## Files to Modify

1. `src/hooks/useStreamProcessor.ts` — stale stdinId guard in `result` + `process_exit` handlers (foreground & background)
2. `src/components/chat/InputBar.tsx` — invisible character strip before empty check
