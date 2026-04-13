# URGENT: Implementation Amendments + Ready-to-Paste Artifacts

> **IMPLEMENT AGENT**: Read this file AFTER `prd.md` and `implementation-plan.md`.
> Contains (a) corrections that supersede parts of implementation-plan.md §2/§3 and
> (b) ready-to-paste Phase 0 test infra code that eliminates research-from-scratch.
>
> Updated: 2026-04-08 based on two follow-up research agents (vitest-tauri-recon + her-desktop-forensics).

---

## Section A: Critical Amendments to implementation-plan.md

### A.1 — F1 is INCOMPLETE as originally written

`implementation-plan.md §3 Phase 2 Fix F1` says: add `case 'content_block_delta'` to the background handler.

**That's correct but not sufficient.** The background handler `handleBackgroundStreamMessage` at `src/hooks/useStreamProcessor.ts:213-691` is ALSO missing `case 'stream_event'`. The foreground handler processes `stream_event` at `useStreamProcessor.ts:924` — the background handler does not. **Both wrapper-form AND bare-form deltas destined for background tabs are silently dropped today.**

**F1 updated scope**:
- Add `case 'content_block_delta'` (bare) — original plan
- Add `case 'thinking_delta'` (bare) — original plan
- **NEW**: Add `case 'stream_event'` (wrapper) — unwrap and dispatch the inner `content_block_delta` / `thinking_delta` / `message_delta` / `message_stop` with the same routing as foreground
- Add `pendingCommandMsgId` clear inside background `case 'result'` at `useStreamProcessor.ts:539-627` — original plan

**Write tests for BOTH wrapper and bare forms.** If the test suite only covers bare deltas, the wrapper form bug will slip through regression.

### A.2 — F2 must patch TWO wipe sites, not one

`implementation-plan.md §3 Phase 2 Fix F2` only mentions the silent wipe at `useStreamProcessor.ts:72-76` inside `_scheduleStreamFlush`.

**There is an identical silent wipe at `useStreamProcessor.ts:111-115` inside `flushStreamBuffer`** — the explicit-flush path used when tearing down streams on `process_exit`, `clearPartial()`, etc. Patching only `_scheduleStreamFlush` leaves a known-defective second path live.

**F2 updated scope**:
- Replace silent wipe at L72-76 with orphan queue logic — original plan
- **NEW**: Replace silent wipe at L111-115 with the SAME orphan queue logic
- Both sites share the same orphan-queue Map, same TTL (5s), same per-stdinId cap (1 MB), same total cap (10 MB), same drain-on-registerStdinTab callback

### A.3 — F3 infrastructure is already 80% in place

`implementation-plan.md §3 Phase 2 Fix F3` describes adding `subAgentDepth` to permission cards. **Good news**: the type field and helpers already exist. Only 3 tiny call-sites need wiring:

| What | Status | File:line |
|---|---|---|
| `ChatMessage.subAgentDepth?: number` type field | ✅ EXISTS | `src/stores/chatStore.ts:76` (with comment: "0 = main agent, 1+ = inside Task sub-agent") |
| `resolveAgentId(parent_tool_use_id, agents)` helper | ✅ EXISTS, used extensively | `src/stores/agentStore.ts`, imported at `useStreamProcessor.ts:5` |
| `getAgentDepth(agentId, agents)` helper | ✅ EXISTS | same import |
| Foreground stream events already set `subAgentDepth` on message types | ✅ USED AT | `useStreamProcessor.ts:881-882, 1121, 1191, 1205, 1217, 1264, 1288, 1414, 1658` |
| **Foreground permission card construction** | ❌ MISSING `subAgentDepth` | `useStreamProcessor.ts:855-870` |
| **Background permission card construction** | ❌ MISSING `subAgentDepth` | `useStreamProcessor.ts:237-258, 281-294` |
| **Foreground `setActivityStatus({phase: 'awaiting'})`** | ❌ UNCONDITIONAL | `useStreamProcessor.ts:871` |
| **Background `setActivityStatus({phase: 'awaiting'})`** | ❌ UNCONDITIONAL | `useStreamProcessor.ts:259, 295` |
| **`InputBar.tsx isAwaiting` derivation** | ❌ DOES NOT CHECK `subAgentDepth` | `src/components/chat/InputBar.tsx:358, 1462` |

