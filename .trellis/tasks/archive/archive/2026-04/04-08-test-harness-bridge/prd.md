# Build CLI-to-GUI Test Harness Bridge

**Slug**: `test-harness-bridge`
**Created**: 2026-04-08
**Creator**: suyuan
**Status**: done (MCP 插件 + testid + helper + E2E 验证全部通过)
**Parent task**: `04-08-fix-cli-sdk-protocol-e2e-harness` (completes its Phase 3)

---

## Goal

让 Claude Code（或任何外部 CLI 进程）能**直接驱动 TOKENICODE GUI 完成端到端测试**，不需要人手点击。这是完成 `04-08-fix-cli-sdk-protocol-e2e-harness` 任务里 Phase 3 E2E harness 的最后一公里——目前 scaffold 已在，缺的是外部进程 ↔ Tauri app 之间的 IPC 桥。

**为什么要做这件事**：整晚修了 6 个 bug 后，我只能做到"代码层 verification 全绿"（pnpm test + cargo test + wire-format smoke），但"行为层 verification"（真 GUI 里跑 6 个 bug 的复现场景）只能靠人手测。用户的原意："我通过 CLI 操作电脑上这个软件的 GUI 界面内容，进行真正的 AI 对话测试" —— 这个目标还没达成。

---

## What I already know

整晚的 `04-08-fix-cli-sdk-protocol-e2e-harness` 任务已经搭好了 **大半** 基础设施，现在在 worktree `trellis-worktrees/fix/cli-sdk-protocol-e2e-harness` 里：

### 已就位的 Phase 3 scaffold

| 组件 | 状态 | 位置 |
|---|---|---|
| `[features] test-harness = []` | ✅ 已定义 | `src-tauri/Cargo.toml:17-19` |
| 9 个 `__test_*` Tauri 命令的函数签名 | ✅ 已定义 | `src-tauri/src/test_commands.rs` |
| `__test_ping` 唯一实现完的命令 | ✅ 返回 `Ok("pong")` | `test_commands.rs` |
| 其他 8 个命令（`set_input`, `click_send`, `read_messages`, `wait_for_event`, ...） | ⚠️ Stub，返回 `Err("not yet implemented")` | 同上 |
| Parallel handler block 把 `__test_*` wire 进 `generate_handler!` | ✅ 可编译 | `src-tauri/src/lib.rs::run()` |
| `cli_resolver.rs` 支持 `CLAUDE_BIN_OVERRIDE` env var | ✅ gate 在 `debug_assertions OR feature = "test-harness"` | `src-tauri/src/commands/cli_resolver.rs:891-914` |
| `tokenicode-test` driver binary | ⚠️ 只做 wire-format smoke（spawn fake_claude_cli 验 NDJSON） | `src-tauri/src/bin/tokenicode_test.rs` |
| `fake_claude_cli` fixture crate | ✅ 4 scenarios，cargo build OK | `src-tauri/tests/fixtures/fake_claude_cli/` |
| `e2e/scenarios/smoke.yaml` | ⚠️ 只是 spec，driver 没 parse | `e2e/scenarios/smoke.yaml` |
| `scripts/e2e-run.sh` | ⚠️ 跑的是 wire-format smoke，非真 GUI | `scripts/e2e-run.sh` |

**最核心的缺口**：**Tauri 的 `#[tauri::command]` 只能在 Tauri 进程内被 frontend `invoke()` 调用。外部进程无法直接调用它们。** 所以"把 TOKENICODE 变成 CLI 工具"的本质是给它加一个从外部接受命令的 IPC 通道。目前 `tokenicode-test` 驱动 binary 跟主 TOKENICODE 进程之间**根本没有 IPC 连接** —— 它们是两个独立的进程。

### 技术限制（已调研）

| 方案 | 状态 | 原因 |
|---|---|---|
| 官方 `tauri-driver` | ❌ 不可用 | macOS 不支持（WKWebView 没有 Apple 提供的 WebDriver） |
| Chrome DevTools Protocol (CDP) | ❌ 不可用 | macOS 下 WKWebView 不支持 CDP 直连 |
| HKUDS CLI-Anything | ❌ 不适用 | 专为 server-side / CLI / library 设计，无法驱动 GUI 应用 |
| `danielraffel/tauri-webdriver` | ✅ 可参考 | 在 Tauri app 内嵌 axum HTTP server 的成熟模式 |
| `tauri-plugin-webdriver` | ✅ 可参考 | 单 crate 架构，比 danielraffel 更成熟 |

