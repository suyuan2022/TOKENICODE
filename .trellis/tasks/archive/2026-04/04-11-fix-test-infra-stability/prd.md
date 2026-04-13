# PRD: 测试基础设施稳定性修复

**Task**: `04-11-fix-test-infra-stability`
**Created**: 2026-04-11
**Status**: pending
**Blocked by**: 无（可直接开始）

---

## 背景

2026-04-11 本轮修复完成后，跑了 57 个 E2E 测试（3 suite × 5 rounds），35% pass rate。所有失败均为**基础设施问题**，非代码 bug：
- 24 次 JS execution timeout（累积 400+ session 后 webview 退化）
- 11 次 wait-for-phase timeout（API 速率限制）

同样的测试在前 3 轮单独跑时 15/15 全过。

## 问题 1: 累积 session 导致 webview 退化

每个测试 `new-session` 建新 tab 但 teardown 只 `stop`（停 CLI 进程），不删除 tab。跑 50+ 测试后 sessionCount 超 400，Zustand state 膨胀，每次 setState 触发所有 subscriber 检查更新，JS 主线程越来越慢。

### 修复方案

在 `scripts/tokenicode-cli.mjs` 中新增 `delete-session` 命令，或在测试 teardown 中用 `exec` 命令直接调 store 清理当前 tab：

```json
{"cmd": "exec", "args": ["(() => { const s = window.__tokenicode_test; if (s && s.deleteCurrentSession) s.deleteCurrentSession(); })()"], "continueOnError": true}
```

需要在 `App.tsx` 的 `__tokenicode_test` 对象上新增 `deleteCurrentSession` 方法。

### 备选方案

每 10 轮自动 `relaunch` 重置状态（已有 relaunch 命令，但慢 ~30s）。

## 问题 2: wait-for-phase timeout 太短

`wait-for-phase writing` 的 flags.timeout 设为 30000ms。连续测试命中 API 速率限制后，模型 thinking 阶段可能超 30s。

### 修复方案

所有 `wait-for-phase` 的 timeout 从 30s 提到 60s，step timeout 对应提到 65s。影响的 suite 文件：
- `.test/suites/stdinid-race-fix/full-validation.json`（T04, T05）
- `.test/suites/interrupt-recovery/interrupt-then-send.json`（T01, T03）

## 验收标准

- [ ] 连续跑 50+ 测试后 webview 不退化（status 命令仍然响应）
- [ ] interrupt-recovery 5 轮全过
- [ ] basic-chat 5 轮全过

## 前置阅读

| 内容 | 路径 |
|------|------|
| 本轮测试结果（57 个测试） | `.test/runs/2026-04-11-post-fix/` |
| 本轮修复验证（15/15 全过） | `.test/runs/2026-04-11-stdinid-race-fix/full/run-001~003.json` |
| 昨晚全量回归测试（260 次） | `.test/runs/2026-04-11-full-issue-regression/` |
| webview 冻结详记 | `.test/runs/2026-04-11-full-issue-regression/notes-webview-freeze.md` |
| 测试指南 | `.test/README.md` |
| CLI 工具参考 | `.test/CLI-TEST-TOOL.md` |
| 测试 suite 定义 | `.test/suites/stdinid-race-fix/full-validation.json` |