**F3 becomes a ~15 LOC patch across 5 call-sites**, not the larger change the original plan implied. Fail-safe remains: if `parent_tool_use_id` is missing or `subAgentDepth === undefined`, treat as main agent (lock input) — matches current behavior.

### A.4 — DANGEROUS NUMBERING COLLISION

> **WARNING to any agent writing code for this task.**
>
> TOKENICODE has its OWN issue #61 (Base64 image rendering, see `CHANGELOG.md:102`) and its OWN issue #64 (stream output interruption, see `CHANGELOG.md:74`). These are UNRELATED to the "Her #61" and "Her #64" references mentioned in the original TOKENICODE issue close comments.
>
> - "Her #61" (sub-agent input lock) ≠ TOKENICODE #61 (markdown image rendering)
> - "Her #64" (unknown symptom) ≠ TOKENICODE #64 (stream interruption, the recurring #57)
>
> Do NOT assume "the fix for Her #N is already in TOKENICODE" based on version numbers or changelog entries. Always verify by reading the actual code.

### A.5 — Fix #70/#142 message queue is ALREADY IN PLACE

The background `result` handler at `useStreamProcessor.ts:575-595` already drains `pendingUserMessages` FIFO (commit `191f7ff`, Her#142 port). The process_exit handler at `useStreamProcessor.ts:666-673` restores pending to draft. **Do not re-implement this.** If the user reports message queuing bugs after the fix lands, debug the existing implementation rather than rewriting it.

### A.6 — #49 / #44 / #30 recommendation

Research Agent B couldn't access Her-Desktop (no web tools in its context). Local delta analysis concluded:

| Bug | Status | Action |
|---|---|---|
| #49 (session isolation v2) | TOKENICODE 0.9.1 "chatStore v2" architecture IS in place (`chatStore.ts:182-235`). Residual gaps overlap exactly with F1+F2 silent-drop sites. | **Subsumed by F1+F2.** User retests after they land. |
| #44 (Her #96) | No local trace. Symptom unknown. | **Defer to user.** If retest reproduces, request upstream commit hash. |
| #30 (Her #64) | No local trace. **Beware numbering collision.** | **Defer to user.** Same posture. |
| #39 (Her #61) | Partial infra exists. | **F3 covers it.** |

---

## Section B: Ready-to-Paste Phase 0 Test Infrastructure

Agent A (vitest-tauri-recon) delivered a complete, verified bootstrap. Use these verbatim
unless you hit a real compatibility issue. **Don't re-research the vitest config — it's
already done.**

### B.0 Compatibility notes (read first)

- Use `defineConfig` from `vitest/config` (NOT `vite/config`) — vitest 4 requirement
- `happy-dom ^15` is compatible with React 19
- We do NOT install `@testing-library/react` initially — tests target stores/hooks, not components. Add later if component tests are needed.
- `@vitest/coverage-v8 ^4.1.1` requires Node 20.19+ (already implied by Vite 7)
- `happy-dom` must be added as a devDependency — not bundled with vitest
- `@testing-library/jest-dom` is OPTIONAL — omit unless component tests appear

### B.1 `vitest.config.ts` (create new file at repo root)

Use a SEPARATE vitest.config.ts (not merging into vite.config.ts) because `vite.config.ts` uses an async function for Tauri-specific config — vitest 4 best practice is isolation.

```ts
// vitest.config.ts
import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';
import { fileURLToPath } from 'node:url';

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
  test: {
    globals: true,
    environment: 'happy-dom',
    setupFiles: ['./src/test/setup.ts'],
    css: false,
    include: ['src/**/*.{test,spec}.{ts,tsx}'],
    exclude: [
      'node_modules/**',
      'dist/**',
      'src-tauri/**',
      '.trellis/**',
    ],
    clearMocks: true,
    restoreMocks: true,
    coverage: {
      provider: 'v8',
      reporter: ['text', 'html', 'lcov'],
      reportsDirectory: './coverage',
      include: ['src/**/*.{ts,tsx}'],
      exclude: [
        'src/main.tsx',
        'src/App.tsx',
        'src/**/*.d.ts',
        'src/test/**',
        'src/**/*.test.{ts,tsx}',
        'src/**/*.spec.{ts,tsx}',
        '**/*.config.*',
      ],
    },
  },
});
```

### B.2 `src/test/setup.ts` (create new file)

Single source-of-truth Tauri mock. Tests import `{ __tauriMock } from '@/test/setup'`
to drive invokes (`whenInvoke('cmd').resolves(value)`) and synthesize events
(`__emit('claude:stream:xxx', payload)`).

**Key design**: `mockInvoke` THROWS on unexpected commands so accidental real-IPC
paths fail loudly. Pre-script with `__tauriMock.whenInvoke(...).resolves(...)` or
`.handle(fn)`.

```ts
// src/test/setup.ts
import { vi, beforeEach } from 'vitest';

// Optional: enable when component tests are added.
// import '@testing-library/jest-dom/vitest';

// ---------------------------------------------------------------------------
// invoke() mock — per-test pre-scripted return values, throws on unexpected
// ---------------------------------------------------------------------------
type InvokeHandler = (args?: Record<string, unknown>) => unknown | Promise<unknown>;

const invokeQueue = new Map<string, Array<unknown | Error>>();
const invokeHandlers = new Map<string, InvokeHandler>();
const invokeCalls: Array<{ cmd: string; args?: Record<string, unknown> }> = [];

const mockInvoke = vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
  invokeCalls.push({ cmd, args });

  const handler = invokeHandlers.get(cmd);
  if (handler) return handler(args);

  const queue = invokeQueue.get(cmd);
  if (queue && queue.length > 0) {
    const next = queue.shift()!;
    if (next instanceof Error) throw next;
    return next;
  }

  throw new Error(
    `[tauri-mock] Unexpected invoke("${cmd}"). ` +
      `Pre-script with __tauriMock.whenInvoke("${cmd}").resolves(...) ` +
      `or __tauriMock.whenInvoke("${cmd}").handle(fn).`,
  );
});

vi.mock('@tauri-apps/api/core', () => ({
  invoke: mockInvoke,
  convertFileSrc: (path: string) => `mock://${path}`,
}));

