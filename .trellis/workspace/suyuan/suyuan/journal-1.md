# Journal - suyuan (Part 1)

> AI development session journal
> Started: 2026-04-07

---

## 2026-04-10: Bug Fix PR 提交 + CLI 测试桥

### 已提交的 PR

1. **yiliqi78/TOKENICODE#77** — `fix/model-switch-thinking-strip` 分支
   - 内容：模型切换前从 JSONL 清除 thinking block，解决 400 签名校验报错
   - 4 文件，319 行（lib.rs strip 函数 + InputBar 前端清理 + 类型定义）
   - 状态：OPEN，MERGEABLE

2. **yiliqi78/TOKENICODE#78** — `fix/sdk-protocol-bugs` 分支
   - 内容：6 个 SDK 控制协议根因修复（后台 tab thinking 丢失、/compact 卡死、orphan buffer 竞态、子 agent 权限冻结主输入、文件拖拽文本泄漏、kill ordering 竞态）
   - 5 文件，157 行
   - 状态：OPEN，MERGEABLE
   - 与 #77 在 lib.rs 有小交叉，PR 描述已标注

### Git 分支状态

- `suyuan`（当前工作分支）：所有未提交改动都在这里，包含 bug fix + CLI 测试桥代码
- `fix/model-switch-thinking-strip`：PR #77 专用，基于 origin/main
- `fix/sdk-protocol-bugs`：PR #78 专用，基于 origin/main
- `fork` remote（suyuan2022/TOKENICODE）：PR 的源头，push 到这里
- `origin` remote（yiliqi78/TOKENICODE）：upstream

### CLI 测试桥（进行中）

suyuan 分支未提交改动包含完整的 CLI-to-GUI 测试桥：
- `src/App.tsx`：`window.__tokenicode_test` 对象（dev only），提供 type/send/stop/switchSession/newSession/getMessages 等操作
- 8 个组件：`data-testid` 属性（dev only）
- `scripts/tokenicode-cli.mjs`：MCP socket 客户端 CLI 工具
- `CLI-TEST-TOOL.md`：使用文档
- `tauri-plugin-mcp`：debug 构建 Unix socket 通信
- 已验证可用：new-session --cwd、restart（webview reload）、check-editor、get-messages --summary

**后续**：继续在 suyuan 分支优化测试桥功能，完成后再开新 fix 分支提 PR。

---

## 2026-04-10: 机械化测试框架验证 + 修复

### 做了什么

1. 用"中断后发送"场景实际跑了一轮机械化测试（run-tests.mjs + tokenicode-cli.mjs）
2. 发现框架本身的 5 个问题并全部修复
3. 发现 haiku 模型的 CLI 兼容性 bug

### 框架修复（已完成，在 suyuan 分支未提交）

| 修复 | 文件 |
|------|------|
| `wait-until-done` 超时返回 `ok:false`（之前返回 ok:true 导致误报 pass） | `scripts/tokenicode-cli.mjs` |
| 新增 `delay` 命令（纯延时，不依赖 socket） | `scripts/tokenicode-cli.mjs` |
| 新增 `wait-for-phase` 命令（等待特定 phase 如 writing） | `scripts/tokenicode-cli.mjs` |
| 新增步骤断言 `assert` 字段（自动验证输出字段） | `scripts/run-tests.mjs` |
| runner 恢复升级：restart 失败自动 fallback 到 relaunch | `scripts/run-tests.mjs` |
| 文档更新 | `CLI-TEST-TOOL.md`, `AI-TEST-GUIDE.md` |

### 测试结论

- **sonnet**：thinking/writing 阶段中断后重发消息，全部正常，无 bug
- **haiku**：CLI 进程大概率不产生任何输出，与前端无关（Task #1 跟踪）

### 待修复 Bug

- **haiku CLI 兼容性**：`claude-haiku-4-5-20251001` 在 `--output-format stream-json` 模式下大概率无输出。详见 `tests/mechanical-test-findings.md`

### 已删除

- `fix/cli-sdk-protocol-e2e-harness` 分支 + worktree（代码已在 suyuan 或 PR 中）

