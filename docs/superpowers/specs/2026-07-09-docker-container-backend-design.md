# 设计文档：Docker 容器后端（连接已有容器打开目录 + 容器内运行 claude CLI）

日期：2026-07-09
状态：已与需求方对齐，待实施

## 1. 背景与目标

TOKENICODE 目前只能打开本机文件夹，并在本机启动 `claude` CLI 子进程。目标：支持
**连接一个已经在运行的本机 Docker 容器**，打开容器内的目录作为项目，claude CLI 在
容器内执行；文件树、预览、编辑、自动刷新等体验与本机项目完全一致。

### 需求决策（已确认）

| 决策点 | 结论 |
|---|---|
| 容器来源 | 连接已存在、正在运行的容器；Docker daemon 在本机 |
| claude CLI | 容器内由用户预先装好；TOKENICODE 只探测与调用，不负责安装 |
| 登录凭据 | 不复用本机；用户在容器内自行 `claude login`，容器持有自己的 `~/.claude` |
| 目录前提 | 打开的容器内目录**总是**位于某个 bind mount（挂载自本机）之下 |
| 文件体验 | 对齐本机完整体验：树、预览、编辑、增删改、自动刷新 |
| 路径空间 | 前端 UI 与 claude 之间流通的路径**一律是容器内路径** |
| 聊天记录 | 必须显示回本机 TOKENICODE（无论容器 `~/.claude` 是否被挂载） |
| 模式共存 | 按项目切换：每个项目记住自己是本机后端还是某容器后端 |

### 明确不做（YAGNI）

- 远程 Docker 主机（DOCKER_HOST / SSH）
- TOKENICODE 代管容器生命周期（创建/删除/构建镜像）
- 非挂载目录的支持（纯容器内部路径直接拒绝）
- 容器内文件 watcher（不需要——监听走本机侧）

## 2. 方案选型

**采用：路径映射 + docker exec。** 依据"目录总是挂载自本机"的前提，同一份文件有
两个地址（本机路径 / 容器内路径）。文件相关功能通过映射走本机现有 `std::fs` /
`notify` 代码（零改动、零性能损失、自动刷新免费）；仅 CLI 进程通过 `docker exec`
进入容器执行。`docker exec` 本身仍是用 `tokio::process::Command` 启动的本机进程，
`Child` / stdin / stdout 管道类型不变，因此 ProcessManager、StdinManager 及整套
NDJSON 流处理无需修改。

否决的备选：
- **纯 docker exec 文件后端**：全部文件操作走 `docker exec ls/cat`，需容器内 watcher
  回传。通用但工作量约为本方案 5 倍，且本场景用不到其通用性。
- **bollard crate（Docker API 直连）**：exec attach 的流模型与现有 `Child` 管道不
  兼容，需重构 ProcessManager；引入大依赖，不值得。

## 3. 核心架构

### 3.1 数据模型：WorkspaceBackend

每个项目携带后端描述，持久化于 settingsStore（前端）与最近项目记录：

```ts
type WorkspaceBackend =
  | { kind: 'local' }
  | { kind: 'docker'; container: string;   // 容器名或 ID
      containerCwd: string }               // 容器内项目路径
```

本机项目行为与现状完全一致（`kind: 'local'` 为默认，向后兼容旧数据：无 backend
字段视为 local）。

### 3.2 Rust 新模块：`src-tauri/src/docker_backend.rs`

三个职责：

1. **容器发现**
   - `list_docker_containers()`：执行 `docker ps --format '{{json .}}'`，返回
     运行中容器（名称、镜像、ID）。
   - `inspect_container(name)`：执行 `docker inspect`，返回 Mounts 表
     （Source=本机路径, Destination=容器路径）与运行状态。
2. **PathMapper（核心）**
   - 由 Mounts 表构建容器路径 ↔ 本机路径的双向映射，最长前缀匹配。
   - `to_host(container_path) -> Option<HostPath>`；`to_container(host_path) ->
     Option<ContainerPath>`。找不到映射时返回 None，由调用方给出明确错误。
   - 打开项目时构建一次，缓存于新的 `BackendManager`（Tauri state，按项目/tab
     维度存储 backend + mapper）。容器重建导致挂载变化时，文件操作报错并引导
     用户重新连接（见 §6）。
3. **容器内进程辅助**
   - `exec_capture(container, cmd)`：一次性 `docker exec` 并收集输出（用于探测
     claude、读聊天记录、杀进程等）。

### 3.3 路径空间约定（关键不变量）