// ---------------------------------------------------------------------------
// event mock — listen/emit with synthetic emission helper
// ---------------------------------------------------------------------------
type EventHandler = (event: { event: string; id: number; payload: unknown }) => void;

const eventListeners = new Map<string, Set<EventHandler>>();
let eventIdCounter = 0;

const mockListen = vi.fn(async (event: string, handler: EventHandler) => {
  let set = eventListeners.get(event);
  if (!set) {
    set = new Set();
    eventListeners.set(event, set);
  }
  set.add(handler);
  return () => {
    set!.delete(handler);
  };
});

const mockEmit = vi.fn(async (_event: string, _payload?: unknown) => {
  // no-op; tests can assert on .mock.calls
});

const mockOnce = vi.fn(async (event: string, handler: EventHandler) => {
  const unlisten = await mockListen(event, (e) => {
    handler(e);
    unlisten();
  });
  return unlisten;
});

function __emitTauriEvent(eventName: string, payload: unknown) {
  const set = eventListeners.get(eventName);
  if (!set || set.size === 0) {
    console.warn(`[tauri-mock] __emit("${eventName}") with no listeners`);
    return;
  }
  const id = ++eventIdCounter;
  for (const handler of set) {
    handler({ event: eventName, id, payload });
  }
}

vi.mock('@tauri-apps/api/event', () => ({
  listen: mockListen,
  emit: mockEmit,
  once: mockOnce,
  TauriEvent: {},
}));

// ---------------------------------------------------------------------------
// Plugin stubs — empty surface, every export is a vi.fn()
// ---------------------------------------------------------------------------
vi.mock('@tauri-apps/plugin-dialog', () => ({
  open: vi.fn(),
  save: vi.fn(),
  message: vi.fn(),
  ask: vi.fn(),
  confirm: vi.fn(),
}));

vi.mock('@tauri-apps/plugin-updater', () => ({
  check: vi.fn(async () => null),
}));

vi.mock('@tauri-apps/plugin-process', () => ({
  relaunch: vi.fn(),
  exit: vi.fn(),
}));

