# Fix Pre-existing Input State Bugs

**Slug**: `fix-input-state-bugs`
**Created**: 2026-04-08
**Discovered**: during manual GUI smoke test of `04-08-fix-cli-sdk-protocol-e2e-harness`
**Status**: planning
**Note**: Both bugs verified pre-existing in 0.10.0 — `04-08-fix-cli-sdk-protocol-e2e-harness` did not touch any related code paths. These are independent regressions to fix separately.

---

## Goal

修两个用户在手动 GUI 烟测中发现的 input-related state bugs：
1. 会话中切换模型后，下一条消息发送 GUI 状态卡住（实际消息已送达 CLI，重启 GUI 即恢复）
2. 拖文件到输入框时，除了正常的 file chip，还多出一个纯文本的文件路径

---

## Bug A: Mid-session Model Switch Deadlock

### Symptom (用户报告)

**复现路径**：
1. 在 active session 里，已经发过若干消息
2. 点 ModelSelector 切换模型（例：Opus 4.6 ↔ Sonnet 4.6），用 "继承系统配置" provider
3. 发送下一条消息
4. **卡住** — 表现为以下两种之一：
   - (a) 思考进度条**根本没出现**
   - (b) 思考进度条**只加载一点点（不到一半）就停住不动**
5. 工作区状态：手动重启 TOKENICODE → 重新进入对话 → **消息其实已经送达 CLI 并被回复**，UI 恢复正常

**结论**：CLI 进程层面是 OK 的（消息发到了，CLI 也回了），**前端 state / 渲染层面卡住**了。

### What I already know

- 触发代码路径：`src/components/chat/InputBar.tsx:855-921`
  - L875 检测 `currentModel !== spawnedModel`
  - L879 `bridge.killSession(stdinId)` 杀旧进程
  - L880-883 cleanup old listeners
  - L886 `setSessionMeta({stdinId: undefined, spawnedModel: undefined, modelSwitched: true, modelSwitchPendingText: text})`
  - L890-898 清除 thinking 块（避免 signature mismatch）
  - L899 `stdinId = undefined` → fall through 到 spawn new persistent process
- 自动 retry 路径：`src/hooks/useStreamProcessor.ts:1720-1843`
  - 仅在 `case 'result'` 且 `msg.subtype !== 'success'` 时触发
  - 检查 `isThinkingSignatureError` + `switchedFlag`
  - 如果匹配 → 杀掉失败进程 → 重新 spawn without `--resume`
- **死代码**：`bridge.setModel` 在 `tauri-bridge.ts:433` 定义但**全代码库零调用者**
- **没有 settingsStore subscribe watcher** for `selectedModel`（settingsStore.ts:345 只 watch `sessionMode`）

### Hypothesis (待 research 验证)

**最可能的 root cause**：kill 旧进程 + spawn 新进程之间的 state 转换不完整。具体可能性：

1. **`partialText` / `isStreaming` 残留**：旧进程被 kill 但 `clearPartial()` 没被调用（只在 `case 'result'` 触发，kill 不走 result 路径）→ 新进程开始流式时 `partialText` 已有残留 / `isStreaming` 状态错乱
2. **`activityStatus.phase` 卡在某个旧值**：可能旧进程的最后状态是 `'thinking'` 或 `'writing'`，新进程开始时没有 reset 回 `'thinking'`
3. **`pendingCommandMsgId` 残留**：上一次有 slash command → 切模型 → 残留的 pendingCommandMsgId 让新流被错误地当作 slash command 完成事件
4. **新 spawn 失败但没 throw**：`bridge.startSession` 静默 error，导致 `isRunning` 永远 true 但实际没 listener
5. **stdinToTab 映射 race**：旧 stdinId unregister 后，新 stdinId 还没 register 时来的 stream 事件被 F2 orphan queue 兜住，但永远不 drain（因为新 stdinId 已经成功 register 了，drain 只对相同 key 触发）

### Why my fix didn't cause this

`InputBar.tsx:855-920` 这段代码我**整晚一行没碰**（git diff 显示我对 InputBar.tsx 的全部改动只有 line 358 附近的 10 行 F3 isAwaiting gate）。useStreamProcessor.ts:1720-1843 的 retry 逻辑我也没改过。Result handler 的非 result 错误 path 是 owner 原代码。

但 F2 orphan queue 是新代码，可能有间接 side effect — research agent 需要确认。

---

## Bug B: File Drop Text Leak

### Symptom (用户报告)

**复现路径**：
1. 在我修复后的 dev build (`pnpm tauri dev` from worktree) 里
2. 拖一个文件到输入框
3. **看到**：除了正常的 rendered file chip（应该只有这个），**还多了一个纯文本的文件路径**

**对照**：用户记忆里 "原本只会显示被渲染后的样式"（应该指 0.10.0 vanilla / `suyuan` 分支），现在多了纯文本。

### What I already know

