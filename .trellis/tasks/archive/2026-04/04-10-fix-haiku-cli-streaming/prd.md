# PRD: 修复 haiku 模型在 TOKENICODE 中流式输出不稳定

**Slug**: `fix-haiku-cli-streaming`
**Created**: 2026-04-10
**Discovered**: 机械化测试中发现（interrupt-recovery 场景）
**Owner**: suyuan
**Status**: implemented (未提交，待 haiku E2E 验证)

> **2026-04-11 修复记录**：
> - `src-tauri/src/lib.rs`: 加了 haiku 模型检测 + effort level 钳位（medium/high/max → low），因 haiku budget_tokens 上限 16384
> - `src/hooks/useStreamProcessor.ts`: stale stdinId guard 修复了 model switch 时旧 process_exit 覆盖新会话状态的竞态
> - 待验证：PRD 验收标准"连续 5 次 haiku 测试全过"
**Priority**: P2

---

## 1. Bug 描述

haiku (`claude-haiku-4-5-20251001`) 通过 TOKENICODE 发送消息后，CLI 进程大概率不产生任何 NDJSON 流式输出。前端停留在 thinking phase，assistant 消息数始终为 0。sonnet/opus 在完全相同流程下 100% 正常。

## 2. 复现步骤

```bash
TKN="node scripts/tokenicode-cli.mjs"

# 1. 启动应用
pnpm tauri dev

# 2. 创建新会话并切换到 haiku
$TKN new-session --cwd /tmp/test
$TKN switch-model claude-haiku-4-5-20251001

# 3. 发送消息
$TKN type "回复OK"
$TKN send

# 4. 等待 — 大概率 timeout
$TKN wait-until-done --timeout 60000
# → 预期: ok:false, status:timeout, phase:thinking, messageCount:1
```

复现率约 80-90%。偶尔在 relaunch 后首次调用能成功。

## 3. 已排除

- **不是前端问题**：afterState 可正常采集到 phase/messageCount，webview 本身没冻结（大部分情况）
- **不是网络问题**：同环境 sonnet/opus 正常
- **不是输出长度问题**：最短的"回复OK"也卡
- **不是中断相关**：单条消息不中断也卡

## 4. 可能方向

1. CLI 对 `--model claude-haiku-4-5-20251001` 的处理方式 — 模型 ID 是否正确？
2. `--output-format stream-json` 模式下 haiku 的兼容性 — haiku 4.5 是否支持 stream-json？
3. haiku 的 extended thinking 配置 — settingsStore 的 thinkingLevel 是否影响 haiku？haiku 是否不支持 extended thinking 但被强制开启？
4. CLI 进程启动参数检查 — 用 `ps aux | grep claude` 看实际启动命令

## 5. 连锁影响

- haiku 卡住后**偶尔**导致 webview 完全无响应（所有 JS 执行超时）
- 此时 `restart`（webview reload）可能失败，需要 `relaunch` 恢复
- 机械化测试中不能使用 haiku 作为测试模型

## 6. 测试数据

- 详细测试记录：`tests/mechanical-test-findings.md`
- 测试定义文件：`/tmp/tokenicode-test/` 目录下多个 JSON
- haiku 成功的一次：单条短消息 5.9s 完成，3 条消息（含 thinking）
- haiku 失败的记录：30-60s timeout，phase=thinking，messageCount=1

## 7. 验收标准

- haiku 模型发送消息后能正常产生流式输出（至少 90% 成功率）
- 机械化测试 `haiku-单条消息能否正常完成` 连续 5 次全部 pass