vi.mock('@tauri-apps/plugin-opener', () => ({
  openPath: vi.fn(),
  openUrl: vi.fn(),
  revealItemInDir: vi.fn(),
}));

// ---------------------------------------------------------------------------
// Public test API
// ---------------------------------------------------------------------------
export const __tauriMock = {
  invoke: mockInvoke,
  listen: mockListen,
  emit: mockEmit,
  calls: invokeCalls,
  /** Synthesize an incoming Tauri event to all registered listeners. */
  __emit: __emitTauriEvent,
  /** Pre-script invoke responses for a specific command. */
  whenInvoke(cmd: string) {
    return {
      resolves(value: unknown) {
        let q = invokeQueue.get(cmd);
        if (!q) invokeQueue.set(cmd, (q = []));
        q.push(value);
      },
      rejects(error: Error | string) {
        let q = invokeQueue.get(cmd);
        if (!q) invokeQueue.set(cmd, (q = []));
        q.push(error instanceof Error ? error : new Error(error));
      },
      handle(fn: InvokeHandler) {
        invokeHandlers.set(cmd, fn);
      },
    };
  },
  /** Reset all mock state. Called in beforeEach automatically. */
  reset() {
    invokeQueue.clear();
    invokeHandlers.clear();
    invokeCalls.length = 0;
    eventListeners.clear();
    eventIdCounter = 0;
    mockInvoke.mockClear();
    mockListen.mockClear();
    mockEmit.mockClear();
    mockOnce.mockClear();
  },
};

beforeEach(() => {
  __tauriMock.reset();
});
```

### B.3 `src/test/smoke.test.ts` (smoke test — verifies infra works)

```ts
// src/test/smoke.test.ts
import { describe, it, expect } from 'vitest';
import { __tauriMock } from './setup';

describe('test infrastructure smoke', () => {
  it('mock invoke is wired', () => {
    expect(typeof __tauriMock.invoke).toBe('function');
  });
  it('event emit helper is wired', () => {
    expect(typeof __tauriMock.__emit).toBe('function');
  });
});
```

### B.4 package.json `scripts` additions

Inside existing `"scripts": { ... }` block (preserve existing dev/build/preview/tauri):

```json
"test": "vitest run",
"test:watch": "vitest",
"test:ui": "vitest --ui",
"test:coverage": "vitest run --coverage",
"test:rust": "cd src-tauri && cargo test",
"test:all": "pnpm test && pnpm test:rust"
```

`test:ui` requires `@vitest/ui` — remove the line if skipping that dep.

### B.5 `src-tauri/Cargo.toml` `[dev-dependencies]` additions

Append next to existing `tempfile = "3"`:

```toml
[dev-dependencies]
tempfile = "3"
tokio-test = "0.4"
assert_cmd = "2"
predicates = "3"
```

Versions verified compatible with `tokio = "1"` and Rust edition 2021.

### B.6 Missing devDependencies to install

```bash
pnpm add -D happy-dom@^15 @vitest/ui@^4.1.1
```

### B.7 Example test file for useStreamProcessor (F1 regression)

Template demonstrating store reset + synthetic NDJSON event + partialText assertion.
`useStreamProcessor` uses rAF-throttled stream flush, so tests must await a frame +
microtask. happy-dom provides real rAF.

```ts
// src/hooks/useStreamProcessor.test.ts
import { describe, it, expect, beforeEach } from 'vitest';
import { renderHook, act } from '@testing-library/react'; // add @testing-library/react@^16
import { __tauriMock } from '@/test/setup';
import { useStreamProcessor } from '@/hooks/useStreamProcessor';
import { useChatStore } from '@/stores/chatStore';
import { useSessionStore } from '@/stores/sessionStore';

