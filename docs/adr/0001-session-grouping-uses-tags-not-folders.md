# ADR-0001：会话分组用 tag（元数据映射），不用真实子文件夹

- **状态**：已接受
- **日期**：2026-06-03
- **相关**：`CONTEXT.md`「组 / 工作区」；`06_DEV/HANDOFF.md`

## 背景

用户（非程序员）要在左侧给会话做二级分组（「组」），收纳同一工作区里混在一起的会话。曾认真评估「组 = 真实子目录（子工作区）」方案，理由是真文件夹能继承根目录 `CLAUDE.md`、原生可在 Finder 浏览 / 带走、并复用工作区卡片 UI。

## 关键约束（三处代码实证）

Claude Code 的会话存储是**两层固定结构** `~/.claude/projects/<cwd 编码>/<uuid>.jsonl`，没有任何一处递归进子目录：

1. `src-tauri/src/lib.rs` `list_sessions`（~2891）：`read_dir(claude_dir)` → 对每个一层子目录再 `read_dir`，只收 `.jsonl` 文件；子文件夹里的 jsonl 不会进列表。
2. `src-tauri/src/lib.rs` `find_session_jsonl`（~1413）：按 UUID 定位时只在每个一层目录下 `join("<uuid>.jsonl")`，不进子目录 → resume 前置定位失败。
3. spawn Claude CLI 用 `current_dir(&params.cwd)`（~1959/2011）：会话归属由 cwd 决定，CLI 按 cwd 编码读写**那一层**目录。

**推论**：把已有会话挪进子文件夹 → ① 从列表消失 ② resume 接不上 ③ CLI 续写按 cwd 写回原层、文件分裂。

## 决策

「组」用**元数据映射**实现（tag 逻辑）：会话 `.jsonl` **永不移动**，分组归属记在 TOKENICODE 自有数据文件 `~/.tokenicode/groups.json`（与 `pinned.json` / `archived.json` 并列）。左侧按映射把会话渲染成「工作区 › 组 › 会话」三级。

## 结果

- 现有老会话可随意归类、自由拖拽换组（只改映射、不碰文件），resume 永不受影响 —— 正是用户最核心的痛点。
- 组不改变 cwd，组里会话照常继承所在工作区的 `CLAUDE.md` 等上下文（用户要的「继承」白送）。
- **代价**：组本身没有独立的目录级上下文（做不到每组一份特化 `CLAUDE.md`）；用户已确认不需要这层。
- 「在 Finder 浏览 / 带走」改由**按需导出**满足：把某组会话复制成 Finder 真文件夹副本（可读文件名 + markdown），原文件不动。

## 被否决的替代

**真实子文件夹 / 子工作区**：工作区下建真子目录、会话在子目录里新建（cwd = 子目录）。能继承上下文、原生 Finder 可达。否决原因：① 对**已有老会话**无法归类（挪文件即坏 resume），而消化存量混乱正是首要目标；② 会话不能自由拖拽换组（换组 = 改 cwd = 挪文件）；③ 与「工作区一级」的扁平 `projects` 机制错位（子目录其实是另一个独立 project）。