**前端 UI、拖拽、发给 claude 的消息、claude 回复中的路径——全部使用容器内路径。
本机路径只存在于 Rust 内部**：每个 fs command 在入口处把容器路径经 PathMapper
翻译为本机路径后走现有逻辑，返回值（文件树节点路径、watch 事件路径）再翻译回
容器路径。

由此自动满足：
- 右侧文件树拖文件进对话框 → 插入的是容器路径 → 容器内 claude 直接可用；
- claude 回复中提到的容器路径 → 点击预览时 Rust 翻译为本机路径读取；
- `open_in_vscode` / `reveal_in_finder` → 翻译为本机路径后照常可用。

### 3.4 涉及改造的 fs command

`read_file_tree`、`read_file_content`、`write_file_content`、`copy_file`、
`rename_file`、`delete_file`、`create_directory`、`read_file_base64`、
`check_file_access`、`get_file_size`、`watch_directory` / `unwatch_directory`
（事件路径回翻）。改法统一：入口翻译 + 出口回翻，内部逻辑不动。
`PathAccessManager` 注册的是翻译后的**本机**根路径，校验逻辑不变。

## 4. CLI 进程：spawn / 停止 / 环境

### 4.1 启动（改 `start_claude_session`，lib.rs:1784 起）

按 backend 分支：

```
本机（现状）:  Command::new(claude_bin).args(args).current_dir(cwd)...
容器（新增）:  Command::new("docker")
                 .args(["exec", "-i",
                        "-w", containerCwd,
                        "-e", "KEY=VAL", ...   // provider env 注入改为 -e
                        container,
                        claude_bin_in_container])  // 默认 "claude"，设置可覆盖
                 .args(原有 claude args)
```

- stdin/stdout/stderr 仍为 `Stdio::piped()`；下游 NDJSON 处理零改动。
- 跳过 `find_claude_binary()`（那是本机发现逻辑）；容器内默认假定 PATH 中有
  `claude`，设置项允许指定容器内绝对路径。
- 无需 `env_remove("CLAUDECODE")` 等本机环境清理（docker exec 不继承本机 env）。
- 为支持精确停止（§4.2），容器内命令包一层 shell 以捕获进程号：
  `sh -c 'echo "__TOKENICODE_PID__$$" >&2; exec claude <args...>'`。
  `exec` 使 claude 沿用同一 PID；stderr 读取侧识别并吞掉该标记行，把容器内 PID
  存入 ProcessManager 的会话记录。

### 4.2 停止（改 `kill_session` 路径，lib.rs:2274 附近）

杀掉本机 `docker exec` 客户端**不会**终止容器内进程。容器模式的 kill 改为两步：

1. `docker exec <container> kill -TERM <容器内 PID>`（PID 来自 §4.1 的标记行，
   点名精确，不影响容器内其他进程；若 PID 未捕获到则回退
   `pkill -f "claude --input-format stream-json"`）；
2. 再对本机 `docker exec` 子进程 `start_kill()` 收尾。

正常结束（stdin 关闭、对话完成）与"打断回答"（走 stdin control request 的
interrupt）不受影响，无需改动。

### 4.3 登录

`open_terminal_login` 在容器模式下打开本机终端并自动执行
`docker exec -it <container> claude login`，其余交给用户在终端完成。

## 5. 聊天记录（容器内 `~/.claude`）

会话 JSONL 由容器内 claude 写入容器的 `~/.claude/projects/…`。TOKENICODE 本机侧
所有 `home.join(".claude")` 的读取点（会话历史扫描 lib.rs:1517、最近项目
lib.rs:4448、tracking 重建 lib.rs:2862、commands/skills 列表等）收口为一个
辅助函数 `resolve_claude_read(backend, relative_path)`：

| 情况 | 读取方式 |
|---|---|
| local 后端 | 本机 `~/.claude`（现状，不变） |
| docker 后端，容器 `~/.claude` 恰好在某 bind mount 下 | PathMapper 翻译后直读本机（快路径） |
| docker 后端，未挂载 | `docker exec cat / ls` 读取（慢路径，功能完整） |

保证：历史列表、点开旧会话、继续上次会话（`--resume` 由容器内 claude 自行解析）
在容器模式下全部可用。写入类操作（如删除历史 JSONL）同样经此辅助函数分派
（未挂载时 `docker exec rm`，并保留现有"仅允许 `~/.claude/projects/` 内删除"的
安全校验语义）。

## 6. 附件与项目外文件

