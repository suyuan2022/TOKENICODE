# Handoff — for suyuan waking up

**Date prepared**: 2026-04-08 (late night / early morning)
**Task**: `04-08-fix-cli-sdk-protocol-e2e-harness`
**Prepared by**: main Claude session during `/trellis:parallel` invocation
**Time to read everything**: ~10 minutes

---

## TL;DR (60 seconds)

> 6 个反复复发的 CLI SDK 协议 bug 已经被深度研究清楚了。**4 个具体根因**，**3 个精确修复方案**（F1+F2+F3），每个都定位到 `file:line`。另外 3 个（#49/#44/#30）等 F1+F2+F3 修完你复测后再决定要不要追查。
>
> **E2E 测试可行**。macOS 上官方 tauri-driver 完全不支持，但研究下来找到一条干净的路径：给 TOKENICODE 加一个 `test-harness` feature flag + 8 个 `__test_*` Tauri 命令 + 一个独立的 `tokenicode-test` 二进制跑 YAML 剧本。~400 行 Rust + 60 行 YAML，1-2 天做出来之后 Claude Code 可以完全无人值守地跑端到端验证。
>
> **零 blocking 问题**。两个非阻塞确认项在 `implementation-plan.md` 第 6 节。
>
> **还没有动代码**。只做了研究 + PRD + 完整的实施方案。等你说「干」。

---

## 你要读的文件（按优先级）

1. **`prd.md`**（5 分钟）— 需求、范围、验收标准、约束
2. **`implementation-plan.md` §2 "Root Cause Findings"**（3 分钟）— 4 个根因，带 `file:line`
3. **`implementation-plan.md` §3 Phase 2 "Fix F1/F2/F3"**（2 分钟）— 具体怎么改
4. **`implementation-plan.md` §6 "Open Questions"**（30 秒）— 两个非阻塞确认项

其他（`§3 Phase 0/1/3/4`, `§5 Risk Register`, `§7 Handoff Checklist`）可以之后读。

---

## 我今晚做了什么

| Step | 结果 |
|---|---|
| 拉了 6 个 issue（#57 #49 #44 #39 #30 #27）的完整评论 | 见 §7 forensic context |
| 查了 HKUDS CLI-Anything + tauri-driver 在 macOS 上的现状 | 结论：官方方案 macOS 不支持，只能走自定义 debug IPC |
| 派了后台 Research Agent（opus，运行约 12 分钟，36 次工具调用） | 返回 4000+ 字深度报告，file:line 精确到行 |
| 创建 Trellis 任务目录 `.trellis/tasks/04-08-fix-cli-sdk-protocol-e2e-harness/` | 已配 `task.json` + `prd.md` + `implementation-plan.md` |
| 创建 `.trellis/spec/backend/` 层（原本没有） | `index.md` + `testing.md` stub |
| 创建 `.trellis/spec/frontend/testing.md` stub | 更新 frontend/index.md 把它列出 |
| 配置 `implement.jsonl` / `check.jsonl` / `debug.jsonl` 上下文 | 注入 CLAUDE.md + ARCHITECTURE.md + cross-layer guide |
| 设置任务分支 `fix/cli-sdk-protocol-e2e-harness`，scope 列清所有要碰的文件 | 任务已 `start`（`.current-task` 激活） |

**我没有做的事**（你明确说过或我判断需要你确认）：
- 动源码
- `git commit` / `git push`
- 起 implement agent（你没授权之前不起）
- 起 worktree（等你决定要不要 parallel pipeline）

---

## 4 个根因 / 3 个修复（决定性摘要）

### 根因表

| Bug | 根因 | 位置 | 修复 |
|---|---|---|---|
| **#57-A** | 背景处理器缺 `case 'content_block_delta'` — 前台有（L1945-1954），背景没有 | `useStreamProcessor.ts:213-691` | **F1** |
| **#27** | 背景处理器的 `case 'result'` 不清 `pendingCommandMsgId` — 前台清（L1594-1606），背景不清 | `useStreamProcessor.ts:539-627` | **F1**（同一个 fix） |
| **#57-B** | rAF flush 在 `mappedId + selectedSessionId` 都空时静默 wipe buffer | `useStreamProcessor.ts:72-76` | **F2** |
| **#39** | Rust 侧 `lib.rs:1557-1591` emit 权限请求时剥掉 `parent_tool_use_id`；前端 L871 无条件设 `phase:awaiting` | Rust + Frontend（推荐纯前端修） | **F3** |
| **#49 #44 #30** | Her-Desktop 私有信息不够，推测被 F1+F2+F3 顺带修掉 | — | **F4**（deferred，修完复测） |

### Fix 概要