- 文件相关代码全部在我未触碰的文件里：
  - `src/hooks/useFileAttachments.ts`（drag-drop / paste / 文件上传逻辑）
  - `src/components/chat/TiptapEditor.tsx`（输入框，处理 drop event）
  - `src/lib/drag-state.ts`（拖拽 state 共享）
- **`git diff suyuan -- src/hooks/useFileAttachments.ts src/components/chat/TiptapEditor.tsx src/lib/drag-state.ts` 输出空** — 我一行没碰这三个文件
- TiptapEditor 是 ProseMirror-based 富文本编辑器，drop event 通过 prosemirror-view 的 `handleDrop` API 处理

### Hypothesis (待 research 验证)

**最可能的 root cause** 候选：

1. **TiptapEditor 的 drop handler 没 preventDefault**：导致浏览器默认行为（把文件路径作为 text 插入）和 TOKENICODE 的 file chip 渲染并发执行 → 两个都出现
2. **拖拽进入区域有两个 listener**：InputBar 一个 drop handler + TiptapEditor 内部一个 drop handler，两个都触发 → 一个建 chip 一个粘文本
3. **OS native drag 和 browser drag 混淆**：macOS 拖文件进 webview 时，文件路径既以 native 路径形式（被 Tauri 处理建 chip），又以 text/plain MIME 形式（被浏览器处理插入文本）
4. **macOS WKWebView 在某次 macOS 升级后改变了 drag 行为**：跟 TOKENICODE 代码无关，只是用户最近升级 macOS 才出现

### Why my fix didn't cause this

零文件触碰 + 没有任何对 TiptapEditor / file attachment 路径的间接 dependency。

---

## Out of Scope

- **不修上游 Her-Desktop**（这是本地 fork 修复）
- **不重写 TiptapEditor / 文件上传系统**（最小修复）
- **不做完整的 mid-session state machine 重写**（最小修复）
- **不阻塞 `04-08-fix-cli-sdk-protocol-e2e-harness` 任务的 ship**（那个 commit 无 regression）

---

## Acceptance Criteria

### Bug A: Model switch
- [ ] 复现路径：在 active session 切模型 → 发消息 → **不卡**，直接显示思考进度条 → 正常出回复
- [ ] 双向都通：Opus → Sonnet → Opus，每次都 OK
- [ ] 重启 GUI 不再是 workaround
- [ ] 没有引入新的 GUI 状态 bug
- [ ] 加 vitest regression test 测这个 case（可能需要 mock 整个 kill+respawn 流程）

### Bug B: File drop text leak
- [ ] 拖文件到输入框 → 只显示 file chip，**不出现**纯文本路径
- [ ] 多种文件类型都验证（.txt, .md, .png, .pdf）
- [ ] 拖放后能正常发送消息，CLI 收到的是 file chip 对应的 attachment
- [ ] 加 vitest regression test（如果测试 setup 允许 — TiptapEditor 渲染需要 jsdom，可能需要 component test）

### 验证矩阵
- [ ] `pnpm build` 通过
- [ ] `pnpm test` 通过（含新增 test）
- [ ] `cargo check` 通过（如果 Rust 也改了的话）
- [ ] `cargo test` 通过
- [ ] 手动 GUI 烟测：A bug 复现序列 + B bug 复现序列都 PASS
- [ ] Codex 双模型 review 通过

---

## Open Questions

无（research agent 会自动处理 root cause analysis）

---

## Technical Notes

### 调查范围

**Bug A (model switch)**:
- `src/components/chat/InputBar.tsx:855-921` (kill+respawn 入口)
- `src/components/chat/InputBar.tsx:925+` (spawn new process 路径)
- `src/hooks/useStreamProcessor.ts:1700-1850` (result + retry)
- `src/stores/chatStore.ts` 的 `partialText` / `isStreaming` / `pendingCommandMsgId` / `activityStatus` 字段相关 mutators
- `src/stores/sessionStore.ts` 的 `stdinToTab` 管理
- `src-tauri/src/lib.rs::start_claude_session` 的 spawn 路径
- `src-tauri/src/commands/claude_process.rs` 的 `kill_session` 实现

**Bug B (file drop)**:
- `src/hooks/useFileAttachments.ts` 全文件
- `src/components/chat/TiptapEditor.tsx` drop / paste 处理
- `src/lib/drag-state.ts`
- `src/components/chat/InputBar.tsx` 中 drop event 相关代码（如有）
- ProseMirror handleDrop 行为
- Tauri 的 file drop event API

### Worktree 位置

继续在 `/Users/suyuan/Documents/夙愿's库/01 主业/01 Her产品/源码/trellis-worktrees/fix/cli-sdk-protocol-e2e-harness` 工作。**不开新 branch**——这是 separate fix 但物理上跟 F1/F2/F3 修复同 worktree。Commit 时分独立 commit 区分。

### Git baseline

可以用 `git diff suyuan -- <file>` 验证我的 fix 没碰过相关文件。任何新发现的 hypothesis 必须先 `git log -- <file>` 看历史，确认 owner 之前怎么改的，避免重蹈覆辙。