粘贴截图、从桌面/访达拖入的文件目前存于**本机**临时目录后把路径发给 CLI
（`useFileAttachments.ts:15`）。容器看不到本机临时目录，因此容器模式下：

- 新增 command `copy_into_container(container, host_path) -> container_path`：
  用 `docker cp` 将文件拷入容器内暂存目录（`/tmp/tokenicode-attachments/<uuid>/`），
  返回容器内路径。
- 附件流程在 docker 后端下先调用它，再把**容器内路径**注入消息。
- 会话结束/应用退出时尽力清理该暂存目录（`docker exec rm -rf`，best-effort）。
- 来自项目文件树的拖拽不经此流程（本就是容器路径）。

## 7. UI 交互

### 7.1 连接容器（改 `ProjectSelector.tsx`）

「选择文件夹」旁新增「连接 Docker 容器」，三步：

1. 下拉列出运行中容器（名称 + 镜像），来自 `list_docker_containers`；
2. 选中后列出该容器的 bind mount 目标（容器内路径）；选定挂载点后可继续向下
   浏览子目录（复用 `read_file_tree`，已走映射）；
3. 确认后项目以 docker backend 存档。

最近项目列表混合展示，容器项目带容器图标 + 容器名徽标；工作区顶部常驻显示当前
连接的容器名。

### 7.2 环境预检（连接时 + 每次新会话前轻量复查）

| 检查 | 失败提示 |
|---|---|
| 本机 `docker` CLI 存在 | 「未检测到 Docker，请先安装」 |
| 容器在运行 | 「容器已停止」+ 一键 `docker start` 按钮 |
| 容器内 `which claude` | 「请在容器内安装 claude CLI」（设置可指定路径） |
| 所选目录在某 bind mount 下 | 连接阶段拒绝：「该目录不是从本机挂载的」 |

## 8. 错误处理（运行中）

| 情况 | 行为 |
|---|---|
| 会话中容器被停止 | `docker exec` 退出触发现有进程退出事件；识别后提示「容器已停止，会话中断」 |
| 挂载失效（容器重建） | PathMapper 查无映射 → 文件操作报「目录不再挂载于该容器」并引导重新连接 |
| CLI 鉴权失败 | 沿用现有错误展示，提示语改为「请进入容器执行 claude login」+ 打开终端按钮 |
| `docker cp` / exec 失败 | 透传 stderr 摘要，附上容器名便于排查 |

## 9. 已知事项（不阻塞，文档提醒）

- **文件属主**：容器以 root 运行时，claude 新建的文件在本机属 root，本机编辑可能
  需要权限。建议容器使用与本机 uid 匹配的用户运行。
- **Windows**：宿主机路径格式（`C:\`、Docker Desktop 的 `/host_mnt/...`）需要
  单独适配。第一版保证 macOS / Linux 完整可用，Windows 映射列为已知限制。

## 10. 测试策略

- **Rust 单测**：PathMapper 双向映射（最长前缀、嵌套挂载、无匹配、尾部斜杠）；
  docker exec 参数拼装（env 注入、-w、二进制覆盖）。
- **命令层测试**：fs command 在 docker 后端下的入口翻译/出口回翻（以假 mapper
  注入）。
- **手动闭环验收**：起一个挂载了本机目录、装好并登录 claude 的容器，走通
  连接 → 浏览/编辑/自动刷新 → 对话（含文件树拖拽、粘贴截图）→ 查看历史 →
  停止按钮 → 停容器观察提示。

## 11. 改动面一览

| 位置 | 改动 |
|---|---|
| `src-tauri/src/docker_backend.rs`（新） | 容器发现、PathMapper、exec 辅助 |
| `src-tauri/src/lib.rs` spawn 段 | docker exec 分支、env 改 `-e`、kill 两步走 |
| `src-tauri/src/lib.rs` fs commands | 入口翻译 + 出口回翻（约 12 个 command） |
| `src-tauri/src/lib.rs` `~/.claude` 读取点 | 收口 `resolve_claude_read` |
| `src-tauri/src/lib.rs` 新 commands | `list_docker_containers`、`inspect_container`、`copy_into_container` |
| `src/components/files/ProjectSelector.tsx` | 「连接容器」三步流程 UI |
| `src/stores/settingsStore.ts` 等 | WorkspaceBackend 模型、项目持久化 |
| `src/hooks/useFileAttachments.ts` | docker 后端下附件先 `docker cp` |
| `ProcessManager` / `StdinManager` / NDJSON 流 | **不改动** |