- **F1**：给 `handleBackgroundStreamMessage` 加 `content_block_delta` + `thinking_delta` case，`result` 里加 `pendingCommandMsgId` 清理。纯前端，LOW risk。**一改修两个 bug**。
- **F2**：把 rAF 的 silent wipe 改成 orphan queue（1 MB/stdinId、10 MB 总、5 s TTL，超限 ERROR 不静默）。纯前端，LOW risk。
- **F3**：推荐**纯前端变种**——加 `subAgentDepth` 字段到权限卡，子 agent 请求不触发 `phase:awaiting`，fail-safe 默认「当作主 agent 锁输入」。MEDIUM risk（误判边界需要埋点监控）。

### 统一架构不变式（要在 spec 里固化的）

> **前台和背景两个 stream 处理器必须实现相同的 case 集合，区别只是"渲染" = 直接写 store vs 写 cache。任何加到一边的 case 必须同步加到另一边，由 lint / 共享 dispatch / 测试强制。未知消息类型必须 warn 而不是静默丢弃。**

这个不变式写进 `.trellis/spec/backend/event-emission.md`（Phase 4 交付物），防止下一次同样的漂移。

---

## 你现在有三个选项

### 选项 A：让我继续跑（低风险推荐）

跟我说「继续」或「干 Phase 0+1」。我会：
1. 派 implement agent 做 Phase 0（测试基建：package.json scripts、vitest config、fake CLI fixture、Rust test 目录）
2. Agent 跑完我 review diff 汇报给你
3. 你点头后派 Phase 1 agent 写失败测试
4. 依次推进 Phase 2 / 3 / 4

**你什么都不用做**，每个阶段我给你一个段落报告。

### 选项 B：派 worktree agent（中风险，适合大改动）

```bash
python3 ./.trellis/scripts/multi_agent/start.py .trellis/tasks/04-08-fix-cli-sdk-protocol-e2e-harness
```

这会起一个 git worktree + dispatch agent 全自动跑 implement → check → finish → create-pr。**隔离在 worktree 里**，不会污染你 `suyuan` 分支的当前工作区。diff 全部集中在一个 PR 里。

**适合**：你想去忙别的，让 pipeline 自己跑完再看结果。

**风险**：pipeline 很长，中间出错不易打断；最终是一个大 PR，review 压力大。

### 选项 C：手动分阶段

按 `implementation-plan.md` §4 "Execution Model" 表格，一个 phase 一个 agent，每个 phase 你 review 一次再推进下一个。

**适合**：你对 F1/F2/F3 想先看第一个 fix 的效果再决定要不要继续。

---

## 我的推荐

**选项 A，先跑 Phase 0 + Phase 1**（基建 + 失败测试），跑完给你看 diff。

**理由**：
1. 这两个 phase 动的全是**新增文件**（test config、fake CLI、测试文件），零风险改动现有代码
2. 跑完 Phase 1 你就能亲眼看到 6 个 bug **以自动化测试的形式复现**——这比读根因分析更有说服力
3. 之后 Phase 2（F1+F2+F3）才是改现有代码，那时你再决定走 worktree 还是继续 in-place
4. 全程零 blocking 问题，Phase 0/1 你可以完全不插手

**如果你同意**：回「干 Phase 0+1」。
**如果要换方案**：直接告诉我。

---

## 非阻塞清单

这两条**不影响**开工，修完 F1+F2+F3 之后再处理：

1. **#49 #44 #30 复测**——F1+F2+F3 上线后，你手动复测三个 bug。还复现的话你把 Her-Desktop 的 commit hash 或 issue 正文给我，我再修 F4。
2. **F3 变种确认**——先按推荐的纯前端变种做。如果埋点显示 CLI 很少传 `parent_tool_use_id`，再追加一个 Rust 侧任务把 `protocol.rs:49 agent_id` 接通。

---

## 文件地图（找东西用）

```
.trellis/tasks/04-08-fix-cli-sdk-protocol-e2e-harness/
├── task.json              # Trellis 元数据，branch / scope 已配
├── prd.md                 # 需求 / 范围 / 验收标准
├── implementation-plan.md # ★ 完整实施方案（根因 + 修复 + Phase 0-4）
├── HANDOFF.md             # 这份
├── implement.jsonl        # Implement Agent 的上下文注入清单
├── check.jsonl            # Check Agent 的上下文注入清单
└── debug.jsonl            # Debug Agent 的上下文注入清单

.trellis/spec/backend/     # 新建的 backend spec 层
├── index.md               # 首页，列出后续要填的文件
└── testing.md             # stub，Phase 4 填完

.trellis/spec/frontend/
├── index.md               # 已经加了 testing.md 的入口
└── testing.md             # stub，Phase 4 填完
```

---

**晚安。**