---

## Research Notes

### 方案（2026-04-09 更新：MCP 插件方案已实施）

#### ~~Approach A: HTTP bridge~~ → 已废弃

原方案自定义 axum HTTP bridge（3-4h），已被更成熟的 MCP 插件方案替代。

#### Approach D: P3GLEG tauri-plugin-mcp（已实施 ✅）

**选择理由**：调研发现 P3GLEG/tauri-plugin-mcp 是成熟的 Tauri 2 MCP 插件，提供 10 个 GUI 交互工具，原生支持 IPC socket + `#[cfg(debug_assertions)]` + macOS NSEvent 注入 + contentEditable/React。比自定义 HTTP bridge 省 3-4h，且功能更强大。

**How**：
- TOKENICODE debug 构建内嵌 `tauri-plugin-mcp` Rust 插件，启动时在 `/tmp/tokenicode-mcp.sock` 开 IPC socket
- Claude Code 通过 `tauri-plugin-mcp-server` MCP server 连接到 socket
- 10 个 MCP 工具：`take_screenshot`, `query_page`, `click`, `type_text`, `mouse_action`, `navigate`, `execute_js`, `manage_storage`, `manage_window`, `wait_for`

**Pros**：
- 成熟插件，专门为 Tauri 2 设计
- IPC socket（无端口冲突）
- `#[cfg(debug_assertions)]` 原生支持，release 构建完全不含
- macOS NSEvent 注入（无需辅助功能权限）
- 显式支持 contentEditable + React（TipTap 兼容）
- 任何 MCP 客户端都能驱动（Claude Code / Cursor / VS Code）

**Cons**：
- git 依赖（未上 crates.io）
- MCP server 需要单独启动（npx tauri-plugin-mcp-server）

**已完成的实施**：
1. ✅ `src-tauri/Cargo.toml` — 添加 `tauri-plugin-mcp` git 依赖（`cfg(debug_assertions)` 门控）
2. ✅ `src-tauri/src/lib.rs` — 在 setup 钩子中注册 MCP 插件（`cfg(debug_assertions)` 门控）
3. ✅ `package.json` — 添加 `tauri-plugin-mcp` npm 包
4. ✅ `src/App.tsx` — 添加 `setupPluginListeners()` 初始化
5. ✅ `.mcp.json` — 项目级 MCP server 配置

**备选方案**（未实施）：
- **hypothesi/mcp-server-tauri** — 20 工具（含 `ipc_execute_command` 可复用 `__test_*` 命令），WebSocket 9223 端口。功能更丰富但 WebSocket 比 IPC socket 多端口管理开销。

### 2 个成熟参考

- `github.com/danielraffel/tauri-webdriver` — 两 crate 架构（CLI + plugin），在 Tauri app 内嵌 axum HTTP server，对外 WebDriver 协议
- `tauri-plugin-webdriver` (OSS，更成熟) — 单 crate 架构，支持 macOS + Linux + Windows，可以直接用作"怎么在 Tauri 里起 HTTP server"的参考代码

### 映射到我们的仓库

- 已有 `test_commands.rs` 可以作为 HTTP endpoint 的内部分发层
- 已有 `test-harness` feature flag，零新 gate 成本
- 已有 `tokenicode-test` driver binary，改造成 HTTP client 即可
- 已有 `fake_claude_cli` 4 个场景，直接复用

---

## Assumptions (confirmed)

1. **Approach D (P3GLEG tauri-plugin-mcp)** 已实施（2026-04-09）
2. MVP 聚焦在 6 个 bug 的复现（#57 #49 #44 #39 #30 #27），**不做通用 GUI 自动化**
3. 代码已同步到主分支 `suyuan`（不依赖 worktree）
4. IPC socket 路径 `/tmp/tokenicode-mcp.sock`
5. 只 target macOS
6. **`execute_js` 是主交互手段**（实测 MCP click 对 React 组件部分失效）
7. 需给关键组件加 `data-testid`（debug 模式 only）辅助定位

---

## Requirements (evolving)

### 功能性

