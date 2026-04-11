# TOKENICODE 自动化测试编排指南

完整命令参考见 `CLI-TEST-TOOL.md`。

---

## ⚠️ 必读：踩坑记录

> 来自 260 次执行 / 34 轮全量 Issue 回归测试（2026-04-11）。

### 1. step timeout 会抢先杀掉 wait 命令

`wait-until-done` 的 `flags.timeout` 控制命令自身的内部等待时间，但 **step 有独立的执行时限，默认 30s**。如果 flags.timeout 设了 60s 而 step timeout 没设，命令在 30s 时就被 runner 杀掉，flags.timeout 根本跑不到。

**修复**：所有 wait 类命令（`wait-until-done`、`wait-for-phase`、`wait-for`）必须同时设 step 级 `"timeout"`，比 flags.timeout 大 5s：

```json
{"cmd": "wait-until-done", "flags": {"timeout": "60000"}, "timeout": 65000}
{"cmd": "wait-for-phase", "args": ["writing"], "flags": {"timeout": "30000"}, "timeout": 35000}
```

⚠️ 下方模板是修复前的旧版，**使用前必须按此规则补 step timeout**。

### 2. 并发 test runner 的限制

`switch-model`、`switch-provider`、`open-settings` 修改的是全局状态（settingsStore），多个 runner 会互踩。**涉及全局状态修改的测试必须串行**。

各自在独立 session 内、不改全局状态的测试可以并发（如两个 runner 各自 `new-session` 后在各自 session 里发消息）。

### 3. Webview 冻结

LLM 交互后，webview JS 引擎有概率冻住（260 次测试中 53 次触发）：
- `ping`（Rust socket 层）正常
- `status`（需 `execute_js`）超时
- `restart`（依赖 `execute_js` 做 reload）也失效
- 只有 `relaunch`（杀进程重启）能恢复

**对策**：
- 非 LLM 测试（session/UI/settings）不受影响，优先跑
- 每轮测试后检查报告，全 JS timeout → 立即 relaunch
- 分析报告时区分三类失败：**JS timeout**（基础设施）、**socket error**（app 已死）、**真实功能 bug**
- 用 `python3 .test/scripts/analyze-reports.py .test/runs/<日期目录>` 自动分类

详见 `runs/2026-04-11-full-issue-regression/notes-webview-freeze.md`。

---

## 工作流

```
1. 确认应用在运行（ping 不通则启动）:
   node scripts/tokenicode-cli.mjs ping
   # 如果返回 ok:false 或 socket not found，启动应用：
   pnpm tauri dev &
   # 等待就绪（轮询 ping 直到返回 pong）：
   while ! node scripts/tokenicode-cli.mjs ping 2>&1 | grep -q pong; do sleep 5; done
   # 如果 cargo 不在 PATH（报 "No such file or directory"）：
   source ~/.cargo/env

2. 建本次测试的时间目录:
   mkdir -p .test/runs/YYYY-MM-DD-<描述>

3. suite 定义在 .test/suites/<主题>/ 下（已有的可复用，新增的也放这里）

4. 执行:
   node scripts/run-tests.mjs .test/suites/<主题>/xxx.json \
     --report .test/runs/YYYY-MM-DD-<描述>/<主题>/run-001.json

5. 读报告:  先 meta → 再 issues → 按需读 tests[N].steps

6. 汇总分析:
   python3 .test/scripts/analyze-reports.py .test/runs/YYYY-MM-DD-<描述>

7. 写 notes:  在时间目录下写 notes-<主题>.md，记录结论和踩坑
```

---

## 目录结构

```
.test/
├── README.md                    # 本文件（测试指南入口）
├── CLI-TEST-TOOL.md             # CLI 工具完整参考
├── scripts/                     # 工具脚本
│   ├── analyze-reports.py       # 跨 suite 汇总分析
│   └── run-all-phases.sh        # 批量串行执行
├── suites/                      # 测试定义（跨时间复用，按主题分子目录）
│   ├── basic-chat/
│   │   └── send-receive.json
│   ├── interrupt-recovery/
│   │   ├── interrupt-then-send.json
│   │   └── writing-interrupt-then-send.json
│   ├── streaming-stress/
│   │   └── stream-stall.json
│   └── ...
└── runs/                        # 按时间组织的测试运行记录
    └── 2026-04-11-full-issue-regression/
        ├── REPORT.md            # 本次测试总结
        ├── notes-webview-freeze.md
        ├── notes-interrupt-recovery.md
        ├── execution*.log
        ├── basic-chat/          # 该主题的所有报告
        │   ├── run-001.json
        │   └── run-002.json
        ├── interrupt-recovery/
        └── ...
```

**每次新测试**：在 `runs/` 下建日期目录（如 `2026-04-12-provider-regression`），所有产物（报告、notes、日志）都放在里面。suite 定义在 `suites/` 里共享复用。

**notes 必须记录**：测试结论、踩过的坑、最短路径、发现的软件 bug、框架本身的 bug。发现软件 bug 同时建 trellis task（`.trellis/tasks/`）写详细 PRD。

---

## 参考

- 命令详细文档、测试定义格式、设计模式、反模式、报告结构、已知限制 → 见 `CLI-TEST-TOOL.md`
- 已有测试 suite 定义 → 见 `suites/` 目录下各主题

### 批量运行

- **串行执行用 `.test/scripts/run-all-phases.sh`**：自动 skip 已有报告，JS timeout 全挂时自动 relaunch。
- **手动跑单个 suite 多轮**：
  ```bash
  RUN=.test/runs/YYYY-MM-DD-xxx
  for i in $(seq 1 20); do
    node scripts/run-tests.mjs .test/suites/<主题>/xxx.json \
      --report "$RUN/<主题>/run-$(printf '%03d' $i).json"
  done
  ```
  报告命名统一三位零填充（`run-001.json`）。
- **每次运行用独立报告文件**：不要覆盖，保留历史用于对比。
- **3 次连续失败自动恢复**：runner 会 restart → relaunch fallback，然后继续。
- **批量完成后跑分析**：`python3 .test/scripts/analyze-reports.py .test/runs/<日期目录>`。

### 经验记录

- **测完就写 notes**：在时间目录下写 `notes-<主题>.md`，记结论、踩坑、最短路径。
- **发现软件 bug**：notes 记现象和复现条件，同时在 `.trellis/tasks/` 建 task 写详细 PRD。
- **发现框架 bug 也记**：测试框架本身的问题同样重要。