async function flushRaf() {
  await new Promise((resolve) => requestAnimationFrame(() => resolve(null)));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

describe('useStreamProcessor', () => {
  const TAB_ID = 'tab-test-1';
  const STDIN_ID = 'stdin-abc-123';

  beforeEach(() => {
    useChatStore.setState((prev) => ({
      ...prev,
      tabs: new Map([[TAB_ID, {
        messages: [],
        partialText: '',
        partialThinking: '',
        isStreaming: true,
        sessionId: null,
        cliSessionId: null,
        pendingUserMessage: null,
      } as any]]),
    }));
    useSessionStore.setState((prev) => ({
      ...prev,
      selectedSessionId: TAB_ID,
      stdinToTab: new Map([[STDIN_ID, TAB_ID]]),
    }));
  });

  it('#57-A: bare content_block_delta routes to background tab partialText', async () => {
    const refs = { current: null } as any;
    renderHook(() => useStreamProcessor(refs));

    await act(async () => {
      __tauriMock.__emit(`claude:stream:${STDIN_ID}`, {
        type: 'content_block_delta',
        delta: { type: 'text_delta', text: 'hello' },
      });
      await flushRaf();
    });

    const tab = useChatStore.getState().tabs.get(TAB_ID);
    expect(tab?.partialText).toBe('hello');
  });

  // Template for other 5 bug tests — per amendment A.1, include stream_event wrapper too.
  // it.todo('#57-A2: wrapped stream_event.content_block_delta routes to background tab');
  // it.todo('#57-B: _scheduleStreamFlush with null mapping preserves in orphan queue');
  // it.todo('#57-B2: flushStreamBuffer with null mapping preserves in orphan queue');
  // it.todo('#27: background result clears pendingCommandMsgId');
  // it.todo('#39: subagent permission does not lock main input');
});
```

**Note**: `useStreamProcessor` takes refs (check `useStreamProcessor.ts:1972` deps list for exact signature). Adapt `renderHook` call.

### B.8 `scripts/bootstrap-tests.sh` (idempotent one-shot setup)

Creates all files, patches package.json + Cargo.toml, installs deps, runs smoke suite.
Re-runnable without breakage.

```bash
#!/usr/bin/env bash
# scripts/bootstrap-tests.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"
echo "==> Bootstrap directory: $REPO_ROOT"

# 1. Create src/test/ directory
mkdir -p src/test

# 2. Write vitest.config.ts if not present — see RESEARCH-DELIVERABLES.md §B.1 for content
# 3. Write src/test/setup.ts if not present — see §B.2
# 4. Write src/test/smoke.test.ts if not present — see §B.3
# (use heredoc <<'EOF' ... EOF blocks to inline the content; copy verbatim from RESEARCH-DELIVERABLES.md)

# 5. Patch package.json scripts via Node (no jq dep)
node <<'NODE'
const fs = require('fs');
const path = 'package.json';
const pkg = JSON.parse(fs.readFileSync(path, 'utf8'));
pkg.scripts = pkg.scripts || {};
const additions = {
  test: 'vitest run',
  'test:watch': 'vitest',
  'test:ui': 'vitest --ui',
  'test:coverage': 'vitest run --coverage',
  'test:rust': 'cd src-tauri && cargo test',
  'test:all': 'pnpm test && pnpm test:rust',
};
let changed = false;
for (const [k, v] of Object.entries(additions)) {
  if (pkg.scripts[k] !== v) { pkg.scripts[k] = v; changed = true; }
}
if (changed) {
  fs.writeFileSync(path, JSON.stringify(pkg, null, 2) + '\n');
  console.log('package.json scripts updated');
}
NODE

# 6. Install missing devDeps
pnpm add -D happy-dom@^15 @vitest/ui@^4.1.1

# 7. Patch src-tauri/Cargo.toml dev-dependencies
CARGO=src-tauri/Cargo.toml
if ! grep -q '^tokio-test' "$CARGO"; then
  awk '
    /^tempfile = "3"/ {
      print
      print "tokio-test = \"0.4\""
      print "assert_cmd = \"2\""
      print "predicates = \"3\""
      next
    }
    { print }
  ' "$CARGO" > "$CARGO.tmp" && mv "$CARGO.tmp" "$CARGO"
  echo "Cargo.toml dev-dependencies patched"
fi

# 8. Run smoke
echo "==> Running pnpm test (smoke)"
pnpm test
echo "==> Bootstrap complete."
```

---

## Section C: Open Questions (non-blocking)

1. **Her-Desktop external research was not completed** — the research agent sub-type doesn't have web tools. If any of #49/#44/#30 still reproduce after F1+F2+F3 land, re-dispatch a research agent from a Claude session that has `WebFetch`/`WebSearch` or run `.claude/skills/web-tools/web_tools.py` with Bash. For now: proceed without this info.

2. **The `renderHook` pattern needs `@testing-library/react@^16`** (React 19 compatible). Add it when writing hook tests. Alternatively, tests can bypass `renderHook` and invoke the hook's effect manually by extracting the stream handler as a pure function — but that's more invasive.

3. **F2 orphan queue drain callback** — should fire when `sessionStore.registerStdinTab` is called. Implementation choices: (a) import a setter from the stream processor module into sessionStore (cross-module coupling), OR (b) subscribe the stream processor to sessionStore via Zustand subscribe API. Prefer (b) to keep stores decoupled.

---

## Section D: Execution Checklist for Implement Agent

```
Phase 0 — bootstrap:
[ ] Create vitest.config.ts from §B.1
[ ] Create src/test/setup.ts from §B.2
[ ] Create src/test/smoke.test.ts from §B.3
[ ] Add scripts to package.json from §B.4
[ ] Add dev-deps to src-tauri/Cargo.toml from §B.5
[ ] pnpm add -D happy-dom @vitest/ui per §B.6
[ ] pnpm add -D @testing-library/react@^16 (if writing hook tests)
[ ] pnpm test — confirm smoke.test.ts passes
[ ] cd src-tauri && cargo test — confirm empty suite builds

Phase 1 — failing tests (following amendments A.1/A.2):
[ ] Write useStreamProcessor.test.ts covering:
    [ ] #57-A: bare content_block_delta → background tab
    [ ] #57-A2: wrapped stream_event.content_block_delta → background tab  (NEW per A.1)
    [ ] #57-B: _scheduleStreamFlush orphan preservation
    [ ] #57-B2: flushStreamBuffer orphan preservation  (NEW per A.2)
    [ ] #27: background result clears pendingCommandMsgId
    [ ] #39: subagent permission does not lock main input
[ ] Write chatStore.test.ts (if needed for store-level invariants)
[ ] Write sessionStore.test.ts (if needed for stdinToTab invariants)
[ ] Create src-tauri/tests/fixtures/fake_claude_cli/ Rust crate
[ ] Create src-tauri/tests/integration/stream_routing.rs (optional for F1/F2, useful for future regression)
[ ] Run pnpm test — confirm ALL 6 target tests FAIL as expected

Phase 2 — fixes:
[ ] F1: add missing cases to handleBackgroundStreamMessage (bare + WRAPPER — per A.1)
[ ] F1: add pendingCommandMsgId clear in background result
[ ] F1: extract applyResultCompletion(tabId, msg) shared helper
[ ] F2: implement orphan queue (Map + TTL + caps + log-on-drop)
[ ] F2: patch _scheduleStreamFlush silent wipe
[ ] F2: patch flushStreamBuffer silent wipe  (NEW per A.2)
[ ] F2: subscribe stream processor to sessionStore.registerStdinTab for drain
[ ] F3: add subAgentDepth to permission cards (5 construction sites per A.3)
[ ] F3: gate setActivityStatus({phase:'awaiting'}) on subAgentDepth === 0 (3 sites)
[ ] F3: update InputBar.isAwaiting to check subAgentDepth
[ ] Re-run pnpm test — confirm all tests GREEN
[ ] pnpm run build — type check pass
[ ] cd src-tauri && cargo check && cargo clippy -- -D warnings

Phase 3 — E2E harness (if time + credits permit):
[ ] Add test-harness feature flag to src-tauri/Cargo.toml
[ ] Create src-tauri/src/test_commands.rs with 8 __test_* commands
[ ] Create src-tauri/src/bin/tokenicode-test.rs CLI driver
[ ] Patch src-tauri/src/commands/cli_resolver.rs to honor CLAUDE_BIN_OVERRIDE
[ ] Create e2e/scenarios/smoke.yaml
[ ] Build and smoke test

Phase 4 — specs:
[ ] Fill .trellis/spec/backend/testing.md
[ ] Fill .trellis/spec/frontend/testing.md
[ ] Write .trellis/spec/backend/event-emission.md (routing invariants)
[ ] Update CLAUDE.md with Testing section pointer
```

---

**End of RESEARCH-DELIVERABLES.md**