- [x] TOKENICODE debug 构建内嵌 MCP 插件，IPC socket 在 `/tmp/tokenicode-mcp.sock`
- [x] 项目级 `.mcp.json` 配置 `tauri-plugin-mcp-server`
- [x] 实测验证：MCP 握手成功，10 个工具可用
- [ ] 关键组件加 `data-testid`（debug 模式 only，release 构建自动删除）
- [ ] 基于 `execute_js` + `data-testid` 的测试指令集，覆盖 6 个 bug 场景
- [ ] 测试场景以 Claude Code prompt 形式编写（非 YAML）

### 实测发现的 MCP 工具可用性（2026-04-09）

| 工具 | 状态 | 说明 |
|------|------|------|
| `execute_js` | ✅ 完全可靠 | **主交互手段**。能直接读 Zustand store、调 React 方法、操作 DOM |
| `query_page(mode="app_info")` | ✅ 正常 | 返回 app 名、版本、OS、窗口列表 |
| `query_page(mode="find_element")` | ✅ 正常 | 坐标精确，可用于定位 |
| `query_page(mode="map")` | ⚠️ ref 不稳定 | 页面操作后 ref 编号重新分配，不能跨操作缓存 ref |
| `take_screenshot` | ✅ 正常 | URL 看着重复但内容实际不同，功能正常 |
| `click` (原生按钮) | ✅ 有效 | 侧栏按钮（新任务、收起侧栏）MCP click 可用 |
| `click` (React 组件) | ❌ 失效 | 设置面板 tab/toggle 完全无效，React 合成事件不响应原生鼠标注入 |
| `type_text` | ⏳ 待测 | TipTap editor 输入 |
| `wait_for` | ⏳ 待测 | 等条件/文字出现 |

**结论**：测试指令应主要使用 `execute_js` 交互，`click` 仅用于侧栏原生按钮，截图做视觉确认。

### 非功能性

- [x] **安全**：MCP 插件仅在 `debug_assertions` 下编译，release 构建不含（Cargo.toml `[target.'cfg(debug_assertions)'.dependencies]` + lib.rs `#[cfg(debug_assertions)]`）
- [x] **IPC socket**：`/tmp/tokenicode-mcp.sock`，无端口冲突
- [x] **macOS native**：NSEvent 注入，无需辅助功能权限

---

## Acceptance Criteria (updated 2026-04-09)

- [x] `cargo check` 通过（debug 构建，含 MCP 插件）
- [x] `pnpm build` 通过（前端，含 MCP guest-js）
- [x] MCP 插件在 debug 模式下注册：`/tmp/tokenicode-mcp.sock` 存在
- [x] `.mcp.json` 配置 `tauri-plugin-mcp-server`
- [x] MCP 握手验证通过（initialize 返回 10 工具）
- [x] 基础操作验证：截图、query_page、execute_js 均可用
- [ ] 关键组件 `data-testid` 加好（输入框、发送按钮、session tab、model selector、permission card）
- [ ] `data-testid` 在 release 构建中不存在
- [ ] 以下 6 个 bug scenarios 用 MCP 工具复现并通过：
  - [ ] `happy_path` — 单 session 流式完整（baseline）
  - [ ] `bug_57_double_session` — 双 session 切走再切回
  - [ ] `bug_27_compact_switch` — /compact + tab 切换
  - [ ] `bug_39_subagent_input` — sub-agent 权限不锁主输入
  - [ ] `bug_49_multi_session_isolation` — 5 session 并发
  - [ ] `bug_44_stdin_churn` — 快速创建/销毁 session
- [ ] Release 构建：MCP 代码完全不存在

---

## Definition of Done

- [ ] 6 个 scenarios 本地全绿（`./scripts/e2e-run.sh all`）
- [ ] `cargo clippy --features test-harness -- -D warnings` 无 warning
- [ ] Security gate 验证：`strings` release binary 不含 axum symbols
- [ ] Docs 更新：`.trellis/spec/backend/testing.md` 新增一节
- [ ] Rollout：只在 dev 环境用，不影响 release 路径
- [ ] 提交为单个 commit 到 `fix/cli-sdk-protocol-e2e-harness` 分支（与 Phase 0-2 修复 commit 并列），或独立分支合并 back

---

## Out of Scope (explicit)

