# PRD: 全量 Issue 回归测试发现的 Bug

**Slug**: `test-discovered-bugs`
**Created**: 2026-04-11
**Discovered**: 全量 issue 回归测试（260 次执行 / 12 suite / 34 轮）
**Owner**: suyuan
**Status**: partial (Bug 2+3 已修，Bug 1 未修)

> **2026-04-11 修复记录**：
> - Bug 1 (Webview 冻结 P0): **未修**。确认为累积 session 导致 JS 退化，非代码 bug。后续 task: `04-11-fix-test-infra-stability`
> - Bug 2 (中断后丢消息 P1): **已修**。stale stdinId guard + discardStreamBuffer + 提前写 stdinId。3 轮 E2E 验证通过 (15/15)
> - Bug 3 (空消息可发送 P3): **已修**。Unicode 视觉判空。E2E 验证通过
**Priority**: P0

---

## Bug 1: Webview JS 执行引擎周期性冻结 [P0]

LLM 交互后 10-60s，前端 JS 执行引擎冻住。`ping`（Rust socket）正常，`status`（JS 执行）超时。`restart` 无法恢复，只有 `relaunch` 能恢复。260 次测试中 53 次（20%）因此失败。可能是 #80 #57 #64 #27 #4 的共同底层原因。

**复现**:
```bash
TKN="node scripts/tokenicode-cli.mjs"
$TKN new-session --cwd /tmp/tokenicode-test
$TKN type "请用300字介绍量子计算" && $TKN send
$TKN wait-until-done --timeout 60000
# 10-60s 后:
$TKN ping    # → ok
$TKN status  # → Timeout waiting for JS execution
```

**推测根因**: streaming NDJSON 解析阻塞 JS 主线程 / Tauri evaluate_js 通道竞争 / dev 构建 HMR 开销。需先在 release 构建下验证。

**附加发现**: relaunch 进程泄漏 — 旧 `target/debug/tokenicode` 子进程杀不干净，无人值守 5 小时累积 76 个僵尸进程。

**关联文件**: `useStreamProcessor.ts`, `App.tsx`, `lib.rs` (stdout loop)
**详细记录**: `.test/notes-webview-freeze.md`

---

## Bug 2: 中断后再发消息丢失 [P1] — GitHub #80

stop 中断 thinking/writing 阶段后 re-send，新消息被静默吞掉。`wait-until-done` 839ms 极速返回 completed（检测的是旧 idle 状态），`get-messages` 只有 2 条（应 ≥3）。有效测试中 100% 复现。

**精确复现**（来源 `.test/interrupt-recovery/reports/run-001.json` T02）:
```
1. new-session → send 长消息 → wait-for-phase thinking (370ms)
2. stop → delay 2s → status: active=false ✓
3. type 新消息 → send
4. wait-until-done → 839ms completed ← 可疑
5. get-messages → total: 2 ← 缺第二条回复，FAIL
```

**推测根因**: `InputBar.tsx` handleSubmit 在 stop 后 streaming state 未重置，sendStdin 被跳过或 stream listener 未重新挂载。

**关联文件**: `InputBar.tsx`, `useStreamProcessor.ts`, `chatStore.ts`
**验证**: 跑 `.test/interrupt-recovery/suites/interrupt-then-send.json` T01-T03

---

## Bug 3: 空消息可以被发送 [P3]

编辑器无内容时 `send`，`get-messages` 返回 total:1（应为 0）。

**修复**: `InputBar.tsx` handleSubmit 加 `if (!content?.trim()) return;`
**来源**: `.test/basic-chat/reports/run-03.json` T04
**验证**: `.test/basic-chat/suites/send-receive.json` T04

---

## 测试报告入口

- 完整报告: `.test/FULL-TEST-REPORT.md`
- Webview 冻结详记: `.test/notes-webview-freeze.md`
- 分析脚本: `python3 .test/analyze-reports.py`
- 所有报告: `.test/*/reports/run-*.json`