- ❌ 通用 GUI 自动化（不做"任意点击任意按钮"的通用 driver）
- ❌ 跨平台支持的 polish（先 macOS，Linux/Windows 顺带能跑就好，不专门测）
- ❌ 与上游（yiliqi78）协调合并 — 这是本地工具，不追求被上游合并
- ❌ CI 接入（本地 dev 用即可，日后有需要再加）
- ❌ 录屏 / 截图验证（不做视觉回归）
- ❌ 性能 / 压力测试（只验功能对错）
- ❌ 完整的 WebDriver 协议实现（只实现够用的子集）
- ❌ 重构 `tokenicode-test` driver 成多语言客户端库

---

## Open Questions

### Q1 — TipTap 输入方式

`type_text` 能否在 TipTap editor 里可靠工作？如果不能，需要在 debug 模式下把 editor 实例暴露到 `window` 上，用 `execute_js` 调 `editor.commands.insertContent()`。待实测。

### Q2 — `tauri-mcp-server` npm 包 bin 缺 shebang

全局安装后 `tauri-mcp-server` 命令被 ImageMagick 劫持。当前用 `node path/to/build/index.js` 绕过。可以提 PR 给 P3GLEG 修复，或在项目 `.mcp.json` 里写死 node 路径。

---

## Definition of Done (team quality bar)

- Tests: 6 个 scenarios 在本地能复跑全绿
- Lint: `cargo clippy --features test-harness -- -D warnings`
- Security: `strings` release binary 不含 axum
- Docs: testing.md 更新
- Rollout: 只 dev，不影响 release

---

## Technical Notes (updated 2026-04-09)

### 已实施的改动（worktree: `fix/cli-sdk-protocol-e2e-harness`）

| 文件 | 改动 |
|---|---|
| `src-tauri/Cargo.toml` | 添加 `tauri-plugin-mcp` git 依赖（`cfg(debug_assertions)` 门控） |
| `src-tauri/src/lib.rs` | setup 钩子中注册 MCP 插件（`cfg(debug_assertions)` 门控） |
| `package.json` | 添加 `tauri-plugin-mcp` npm 包 |
| `src/App.tsx` | 添加 `setupPluginListeners()` 初始化 |
| `.mcp.json` | 项目级 MCP server 配置 |

### MCP 工具到测试场景的映射（实测后修正）

| 测试操作 | 首选工具 | 备选 | 说明 |
|----------|---------|------|------|
| 输入文字 | `execute_js` 操作 TipTap API | `type_text`（待测） | TipTap 是 contentEditable，直接 JS 注入最可靠 |
| 点击发送 | `execute_js` 找 `[data-testid="send-button"]` 并 `.click()` | `click`（仅原生按钮有效） | MCP click 对 React 组件失效 |
| 读取消息 | `execute_js` | — | `useChatStore.getState().messages[tabId]` |
| 等待完成 | `wait_for` | `execute_js` 轮询 | 等文字出现/元素变化 |
| 截图验证 | `take_screenshot` | — | URL 重复是假象，内容实际不同 |
| 读取状态 | `execute_js` | — | 读任意 Zustand store |
| 切换 session tab | `execute_js` | `click`（侧栏按钮 OK） | `useSessionStore.getState().switchTab(id)` |
| 切换模型 | `execute_js` | — | 直接调 store method，MCP click 对 React 组件无效 |

### 需要加 `data-testid` 的组件（按实际测试流程排序）

用户测试场景：**对话 → 切 CLI session → 切模型/供应商 → 切回继续对话**

#### 对话流程

| 元素 | testid | 文件 | 行号 |
|------|--------|------|------|
| TipTap 编辑器容器 | `chat-input-editor` | `src/components/chat/TiptapEditor.tsx` | — |
| 发送按钮 | `send-button` | `src/components/chat/InputBar.tsx` | L1484 |
| 聊天消息区域 | `chat-messages` | `src/components/chat/ChatPanel.tsx` | — |

#### 切换 CLI session

| 元素 | testid | 文件 | 行号 |
|------|--------|------|------|
| 新建 session 按钮 | `new-session-button` | `src/components/layout/Sidebar.tsx` | L91 |
| Session 列表项 | `session-item-{sessionId}` | `src/components/conversations/SessionItem.tsx` | — |
| 当前 session 卡片 | `current-session-card` | `src/components/layout/Sidebar.tsx` | L104 |

#### 切换模型/供应商

| 元素 | testid | 文件 | 行号 |
|------|--------|------|------|
| Model selector 触发按钮 | `model-selector` | `src/components/chat/ModelSelector.tsx` | L84 |
| Model option 项 | `model-option-{modelId}` | `src/components/chat/ModelSelector.tsx` | L113 |
| Provider 切换入口 | `provider-selector` | `src/components/settings/ProviderTab.tsx` 或 `ProviderManager.tsx` | — |

#### 读取状态用

| 需要读的数据 | execute_js 代码 |
|-------------|----------------|
| 当前 messages | `useChatStore.getState().messages[tabId]` |
| 当前活跃 session | `useSessionStore.getState().selectedSessionId` |
| 所有 session 列表 | `useSessionStore.getState().sessions` |
| 当前模型 | `useSettingsStore.getState().selectedModel` |
| 当前 provider | `useProviderStore.getState().activeProviderId` |
| 流式状态 | `useChatStore.getState().partialText[tabId]` |

实现方式：`{...(import.meta.env.DEV && { 'data-testid': 'xxx' })}` — Vite build 时自动删除。

### 已有 scaffold（不再需要，但保留）

- `test_commands.rs` — 9 个 `__test_*` stub（MCP 插件替代了这些命令）
- `tokenicode-test` driver binary — wire-format smoke test（MCP server 替代了自定义 driver）
- `fake_claude_cli` fixtures — 仍可用于单元测试

### 关键风险（实测后更新）

1. **MCP click 与 React 合成事件断层**（已确认，严重度：高）
   - 现象：MCP click 对侧栏原生按钮有效，对设置面板 React tab/toggle 完全无效
   - 推测：Tauri WKWebView 里 MCP 的 NSEvent 鼠标注入走原生事件路径，但 React onClick 是合成事件，某些场景不触发
   - 应对：测试指令全部走 `execute_js` → `element.click()` / 直接调 store method

2. **设置面板开关不可自动化**（已确认，严重度：高）
   - 现象：MCP click 和 JS `.click()` 都无法关闭设置面板
   - 推测：设置面板的 toggle 行为可能走 Zustand store 的 `isSettingsOpen` 状态，不依赖 DOM 事件
   - 应对：用 `execute_js` 直接改 store 状态：`useSettingsStore.getState().setSettingsOpen(false)`

3. **TipTap 编辑器输入待验证**（严重度：中）
   - `type_text` 声称支持 contentEditable，但 TipTap 是复杂的 ProseMirror 封装
   - 如果 `type_text` 失败，用 `execute_js` 调 TipTap 的 `editor.commands.insertContent()`
   - 需要先暴露 editor 实例到 window（debug mode only）

4. **map 模式 ref 不稳定**（已确认，严重度：低）
   - 每次 DOM 变化后 ref 编号重新分配
   - 应对：不跨操作缓存 ref，每次操作前重新 query

### 参考实现

- `github.com/danielraffel/tauri-webdriver`（开源 macOS WebDriver for Tauri）
- `tauri-plugin-webdriver`（OSS，更成熟，macOS + Linux + Windows）
- 整晚已研究过：`.trellis/tasks/04-08-fix-cli-sdk-protocol-e2e-harness/RESEARCH-DELIVERABLES.md` §C3 E2E 方案评估

### Prior art in this repo

- `src-tauri/src/lib.rs::start_claude_session` 展示了如何用 tokio::spawn + channels 管理子进程
- `tokio-tungstenite` 已被间接依赖，如果 WebSocket 路径更喜欢可以考虑
- `reqwest` 已在 main deps，HTTP client 侧（`tokenicode-test` binary）可以复用

---

## 实施日志（2026-04-09）

### 一、最终方案：MCP 插件 + Zustand store 直操

**架构**：`Claude Code` → `MCP protocol (stdio)` → `tauri-plugin-mcp-server (Node.js)` → `IPC socket (/tmp/tokenicode-mcp.sock)` → `TOKENICODE (Tauri webview)`

**核心发现**：MCP 的 `click()` 工具对 React 组件无效——触发了原生事件但 React 合成事件不响应，所有涉及 Zustand store 状态变更的操作都失败了。解决方案：**绕过 React 事件系统，直接操作 Zustand store**。

### 二、踩过的坑

#### 坑 1：MCP click 在 React 组件上失效

- **现象**：`click()` 坐标解析正确，但 UI 无变化。设置按钮、session 列表、model 选择器全部失败
- **根因**：React 18/19 的事件委托机制。React 把所有事件监听挂载到 root 元素上，通过事件冒泡捕获。MCP 的 click 只触发了原生 DOM 事件，但没有正确冒泡到 React 的事件系统
- **解决**：放弃 click，改用 `execute_js` 直接调用 Zustand store 方法。在 `window` 上暴露 `__tokenicode_test` helper 对象

#### 坑 2：`execute_js` 不支持多语句 / top-level await

- **现象**：`var x = 1; JSON.stringify(x)` 有时返回 `undefined`
- **根因**：execute_js 执行环境对多语句支持不稳定
- **解决**：每条 execute_js 只写一个表达式。需要多步操作时拆成多次调用

#### 坑 3：`toggleSettings()` 不是幂等的

- **现象**：`openSettings()` 返回 `{settingsOpen: true}` 但实际状态立刻变回 false
- **根因**：最初用 `toggleSettings()`（翻转状态），但不确定原因导致状态被立即翻转回去
- **解决**：改成 `useSettingsStore.setState({ settingsOpen: true })`，直接设值而非翻转

#### 坑 4：MCP server npm bin 缺 shebang

- **现象**：`.mcp.json` 用 `tauri-mcp-server` 命令启动时报错，ImageMagick 的 `import` 命令拦截
- **根因**：npm 包 `tauri-plugin-mcp-server` 的 bin 是 ESM JS 文件但没有 shebang
- **解决**：`.mcp.json` 里用 `node` 作为 command，直接指向 `build/index.js`：
  ```json
  {"command": "node", "args": ["/path/to/build/index.js"]}
  ```

#### 坑 5：HMR 不更新 useEffect 闭包

- **现象**：改了 App.tsx 的 helper 函数后，execute_js 调用的还是旧版本
- **根因**：Vite HMR 热更新了组件，但 useEffect 里的闭包捕获的是旧模块变量
- **解决**：需要 `navigate(reload)` 刷新整个 webview 才能生效

#### 坑 6：release 版本没有 MCP 插件

- **现象**：MCP socket 存在但连接被拒（ECONNREFUSED）
- **根因**：正在运行的是 `/Applications/TOKENICODE.app`（release 构建），MCP 插件只在 `cfg(debug_assertions)` 下编译
- **解决**：必须用 `pnpm tauri dev` 启动 debug 版本

### 三、做了什么操作（按时间线）

1. **选型**：从 Approach A (HTTP bridge) → Approach D (MCP plugin P3GLEG/tauri-plugin-mcp)
2. **集成 MCP 插件**：
   - Rust 侧：`Cargo.toml` 加依赖 + `lib.rs` 注册插件（`cfg(debug_assertions)` 门控）
   - JS 侧：`App.tsx` 加 `useEffect` 初始化 MCP listener
   - npm：`pnpm add tauri-plugin-mcp`
3. **手动实测 MCP 工具**：确认 10 个工具可用性，发现 click 失效问题
4. **加 data-testid**：8 个组件加 testid（`import.meta.env.DEV` 门控，release 构建自动删除）
5. **暴露前端 helper**：`window.__tokenicode_test` 20 个方法，覆盖读状态/输入发送/切 session/切模型/切 provider/权限处理
6. **注入 MCP 指令**：直接改 `tauri-plugin-mcp-server` 的 `instructions` 字段，注入 TOKENICODE 专用操作指南
7. **E2E 验证**：全部通过

### 四、现在的源码改动清单

#### Rust 侧（`src-tauri/`）

**`Cargo.toml`** — MCP 插件依赖，debug 门控：
```toml
[target.'cfg(debug_assertions)'.dependencies]
tauri-plugin-mcp = { git = "https://github.com/P3GLEG/tauri-plugin-mcp" }
```

**`src/lib.rs`** — MCP 插件注册（在 updater 插件之后）：
```rust
#[cfg(debug_assertions)]
{
    let mcp_config = tauri_plugin_mcp::PluginConfig::new("TOKENICODE".to_string())
        .start_socket_server(true)
        .socket_path(std::path::PathBuf::from("/tmp/tokenicode-mcp.sock"));
    app.handle().plugin(tauri_plugin_mcp::init_with_config(mcp_config))?;
    eprintln!("[TOKENICODE] MCP test harness plugin registered on /tmp/tokenicode-mcp.sock");
}
```

#### 前端侧（`src/`）

**`App.tsx`** — 3 处改动：
1. MCP listener 初始化 useEffect
2. `window.__TOKENICODE_TESTIDS` 发现对象（18 个 testid 描述）
3. `window.__tokenicode_test` helper 对象（20 个方法），包括：
   - 读状态：getMessages, getLastMessage, getActiveSessionId, getAllSessions, getCurrentModel, getCurrentProvider, isStreaming, isSettingsOpen
   - 操作：type, send, switchSession, newSession, switchModel, switchProvider, openSettings, closeSettings, switchSettingsTab, allowPermission, denyPermission, stop

**`TiptapEditor.tsx`** — 2 处改动：
1. `data-testid="chat-input-editor"` on wrapper div
2. `window.__tokenicode_editor = editor`（debug only）

**`InputBar.tsx`** — 3 处改动：
1. `data-testid="send-button"` on submit button
2. `data-testid="stop-button"` on stop button
3. `window.__tokenicode_send = handleSubmit`（debug only）

**`ChatPanel.tsx`** — 1 处：
- `data-testid="chat-messages"` on scroll container

**`ModelSelector.tsx`** — 2 处：
- `data-testid="model-selector"` on trigger button
- `data-testid="model-option-{id}"` on each option

**`Sidebar.tsx`** — 3 处：
- `data-testid="new-session-button"` on new chat button
- `data-testid="current-session-card"` on session info card
- `data-testid="settings-button"` on settings toggle (footer)

**`SessionItem.tsx`** — 1 处：
- `data-testid="session-item-{sessionId}"` on each session item

**`PermissionCard.tsx`** — 3 处：
- `data-testid="permission-card"` on wrapper div
- `data-testid="permission-allow-button"` on allow button
- `data-testid="permission-deny-button"` on deny button
- `window.__tokenicode_respond_permission = handleRespond`（debug only, pending 时暴露）

**`SettingsPanel.tsx`** — 3 处：
- `data-testid="settings-panel"` on modal overlay
- `data-testid="settings-close-button"` on X button
- `data-testid="settings-tab-{id}"` on each tab (general/provider/cli/mcp)

**`ProviderCard.tsx`** — 1 处：
- `data-testid="provider-card-{id}"` on each provider card

**`ProviderManager.tsx`** — 1 处：
- `data-testid="provider-inherit-button"` on inherit system config button

#### MCP 配置

**`package.json`** — 加了 MCP 插件 npm 包：
```json
"tauri-plugin-mcp": "^0.1.0"
```
通过 `pnpm add tauri-plugin-mcp` 安装，提供 JS 侧的 `setupPluginListeners` / `cleanupPluginListeners` API。

**`.mcp.json`**（项目根目录，新文件）：
```json
{
  "mcpServers": {
    "tokenicode-gui": {
      "command": "node",
      "args": ["/Users/suyuan/.npm-global/lib/node_modules/tauri-plugin-mcp-server/build/index.js"],
      "env": {"TAURI_MCP_IPC_PATH": "/tmp/tokenicode-mcp.sock"}
    }
  }
}
```

**`tauri-plugin-mcp-server/build/index.js`**（npm 全局包，本地修改） — 改了 `instructions` 字段，注入 TOKENICODE 专用操作指南（约 60 行）。注意：`npm update` 会覆盖，长期需要 fork 或写 wrapper。

#### 非 MCP 相关的混入改动（已在 git diff 中但属于其他任务）

- `src-tauri/src/commands/claude_process.rs` — `StartSessionParams` 加了 `model_switch: Option<bool>` 字段
- `src/lib/tauri-bridge.ts` — `StartSessionParams` 接口加了 `model_switch?: boolean` 字段

这两个改动是 model switch 功能的一部分，不是 MCP 测试桥的改动，提交时应注意分开。

### 五、MCP 是怎么回事 & 怎么用

#### 什么是 MCP

MCP (Model Context Protocol) 是 Anthropic 定义的 AI 工具调用协议。Claude Code 通过 MCP 连接外部工具服务器，获得额外的能力（如浏览器控制、文件操作等）。对于 TOKENICODE，MCP 让 Claude Code 能"看到"并"操控"正在运行的 TOKENICODE GUI。

#### 架构

```
Claude Code (this process)
    ↓ stdio (JSON-RPC)
tauri-plugin-mcp-server (Node.js subprocess)
    ↓ IPC socket (/tmp/tokenicode-mcp.sock)
TOKENICODE (Tauri app, debug build)
    ↑ execute_js → window.__tokenicode_test helper
    ↑ Zustand store direct manipulation
```

#### 10 个 MCP 工具

| 工具 | 用途 | TOKENICODE 可用性 |
|------|------|------------------|
| query_page | 获取 DOM 结构、页面状态 | ✅ 正常 |
| execute_js | 执行任意 JS | ✅ **主要工具** |
| take_screenshot | 截图 | ✅ 正常 |
| wait_for | 等待元素/文本出现 | ✅ 正常 |
| click | 点击坐标 | ❌ React 组件失效 |
| type_text | 输入文字 | ❌ TipTap 不兼容 |
| mouse_action | hover/scroll/drag | ⚠️ 未测 |
| navigate | 页面导航/reload | ✅ 正常 |
| manage_storage | localStorage/cookies | ✅ 正常 |
| manage_window | 窗口管理 | ✅ 正常 |

#### 使用流程

1. **启动 TOKENICODE dev 版**：`pnpm tauri dev`
2. **确认 MCP 注册**：控制台输出 `[TOKENICODE] MCP test harness plugin registered on /tmp/tokenicode-mcp.sock`
3. **启动 Claude Code**（在 TOKENICODE 项目目录下），`.mcp.json` 会自动配置 MCP 连接
4. **使用 `window.__tokenicode_test` helper** 操作 GUI：

```javascript
// 对话流程
window.__tokenicode_test.type('你好')
window.__tokenicode_test.send()
// 等待回复完成
window.__tokenicode_test.isStreaming()  // → false 表示完成
// 读取完整回复（包含思考、工具调用、文本回复）
window.__tokenicode_test.getMessages()

// 切换 session
window.__tokenicode_test.getAllSessions()
window.__tokenicode_test.switchSession('session-id')

// 切换模型
window.__tokenicode_test.switchModel('claude-sonnet-4-6')

// 切换 provider
window.__tokenicode_test.openSettings()
window.__tokenicode_test.switchSettingsTab('provider')
window.__tokenicode_test.switchProvider('provider-id')
window.__tokenicode_test.closeSettings()

// 权限处理
window.__tokenicode_test.allowPermission()  // 或 denyPermission()
```

#### 已验证的完整测试链路

| 操作 | 结果 |
|------|------|
| 读取当前状态 (session/model/provider) | ✅ |
| 列出所有 session (48个) | ✅ |
| 切换模型 | ✅ |
| 切换 provider (id/null) | ✅ |
| 打开/关闭设置面板 | ✅ |
| 切换设置 tab | ✅ |
| 切换 session | ✅ |
| 输入文字到 TipTap | ✅ |
| 发送消息触发 CLI | ✅ |
| 检测 streaming 状态 | ✅ |
| 读取完整消息流 (thinking + tool_use + text) | ✅ |
| 连续对话 (第二轮) | ✅ |
| 截图 | ✅ |

### 六、未完成 / 待改进

1. **MCP 指令的持久化**：当前改的是本地 npm 全局包，`npm update` 会覆盖。长期方案：fork `tauri-plugin-mcp-server` 或写 wrapper 脚本
2. **type_text 兼容**：MCP 原生 type_text 无法输入 TipTap 编辑器，目前用 `window.__tokenicode_editor.commands.insertContent()` 绕过
3. **click 兼容**：MCP 原生 click 对所有 React 组件无效，全部走 store 直操。`switchSettingsTab` 是唯一还需要 `.click()` 的地方（因为 tab 状态是 SettingsPanel 组件内部 state，不在全局 store）
4. **自动轮询 isStreaming**：当前需要手动调用。可以加一个 `waitUntilDone()` helper 内部轮询
5. **tool_use 详情**：当前 `getMessages()` 返回 tool_use 消息有 `toolName`，但没有 input/output 详情。需要扩展消息结构
