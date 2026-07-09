# Docker 容器后端 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 TOKENICODE 连接一个已运行的本机 Docker 容器：打开容器内（bind-mount 自本机）的目录作为项目，claude CLI 通过 `docker exec` 在容器内运行；文件树/编辑/自动刷新体验与本机项目一致。

**Architecture:** 路径映射 + docker exec（见设计文档
`docs/superpowers/specs/2026-07-09-docker-container-backend-design.md`）。
前端与 CLI 之间流通容器路径；Rust fs 命令入口把容器路径翻译成本机路径走现有
`std::fs`/`notify` 逻辑，出口回翻。CLI spawn 在 docker 后端下包一层
`docker exec -i -w …`，管道结构不变，ProcessManager/StdinManager/NDJSON 流零改动。

**Tech Stack:** Rust (tauri 2, tokio, serde_json)、React 19 + zustand、vitest、cargo test。

## Global Constraints

- 测试命令：Rust `cd src-tauri && cargo test --lib -j 2`（必须 `-j 2`，机器内存有限，并行编译会被 OOM 杀）；前端 `pnpm vitest run`。
- 基线：Rust 168 passed / 前端 309 passed，每个任务结束必须保持全绿。
- 不引入新 crate/npm 依赖（docker 全走 CLI 子进程）。
- 前端所有用户可见文案走 `src/lib/i18n.ts` 的 t() 键值（中英双语）。
- 每个 fs 命令的翻译必须"入口翻译、出口回翻"，不改内部逻辑。
- 与设计文档的一处偏离（已确认更优）：附件不用 `docker cp`——`save_temp_file`
  本来就写到 `{cwd}/.tokenicode/tmp/`，cwd 是挂载目录，容器天然可见；外部拖入
  文件同样拷到该目录。设计文档 §6 以本计划为准。

---

### Task 1: PathMapper（docker_backend.rs 纯逻辑核心）

**Files:**
- Create: `src-tauri/src/docker_backend.rs`
- Modify: `src-tauri/src/lib.rs:11`（`mod docker_backend;` 与现有 `mod path_access;` 并排）
- Test: 同文件 `#[cfg(test)] mod tests`

**Interfaces:**
- Produces:
  - `pub struct MountEntry { pub source: String /*host*/, pub destination: String /*container*/ }`
  - `pub struct PathMapper { mounts: Vec<MountEntry> }`
  - `PathMapper::new(mounts: Vec<MountEntry>) -> Self`（按 destination 长度降序排序）
  - `PathMapper::to_host(&self, container_path: &str) -> Option<PathBuf>`
  - `PathMapper::to_container(&self, host_path: &Path) -> Option<String>`

- [ ] **Step 1: 写失败测试**（在新文件底部）

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn mapper() -> PathMapper {
        PathMapper::new(vec![
            MountEntry { source: "/Users/me/proj".into(), destination: "/workspace".into() },
            MountEntry { source: "/Users/me/data".into(), destination: "/workspace/data".into() },
        ])
    }

    #[test]
    fn to_host_maps_prefix() {
        assert_eq!(mapper().to_host("/workspace/src/a.rs").unwrap(),
            std::path::PathBuf::from("/Users/me/proj/src/a.rs"));
    }

    #[test]
    fn to_host_longest_prefix_wins() {
        // /workspace/data 是更长的挂载点，嵌套挂载必须命中它而非 /workspace
        assert_eq!(mapper().to_host("/workspace/data/x").unwrap(),
            std::path::PathBuf::from("/Users/me/data/x"));
    }

    #[test]
    fn to_host_exact_mount_root() {
        assert_eq!(mapper().to_host("/workspace").unwrap(),
            std::path::PathBuf::from("/Users/me/proj"));
    }

    #[test]
    fn to_host_rejects_non_component_prefix() {
        // /workspace2 不能误匹配 /workspace
        assert!(mapper().to_host("/workspace2/x").is_none());
    }

    #[test]
    fn to_host_unmounted_returns_none() {
        assert!(mapper().to_host("/etc/passwd").is_none());
    }

    #[test]
    fn to_container_roundtrip() {
        assert_eq!(mapper().to_container(std::path::Path::new("/Users/me/proj/src/a.rs")).unwrap(),
            "/workspace/src/a.rs");
    }

    #[test]
    fn to_container_longest_prefix_wins() {
        assert_eq!(mapper().to_container(std::path::Path::new("/Users/me/data/x")).unwrap(),
            "/workspace/data/x");
    }

    #[test]
    fn trailing_slash_normalized() {
        let m = PathMapper::new(vec![MountEntry {
            source: "/Users/me/proj/".into(), destination: "/workspace/".into() }]);
        assert_eq!(m.to_host("/workspace/a").unwrap(), std::path::PathBuf::from("/Users/me/proj/a"));
    }
}
```

- [ ] **Step 2: 跑测试确认编译失败**（类型不存在）
  Run: `cd src-tauri && cargo test --lib -j 2 docker_backend 2>&1 | tail -5`
  Expected: 编译错误 `cannot find … PathMapper`

- [ ] **Step 3: 最小实现**（文件头部）

```rust
//! Docker container backend: path mapping between container paths (what the
//! UI and the in-container CLI see) and host paths (what std::fs operates on).
//!
//! Precondition (spec §1): every opened container directory lives under a
//! bind mount, so both address spaces refer to the same files on disk.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MountEntry {
    /// Host-side path (docker inspect .Mounts[].Source)
    pub source: String,
    /// Container-side path (docker inspect .Mounts[].Destination)
    pub destination: String,
}

#[derive(Debug, Clone)]
pub struct PathMapper {
    /// Sorted by destination length descending so the longest (most specific)
    /// mount wins for nested bind mounts.
    mounts: Vec<MountEntry>,
}

fn strip_trailing_slash(s: &str) -> &str {
    if s.len() > 1 { s.trim_end_matches('/') } else { s }
}

/// Component-aware prefix check: "/workspace" is a prefix of
/// "/workspace/x" and of itself, but NOT of "/workspace2".
fn container_prefix_rest<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = path.strip_prefix(prefix)?;
    if rest.is_empty() || rest.starts_with('/') {
        Some(rest.trim_start_matches('/'))
    } else {
        None
    }
}

impl PathMapper {
    pub fn new(mut mounts: Vec<MountEntry>) -> Self {
        for m in &mut mounts {
            m.source = strip_trailing_slash(&m.source).to_string();
            m.destination = strip_trailing_slash(&m.destination).to_string();
        }
        mounts.sort_by(|a, b| b.destination.len().cmp(&a.destination.len()));
        Self { mounts }
    }

    pub fn to_host(&self, container_path: &str) -> Option<PathBuf> {
        for m in &self.mounts {
            if let Some(rest) = container_prefix_rest(container_path, &m.destination) {
                let mut p = PathBuf::from(&m.source);
                if !rest.is_empty() { p.push(rest); }
                return Some(p);
            }
        }
        None
    }

    pub fn to_container(&self, host_path: &Path) -> Option<String> {
        // Longest host prefix wins; sort order is by destination, so scan all.
        let host_str = host_path.to_string_lossy();
        let mut best: Option<(usize, String)> = None;
        for m in &self.mounts {
            if let Some(rest) = container_prefix_rest(&host_str, &m.source) {
                let mapped = if rest.is_empty() {
                    m.destination.clone()
                } else {
                    format!("{}/{}", m.destination, rest)
                };
                if best.as_ref().map_or(true, |(l, _)| m.source.len() > *l) {
                    best = Some((m.source.len(), mapped));
                }
            }
        }
        best.map(|(_, s)| s)
    }
}
```

并在 `lib.rs` 模块声明区加 `mod docker_backend;`。

- [ ] **Step 4: 跑测试确认通过**
  Run: `cargo test --lib -j 2 docker_backend 2>&1 | tail -3`
  Expected: `8 passed`；再跑全量 `cargo test --lib -j 2` 保持 168+8 绿。

- [ ] **Step 5: Commit** `feat(docker): add PathMapper for container/host path translation`

---

### Task 2: docker CLI 输出解析 + exec 参数构建

**Files:**
- Modify: `src-tauri/src/docker_backend.rs`
- Test: 同文件 tests 模块

**Interfaces:**
- Produces:
  - `pub struct ContainerSummary { pub id: String, pub name: String, pub image: String, pub state: String }`
  - `pub fn parse_docker_ps(line_delimited_json: &str) -> Vec<ContainerSummary>`
  - `pub fn parse_inspect_mounts(inspect_json: &str) -> Result<Vec<MountEntry>, String>`
    （只保留 `"Type": "bind"` 的条目）
  - `pub fn parse_inspect_running(inspect_json: &str) -> Result<bool, String>`
  - `pub async fn docker_capture(args: &[&str]) -> Result<String, String>`
    （`tokio::process::Command::new("docker")`，非 0 退出时 Err(stderr 摘要)）

- [ ] **Step 1: 写失败测试**

```rust
    const PS_FIXTURE: &str = r#"{"ID":"abc123","Names":"dev-box","Image":"ubuntu:24.04","State":"running"}
{"ID":"def456","Names":"db","Image":"postgres:16","State":"running"}"#;

    const INSPECT_FIXTURE: &str = r#"[{
      "State": {"Running": true},
      "Mounts": [
        {"Type": "bind", "Source": "/Users/me/proj", "Destination": "/workspace"},
        {"Type": "volume", "Source": "/var/lib/docker/volumes/v1/_data", "Destination": "/data"}
      ]
    }]"#;

    #[test]
    fn parse_ps_lines() {
        let list = parse_docker_ps(PS_FIXTURE);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "dev-box");
        assert_eq!(list[0].image, "ubuntu:24.04");
    }

    #[test]
    fn parse_ps_ignores_garbage_lines() {
        assert_eq!(parse_docker_ps("not json\n").len(), 0);
    }

    #[test]
    fn inspect_mounts_keeps_only_binds() {
        let mounts = parse_inspect_mounts(INSPECT_FIXTURE).unwrap();
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].destination, "/workspace");
    }

    #[test]
    fn inspect_running_state() {
        assert!(parse_inspect_running(INSPECT_FIXTURE).unwrap());
    }

    #[test]
    fn inspect_bad_json_is_err() {
        assert!(parse_inspect_mounts("[]").is_err()); // 空数组=容器不存在
    }
```

- [ ] **Step 2: 确认失败**（函数未定义，编译错误）
- [ ] **Step 3: 实现**

```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct ContainerSummary {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: String,
}

pub fn parse_docker_ps(output: &str) -> Vec<ContainerSummary> {
    output
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line.trim()).ok())
        .filter_map(|v| {
            Some(ContainerSummary {
                id: v.get("ID")?.as_str()?.to_string(),
                name: v.get("Names")?.as_str()?.to_string(),
                image: v.get("Image")?.as_str()?.to_string(),
                state: v.get("State")?.as_str().unwrap_or("unknown").to_string(),
            })
        })
        .collect()
}

fn inspect_first(inspect_json: &str) -> Result<serde_json::Value, String> {
    let v: serde_json::Value = serde_json::from_str(inspect_json)
        .map_err(|e| format!("docker inspect: invalid JSON: {}", e))?;
    v.as_array()
        .and_then(|a| a.first().cloned())
        .ok_or_else(|| "docker inspect: container not found".to_string())
}

pub fn parse_inspect_mounts(inspect_json: &str) -> Result<Vec<MountEntry>, String> {
    let first = inspect_first(inspect_json)?;
    let mounts = first.get("Mounts").and_then(|m| m.as_array()).cloned().unwrap_or_default();
    Ok(mounts
        .iter()
        .filter(|m| m.get("Type").and_then(|t| t.as_str()) == Some("bind"))
        .filter_map(|m| {
            Some(MountEntry {
                source: m.get("Source")?.as_str()?.to_string(),
                destination: m.get("Destination")?.as_str()?.to_string(),
            })
        })
        .collect())
}

pub fn parse_inspect_running(inspect_json: &str) -> Result<bool, String> {
    let first = inspect_first(inspect_json)?;
    Ok(first
        .pointer("/State/Running")
        .and_then(|b| b.as_bool())
        .unwrap_or(false))
}

/// Run `docker <args>` and capture stdout; Err carries a stderr summary.
pub async fn docker_capture(args: &[&str]) -> Result<String, String> {
    let output = tokio::process::Command::new("docker")
        .args(args)
        .output()
        .await
        .map_err(|e| format!("docker not available: {}", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("docker {} failed: {}", args.first().unwrap_or(&""), stderr.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
```

- [ ] **Step 4: 全量测试绿** → **Step 5: Commit** `feat(docker): parse docker ps/inspect output, docker_capture helper`

---

### Task 3: BackendManager（全局状态）+ 连接/列表 commands

**Files:**
- Modify: `src-tauri/src/docker_backend.rs`（BackendManager）
- Modify: `src-tauri/src/lib.rs`（3 个新 command + `.manage()` + generate_handler 注册）
- Test: docker_backend.rs tests

**Interfaces:**
- Produces:
  - `pub struct DockerProject { pub container: String, pub mapper: PathMapper, pub home: Option<String> /*容器内 $HOME*/ }`
  - `#[derive(Clone, Default)] pub struct BackendManager { projects: Arc<Mutex<Vec<DockerProject>>> }`（tokio Mutex，模式与 `PathAccessManager` 一致）
  - `BackendManager::register(&self, project: DockerProject)`（同名容器去重替换）
  - `BackendManager::to_host_or_passthrough(&self, path: &str) -> PathBuf`
    （扫描所有 docker 项目取映射；无匹配原样返回 —— **本机项目路径天然走 passthrough，是所有 fs 命令的统一入口**）
  - `BackendManager::to_container_for_host(&self, host: &Path) -> Option<(String /*container*/, String /*container path*/)>`
  - `BackendManager::backend_for_cwd(&self, cwd: &str) -> Option<DockerProject>`
    （cwd 能被某项目 to_host 映射即命中）
  - Tauri commands（lib.rs）：
    - `list_docker_containers() -> Result<Vec<ContainerSummary>, String>`
    - `connect_docker_project(backends: State<BackendManager>, path_access: State<PathAccessManager>, container: String, container_cwd: String) -> Result<(), String>`
    - `docker_preflight(container: String) -> Result<(), String>`（容器运行中 + `which claude`）

- [ ] **Step 1: 写失败测试**（异步方法用 `tokio::test`；Cargo.toml 已含 tokio full，无需新依赖）

```rust
    fn docker_proj() -> DockerProject {
        DockerProject {
            container: "dev-box".into(),
            mapper: PathMapper::new(vec![MountEntry {
                source: "/Users/me/proj".into(), destination: "/workspace".into() }]),
            home: Some("/root".into()),
        }
    }

    #[tokio::test]
    async fn passthrough_for_local_paths() {
        let mgr = BackendManager::default();
        mgr.register(docker_proj()).await;
        assert_eq!(mgr.to_host_or_passthrough("/tmp/x").await,
            std::path::PathBuf::from("/tmp/x"));
    }

    #[tokio::test]
    async fn container_path_maps_to_host() {
        let mgr = BackendManager::default();
        mgr.register(docker_proj()).await;
        assert_eq!(mgr.to_host_or_passthrough("/workspace/a.rs").await,
            std::path::PathBuf::from("/Users/me/proj/a.rs"));
    }

    #[tokio::test]
    async fn host_path_reverse_maps() {
        let mgr = BackendManager::default();
        mgr.register(docker_proj()).await;
        let (c, p) = mgr.to_container_for_host(std::path::Path::new("/Users/me/proj/a.rs")).await.unwrap();
        assert_eq!(c, "dev-box");
        assert_eq!(p, "/workspace/a.rs");
    }

    #[tokio::test]
    async fn backend_for_cwd_matches_docker_project() {
        let mgr = BackendManager::default();
        mgr.register(docker_proj()).await;
        assert_eq!(mgr.backend_for_cwd("/workspace").await.unwrap().container, "dev-box");
        assert!(mgr.backend_for_cwd("/Users/me/other").await.is_none());
    }

    #[tokio::test]
    async fn register_same_container_replaces() {
        let mgr = BackendManager::default();
        mgr.register(docker_proj()).await;
        mgr.register(docker_proj()).await;
        assert_eq!(mgr.projects.lock().await.len(), 1);
    }
```

- [ ] **Step 2: 确认失败** → **Step 3: 实现 BackendManager**

```rust
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub struct DockerProject {
    pub container: String,
    pub mapper: PathMapper,
    /// Container-side $HOME (for locating ~/.claude inside the container).
    pub home: Option<String>,
}

#[derive(Clone, Default)]
pub struct BackendManager {
    pub projects: Arc<Mutex<Vec<DockerProject>>>,
}

impl BackendManager {
    pub async fn register(&self, project: DockerProject) {
        let mut projects = self.projects.lock().await;
        projects.retain(|p| p.container != project.container);
        projects.push(project);
    }

    /// Container path → host path; non-container paths pass through unchanged.
    /// This is the single entry point used by every fs command, so local
    /// projects keep working with zero behavioural change.
    pub async fn to_host_or_passthrough(&self, path: &str) -> PathBuf {
        let projects = self.projects.lock().await;
        for p in projects.iter() {
            if let Some(host) = p.mapper.to_host(path) {
                return host;
            }
        }
        PathBuf::from(path)
    }

    pub async fn to_container_for_host(&self, host: &Path) -> Option<(String, String)> {
        let projects = self.projects.lock().await;
        for p in projects.iter() {
            if let Some(c) = p.mapper.to_container(host) {
                return Some((p.container.clone(), c));
            }
        }
        None
    }

    pub async fn backend_for_cwd(&self, cwd: &str) -> Option<DockerProject> {
        let projects = self.projects.lock().await;
        projects.iter().find(|p| p.mapper.to_host(cwd).is_some()).cloned()
    }
}
```

注意：passthrough 有一个理论歧义——本机也可能真实存在 `/workspace` 这样的路径。
按前提"容器项目路径总在挂载表里"，映射优先于 passthrough，可接受；测试
`passthrough_for_local_paths` 用 `/tmp/x` 这类不在挂载表内的路径。

- [ ] **Step 4: lib.rs 添加 commands 与注册**

```rust
#[tauri::command]
async fn list_docker_containers() -> Result<Vec<docker_backend::ContainerSummary>, String> {
    let out = docker_backend::docker_capture(&["ps", "--format", "{{json .}}"]).await?;
    Ok(docker_backend::parse_docker_ps(&out))
}

#[tauri::command]
async fn connect_docker_project(
    backends: State<'_, docker_backend::BackendManager>,
    path_access: State<'_, PathAccessManager>,
    container: String,
    container_cwd: String,
) -> Result<(), String> {
    let inspect = docker_backend::docker_capture(&["inspect", &container]).await?;
    if !docker_backend::parse_inspect_running(&inspect)? {
        return Err(format!("CONTAINER_NOT_RUNNING:{}", container));
    }
    let mounts = docker_backend::parse_inspect_mounts(&inspect)?;
    let mapper = docker_backend::PathMapper::new(mounts);
    let host_cwd = mapper
        .to_host(&container_cwd)
        .ok_or_else(|| format!("PATH_NOT_MOUNTED:{}", container_cwd))?;
    if !host_cwd.exists() {
        return Err(format!("HOST_PATH_MISSING:{}", host_cwd.display()));
    }
    // Container-side $HOME, best-effort (used later for reading ~/.claude).
    let home = docker_backend::docker_capture(&["exec", &container, "sh", "-c", "echo $HOME"])
        .await
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    path_access.register_cwd(&host_cwd).await;
    backends
        .register(docker_backend::DockerProject { container, mapper, home })
        .await;
    Ok(())
}

#[tauri::command]
async fn docker_preflight(container: String) -> Result<(), String> {
    let inspect = docker_backend::docker_capture(&["inspect", &container]).await?;
    if !docker_backend::parse_inspect_running(&inspect)? {
        return Err(format!("CONTAINER_NOT_RUNNING:{}", container));
    }
    docker_backend::docker_capture(&["exec", &container, "sh", "-c", "command -v claude"])
        .await
        .map_err(|_| format!("CLAUDE_NOT_FOUND_IN_CONTAINER:{}", container))?;
    Ok(())
}
```

`run()` 里与 `.manage(PathAccessManager::new())` 并排加
`.manage(docker_backend::BackendManager::default())`；generate_handler 列表加 3 个新命令。
错误码用 `CODE:detail` 字符串前缀，前端据此显示 i18n 文案（Task 7）。

- [ ] **Step 5: 全量测试绿 + Commit** `feat(docker): BackendManager state and container connect/list/preflight commands`

---

### Task 4: fs 命令路径翻译接线

**Files:**
- Modify: `src-tauri/src/lib.rs` 中以下命令（行号为当前值）：
  `read_file_tree:4066`、`read_file_content:4154`、`check_file_access:4178`、
  `read_file_base64:4190`、`write_file_content:4241`、`copy_file:4258`、
  `rename_file:4284`、`delete_file:4308`、`create_directory:4325`、
  `get_file_size:4609`、`save_temp_file:4559`、`watch_directory:4459`、
  `open_in_vscode:3621`、`reveal_in_finder:3632`、`open_with_default_app:3665`
- Test: docker_backend.rs（翻译辅助已测）+ 手动回归本机项目

**Interfaces:**
- Consumes: `BackendManager::to_host_or_passthrough` / `to_container_for_host`（Task 3）
- Produces: 各命令签名增加 `backends: State<'_, docker_backend::BackendManager>` 参数（Tauri 注入，前端不用改调用）。

改法模式（以 read_file_content 为例，其余同样）：

```rust
async fn read_file_content(
    path_access: State<'_, PathAccessManager>,
    backends: State<'_, docker_backend::BackendManager>,
    path: String,
) -> Result<String, String> {
    let host_path = backends.to_host_or_passthrough(&path).await;
    let path = host_path.to_string_lossy().to_string();
    // …以下原逻辑不动…
```

特殊点：
- `read_file_tree`：入口翻译后，`read_dir_recursive` 产出的 `FileNode.path`
  是本机路径 —— 出口需要回翻。给 `read_file_tree` 加一步：若入参被映射过
  （host != 原 path），对返回树做一次 DFS，把每个 node.path 从 host 前缀替换回
  container 前缀（直接 `to_container_for_host`）。
- `watch_directory`：入口翻译 path 给 notify；回调里 emit 前对每个事件 path 做
  `to_container_for_host`（有映射用容器路径，无映射用原样）。注意回调是同步闭包，
  拿不到 async lock —— 解法：watch 注册时把当前 mapper snapshot（`Option<PathMapper>`）
  clone 进闭包，同步调用 `mapper.to_container()`。watchers HashMap 的 key 保持
  前端传入的容器路径（unwatch 用同一 key）。
- `save_temp_file`：cwd 参数入口翻译（文件落到挂载的项目 tmp 目录），返回值回翻成
  容器路径（前端要把它发给 CLI）。
- `copy_file`：src 和 dst 都翻译。
- `open_in_vscode`/`reveal_in_finder`/`open_with_default_app`：入口翻译即可
  （翻译成本机路径后现有实现直接可用）。

- [ ] **Step 1: 失败测试** —— 树回翻逻辑抽成纯函数并先写测试：

```rust
    #[test]
    fn remap_tree_paths_rewrites_prefix() {
        let mapper = PathMapper::new(vec![MountEntry {
            source: "/Users/me/proj".into(), destination: "/workspace".into() }]);
        // remap_path_out: host path string → container path string (passthrough if unmapped)
        assert_eq!(remap_path_out(&Some(mapper.clone()), "/Users/me/proj/src"), "/workspace/src");
        assert_eq!(remap_path_out(&None, "/tmp/x"), "/tmp/x");
    }
```

`pub fn remap_path_out(mapper: &Option<PathMapper>, host: &str) -> String` 放
docker_backend.rs，watch 回调与文件树 DFS 共用。

- [ ] **Step 2: 确认失败 → Step 3: 实现 remap_path_out + 逐个命令接线**

```rust
pub fn remap_path_out(mapper: &Option<PathMapper>, host: &str) -> String {
    match mapper {
        Some(m) => m
            .to_container(Path::new(host))
            .unwrap_or_else(|| host.to_string()),
        None => host.to_string(),
    }
}
```

read_file_tree 出口回翻（在 `Ok(read_dir_recursive(...))` 前）：

```rust
    let mapper = backends.mapper_for_container_path(&original_path).await; // Option<PathMapper>
    let mut nodes = read_dir_recursive(root, 0, max_depth);
    if mapper.is_some() {
        fn remap(nodes: &mut Vec<FileNode>, mapper: &Option<docker_backend::PathMapper>) {
            for n in nodes.iter_mut() {
                n.path = docker_backend::remap_path_out(mapper, &n.path);
                if let Some(ref mut ch) = n.children { remap(ch, mapper); }
            }
        }
        remap(&mut nodes, &mapper);
    }
    Ok(nodes)
```

需要在 BackendManager 增加
`pub async fn mapper_for_container_path(&self, path: &str) -> Option<PathMapper>`
（哪个项目能映射该路径就返回其 mapper clone；含测试）。

- [ ] **Step 4: 全量 Rust 测试绿；`pnpm vitest run` 绿（前端无改动，确认没破）**
- [ ] **Step 5: Commit** `feat(docker): translate container paths in all fs commands and watcher events`

---

### Task 5: CLI spawn 的 docker exec 分支 + PID 捕获 + kill 两步走

**Files:**
- Modify: `src-tauri/src/docker_backend.rs`（命令构建纯函数 + 测试）
- Modify: `src-tauri/src/lib.rs:1889-1899`（binary 解析）、`:2127-2149`（Unix spawn）、`:2064-2126`（Windows spawn 分支加 docker 时直接走同一 docker 分支）、`:2637`（stderr reader 加 PID 标记解析）、`kill_session:2802`
- Modify: `src-tauri/src/commands/claude_process.rs:191`（StartSessionParams 加字段）、`ManagedProcess` 加 `container_kill: Option<(String, Arc<std::sync::atomic::AtomicU32>)>`
- Test: docker_backend.rs

**Interfaces:**
- Consumes: `BackendManager::backend_for_cwd`（Task 3）
- Produces:
  - `pub struct DockerExecSpec { pub program: String, pub args: Vec<String> }`
  - `pub fn build_docker_exec(container: &str, container_cwd: &str, env: &[(String, String)], claude_bin: &str, claude_args: &[String]) -> DockerExecSpec`
  - `pub fn parse_pid_marker(stderr_line: &str) -> Option<u32>`（识别 `__TOKENICODE_PID__<n>`）
  - `StartSessionParams` 新字段：`pub docker_container: Option<String>`（serde 默认 None，兼容旧前端调用）

- [ ] **Step 1: 失败测试**

```rust
    #[test]
    fn docker_exec_command_shape() {
        let spec = build_docker_exec(
            "dev-box", "/workspace",
            &[("ANTHROPIC_BASE_URL".into(), "https://x".into())],
            "claude",
            &["--input-format".into(), "stream-json".into()],
        );
        assert_eq!(spec.program, "docker");
        assert_eq!(spec.args[..6], ["exec".to_string(), "-i".into(), "-w".into(),
            "/workspace".into(), "-e".into(), "ANTHROPIC_BASE_URL=https://x".into()]);
        assert_eq!(spec.args[6], "dev-box");
        // 包 sh -c 以回报 PID；exec 保证 claude 沿用同一 PID
        assert_eq!(spec.args[7], "sh");
        assert_eq!(spec.args[8], "-c");
        let script = &spec.args[9];
        assert!(script.starts_with("echo \"__TOKENICODE_PID__$$\" >&2; exec claude"));
        assert!(script.contains("'--input-format' 'stream-json'"));
    }

    #[test]
    fn shell_quoting_escapes_single_quotes() {
        let spec = build_docker_exec("c", "/w", &[], "claude", &["it's".into()]);
        assert!(spec.args[9].contains(r#"'it'\''s'"#));
    }

    #[test]
    fn pid_marker_parses() {
        assert_eq!(parse_pid_marker("__TOKENICODE_PID__1234"), Some(1234));
        assert_eq!(parse_pid_marker("random stderr"), None);
    }
```

- [ ] **Step 2: 确认失败 → Step 3: 实现**

```rust
#[derive(Debug)]
pub struct DockerExecSpec {
    pub program: String,
    pub args: Vec<String>,
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

pub fn build_docker_exec(
    container: &str,
    container_cwd: &str,
    env: &[(String, String)],
    claude_bin: &str,
    claude_args: &[String],
) -> DockerExecSpec {
    let mut args = vec![
        "exec".to_string(),
        "-i".to_string(),
        "-w".to_string(),
        container_cwd.to_string(),
    ];
    for (k, v) in env {
        args.push("-e".to_string());
        args.push(format!("{}={}", k, v));
    }
    args.push(container.to_string());
    // Report the shell PID to stderr, then exec claude so it keeps that PID.
    // kill_session uses this PID for an in-container `kill -TERM`.
    let quoted: Vec<String> = claude_args.iter().map(|a| shell_quote(a)).collect();
    let script = format!(
        "echo \"__TOKENICODE_PID__$$\" >&2; exec {} {}",
        claude_bin,
        quoted.join(" ")
    );
    args.extend(["sh".to_string(), "-c".to_string(), script]);
    DockerExecSpec { program: "docker".to_string(), args }
}

pub fn parse_pid_marker(line: &str) -> Option<u32> {
    line.trim().strip_prefix("__TOKENICODE_PID__")?.parse().ok()
}
```

- [ ] **Step 4: lib.rs 接线**（要点，代码就位于所列行号）
  1. `start_claude_session` 开头：`let docker_backend_proj = match params.docker_container { Some(ref c) => backends.project_by_container(c).await, None => backends.backend_for_cwd(&params.cwd).await };`
     （`project_by_container` 为 BackendManager 新增的简单查找，含测试；双通道
     保证前端显式传容器名或仅凭 cwd 都能命中。）
  2. docker 分支跳过 `find_claude_binary()`/`build_enriched_path()`；`claude_bin`
     取设置覆盖值或 `"claude"`。
  3. spawn 处（Unix 与 Windows 分支之前）加：

```rust
    let mut child = if let Some(ref proj) = docker_backend_proj {
        let env_pairs: Vec<(String, String)> =
            resolved_env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let spec = docker_backend::build_docker_exec(
            &proj.container, &params.cwd, &env_pairs, &claude_bin, &args);
        Command::new(&spec.program)
            .args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn docker exec: {}", e))?
    } else {
        /* 现有 Unix/Windows spawn 代码整体挪进 else */
    };
```

  4. stderr reader（`:2637` 起的任务）循环里：

```rust
    if let Some(pid) = docker_backend::parse_pid_marker(&line) {
        container_pid_clone.store(pid, std::sync::atomic::Ordering::SeqCst);
        continue; // 吞掉标记行，不发给前端
    }
```

  5. `ManagedProcess` 增加字段
     `pub container_kill: Option<(String, Arc<std::sync::atomic::AtomicU32>)>`；
     insert 时 docker 分支填 `Some((container.clone(), container_pid.clone()))`。
  6. `kill_session`：`state.remove` 之前加：

```rust
    if let Some((container, pid_atomic)) = state.container_kill_info(&session_id).await {
        let pid = pid_atomic.load(std::sync::atomic::Ordering::SeqCst);
        let _ = if pid > 0 {
            docker_backend::docker_capture(&["exec", &container, "kill", "-TERM", &pid.to_string()]).await
        } else {
            docker_backend::docker_capture(&["exec", &container, "pkill", "-f",
                "claude --input-format stream-json"]).await
        };
    }
```

     `ProcessManager::container_kill_info` 为 claude_process.rs 新增只读查找。
  7. `start_claude_session` 的 `path_access.register_cwd`（`:1792`）改为注册翻译后
     的 host cwd：`backends.to_host_or_passthrough(&params.cwd).await`。

- [ ] **Step 5: 全量测试绿 + Commit** `feat(docker): spawn claude via docker exec with PID capture and two-step kill`

---

### Task 6: 会话历史（容器 ~/.claude 读取）

**Files:**
- Modify: `src-tauri/src/lib.rs`：`find_session_jsonl:1413`、`list_recent_projects:4376`、
  `load_tracked_sessions:2835` 的 projects 扫描、`list_sessions`/`search_sessions`/
  `load_session` 共用的 projects 目录枚举点（都经 `home.join(".claude").join("projects")`）
- Modify: `src-tauri/src/docker_backend.rs`
- Test: docker_backend.rs

**Interfaces:**
- Produces:
  - `BackendManager::extra_claude_projects_dirs(&self) -> Vec<PathBuf>`：对每个
    docker 项目，若 `home` 已知且 `mapper.to_host(&format!("{}/.claude/projects", home))`
    有值且该 host 目录存在 → 收集。**挂载了容器家目录时，历史读取零成本复用现有代码。**
  - lib.rs 辅助 `fn claude_projects_dirs(backends: &BackendManager) -> Vec<PathBuf>`
    （tokio runtime 内用 `block_in_place`/改 async 调用方；首元素恒为本机
    `~/.claude/projects`，追加 extras）。
- 未挂载家目录的容器：v1 降级为「历史列表提示条」（Task 7 前端在 docker 项目且
  extras 探测失败时显示"容器内会话历史不可见（未挂载 ~/.claude）"）。
  `--resume` 不受影响（容器内 CLI 自行读取）。**此为对设计文档 §5 exec-回退的
  有意收缩**：exec 逐文件读 JSONL 的成本/复杂度高，v1 先交付挂载路径 + 明确提示，
  exec 回退列入后续迭代。

- [ ] **Step 1: 失败测试**

```rust
    #[tokio::test]
    async fn extra_claude_dirs_only_when_mounted_and_exists() {
        let tmp = std::env::temp_dir().join(format!("tok-test-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join(".claude/projects")).unwrap();
        let mgr = BackendManager::default();
        mgr.register(DockerProject {
            container: "c1".into(),
            mapper: PathMapper::new(vec![MountEntry {
                source: tmp.to_string_lossy().into(), destination: "/root".into() }]),
            home: Some("/root".into()),
        }).await;
        // 家目录挂载且 host 侧存在 → 收集
        assert_eq!(mgr.extra_claude_projects_dirs().await,
            vec![tmp.join(".claude/projects")]);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn extra_claude_dirs_empty_when_unmounted() {
        let mgr = BackendManager::default();
        mgr.register(docker_proj()).await; // home=/root 未在挂载表
        assert!(mgr.extra_claude_projects_dirs().await.is_empty());
    }
```

- [ ] **Step 2: 确认失败 → Step 3: 实现**

```rust
impl BackendManager {
    pub async fn extra_claude_projects_dirs(&self) -> Vec<PathBuf> {
        let projects = self.projects.lock().await;
        projects
            .iter()
            .filter_map(|p| {
                let home = p.home.as_deref()?;
                let host = p.mapper.to_host(&format!("{}/.claude/projects", home))?;
                host.exists().then_some(host)
            })
            .collect()
    }
}
```

- [ ] **Step 4: lib.rs 接线**：上述扫描点从单一 `claude_projects` 目录改为
  `for dir in claude_projects_dirs(...)` 循环（原逻辑体不变，仅套一层目录迭代）。
  `find_session_jsonl` 同样在多个 roots 里找第一个命中。
  新增 command `docker_history_available(backends: State<BackendManager>, container: String) -> Result<bool, String>`
  供前端决定是否显示提示条。
- [ ] **Step 5: 全量测试绿 + Commit** `feat(docker): read session history from mounted container ~/.claude`

---

### Task 7: 前端 — backend 模型、bridge、连接容器 UI

**Files:**
- Modify: `src/lib/tauri-bridge.ts`（类型 + 5 个新 invoke 封装 + startSession 传 dockerContainer）
- Modify: `src/stores/settingsStore.ts`（workingBackend 状态 + persist + 迁移默认 local）
- Modify: `src/components/files/ProjectSelector.tsx`（连接容器按钮 + 选择弹层）
- Create: `src/components/files/DockerConnectDialog.tsx`
- Modify: `src/lib/i18n.ts`（新增 docker.* 文案，中英）
- Test: `src/stores/__tests__/workspaceBackend.test.ts`（vitest，store 逻辑）

**Interfaces:**
- Produces（bridge）：

```ts
export interface ContainerSummary { id: string; name: string; image: string; state: string }
export type WorkspaceBackend =
  | { kind: 'local' }
  | { kind: 'docker'; container: string; containerCwd: string };

listDockerContainers(): Promise<ContainerSummary[]>          // invoke('list_docker_containers')
connectDockerProject(container: string, containerCwd: string) // invoke('connect_docker_project', ...)
dockerPreflight(container: string): Promise<void>
dockerHistoryAvailable(container: string): Promise<boolean>
```

- settingsStore 新增：`workingBackend: WorkspaceBackend`（默认 `{kind:'local'}`，
  随 workingDirectory 持久化）、`setWorkingProject(dir: string, backend: WorkspaceBackend)`
  （原 `setWorkingDirectory(dir)` 保留并等价于 local backend，兼容所有既有调用点）。
- startSession 调用点（`tauri-bridge.ts:268` 的 `StartSessionOptions`）加
  `dockerContainer?: string`，由发起会话处从 settingsStore 读
  `workingBackend.kind === 'docker' ? backend.container : undefined` 传入
  （Rust 侧 serde 字段 `docker_container`）。

- [ ] **Step 1: 失败测试**（`src/stores/__tests__/workspaceBackend.test.ts`）

```ts
import { describe, it, expect, beforeEach } from 'vitest';
import { useSettingsStore } from '../settingsStore';

describe('workspace backend', () => {
  beforeEach(() => {
    useSettingsStore.setState({ workingDirectory: '', workingBackend: { kind: 'local' } });
  });

  it('defaults to local backend', () => {
    expect(useSettingsStore.getState().workingBackend).toEqual({ kind: 'local' });
  });

  it('setWorkingDirectory keeps local backend', () => {
    useSettingsStore.getState().setWorkingDirectory('/Users/me/p');
    expect(useSettingsStore.getState().workingBackend.kind).toBe('local');
  });

  it('setWorkingProject stores docker backend with container and cwd', () => {
    useSettingsStore.getState().setWorkingProject('/workspace', {
      kind: 'docker', container: 'dev-box', containerCwd: '/workspace' });
    const s = useSettingsStore.getState();
    expect(s.workingDirectory).toBe('/workspace');
    expect(s.workingBackend).toEqual({ kind: 'docker', container: 'dev-box', containerCwd: '/workspace' });
  });

  it('switching back to a local folder resets backend', () => {
    useSettingsStore.getState().setWorkingProject('/workspace', {
      kind: 'docker', container: 'dev-box', containerCwd: '/workspace' });
    useSettingsStore.getState().setWorkingDirectory('/Users/me/p');
    expect(useSettingsStore.getState().workingBackend).toEqual({ kind: 'local' });
  });
});
```

- [ ] **Step 2: `pnpm vitest run workspaceBackend` 确认失败**
- [ ] **Step 3: 实现** settingsStore（interface + 实现 + partialize 持久化 backend）、
  bridge 封装、DockerConnectDialog：
  - 弹层第 1 屏：容器列表（`listDockerContainers()`，仅 state==="running"，显示
    name + image；空列表显示 `t('docker.noContainers')`；invoke 报错含
    `docker not available` 时显示 `t('docker.notInstalled')`）。
  - 第 2 屏：`connectDockerProject` 之前先按 inspect 的挂载列表展示 Destination
    可选项（新增 bridge `listContainerMounts(container)` → 新 Rust command
    `list_container_mounts(container) -> Vec<MountEntry>`，实现 = inspect + parse，
    与 Task 3 代码共用，含注册）；选中挂载点后调 `connectDockerProject(container, dest)`，
    成功后 `read_file_tree` 直接可浏览子目录（复用 FileExplorer 已有 UI，不新做树）。
    v1 简化：选中挂载点即作为项目根（子目录细选交给文件树浏览），确认按钮把
    `setWorkingProject(dest, {kind:'docker', container, containerCwd: dest})` 落库。
  - ProjectSelector 在"选择文件夹"按钮旁加 `t('docker.connectBtn')` 按钮打开弹层；
    docker 项目激活时组件顶部显示 `🐳 {container}` 徽标（用现有 chip 样式）。
  - 错误码映射：`CONTAINER_NOT_RUNNING`→`t('docker.notRunning')`（附一键
    `docker start`：新增 bridge `startContainer(name)` → Rust command
    `docker_start(container)` = `docker_capture(&["start", &container])`）；
    `PATH_NOT_MOUNTED`→`t('docker.notMounted')`；`CLAUDE_NOT_FOUND_IN_CONTAINER`→
    `t('docker.claudeMissing')`。
  - i18n 键（en/zh 各一份）：`docker.connectBtn`（Connect Docker container/连接
    Docker 容器）、`docker.noContainers`、`docker.notInstalled`、`docker.notRunning`、
    `docker.startContainer`、`docker.notMounted`、`docker.claudeMissing`、
    `docker.historyUnavailable`、`docker.pickMount`、`docker.badge`。
- [ ] **Step 4: `pnpm vitest run` 全绿；`pnpm build`（tsc）无类型错误**
- [ ] **Step 5: Commit** `feat(docker): workspace backend model, container connect dialog, i18n`

---

### Task 8: 会话接线 + 附件 + 登录 + 历史提示条

**Files:**
- Modify: 发起会话处（`grep -n "startSession(" src/ -r` 的调用点，把
  `dockerContainer` 从 settingsStore 注入）
- Modify: `src/hooks/useFileAttachments.ts:174` `addFilePaths`（docker 项目下，
  外部拖入的本机文件先经新 bridge `stageExternalFile` 拷进项目 tmp）
- Modify: `src-tauri/src/lib.rs`（新 command `stage_external_file`）
- Modify: `open_terminal_login:8122`（docker 分支）
- Modify: 历史面板组件（`dockerHistoryAvailable()===false` 时显示
  `t('docker.historyUnavailable')` 提示条）
- Test: docker_backend.rs（stage 目标路径纯函数）+ 手动

**Interfaces:**
- `stage_external_file(backends, path_access, cwd: String /*容器路径*/, src: String /*本机路径*/) -> Result<String /*容器路径*/, String>`：
  host_cwd = 翻译(cwd)；目标 = `{host_cwd}/.tokenicode/tmp/{uuid}-{filename}`；
  `std::fs::copy`；返回 `{cwd}/.tokenicode/tmp/{同名}`（容器路径，直接可发给 CLI）。
  复用 save_temp_file 的 gitignore 写入逻辑（抽成小函数 `ensure_tokenicode_tmp(dir)`）。
- `open_terminal_login` 加参数 `container: Option<String>`：Some 时终端命令改为
  `docker exec -it <container> claude login`（macOS osascript / Linux
  gnome-terminal / Windows cmd 三分支只换命令字符串，结构不动）。
- 粘贴图片路径已由 Task 4 的 save_temp_file 翻译覆盖，无需前端改动；仅
  `addFilePaths`（OS 拖拽外部文件）需在 docker 项目下改调 `stageExternalFile`。

- [ ] **Step 1: 失败测试**（目标路径构造纯函数）

```rust
    #[test]
    fn staged_container_path_shape() {
        let p = staged_attachment_container_path("/workspace", "photo.png", "abcd1234");
        assert_eq!(p, "/workspace/.tokenicode/tmp/abcd1234-photo.png");
    }
```

- [ ] **Step 2: 确认失败 → Step 3: 实现 + 接线**（`staged_attachment_container_path`
  放 docker_backend.rs：`format!("{}/.tokenicode/tmp/{}-{}", cwd, stamp, name)`）
- [ ] **Step 4: 全量 Rust + vitest 绿** → **Step 5: Commit**
  `feat(docker): external attachment staging, in-container login, history notice`

---

### Task 9: 收尾 — 运行中断提示、CHANGELOG、spec 同步

**Files:**
- Modify: 进程退出事件的前端处理（useStreamProcessor 或 chatStore 的 process_exit
  分支）：会话 backend 为 docker 且退出异常时，附加 `t('docker.containerStopped')`
  文案（需先 `dockerPreflight` 探测确认是容器停了再显示，避免误报）。
- Modify: `CHANGELOG.md`（0.12.0 unreleased 段新增 feature 条目，中英）。
- Modify: `docs/superpowers/specs/2026-07-09-docker-container-backend-design.md`
  §5/§6：按实现落地更新（历史 exec 回退收缩为 v1 提示条 + 附件改项目 tmp 方案）。
- Test: 全量回归。

- [ ] **Step 1: 前端 containerStopped 分支 + i18n**
- [ ] **Step 2: CHANGELOG + spec 更新**
- [ ] **Step 3: `cargo test --lib -j 2` + `pnpm vitest run` + `pnpm build` 全绿**
- [ ] **Step 4: Commit** `feat(docker): container-stopped notice, changelog, spec sync`

---

## 手动验收（需要有 Docker 的机器，本开发环境无 socket）

1. `docker run -d --name dev-box -v ~/proj:/workspace -it ubuntu sleep infinity`，
   容器内装 node + claude 并 `claude login`。
2. TOKENICODE →「连接 Docker 容器」→ 选 dev-box → 选 /workspace → 文件树可浏览、
   预览、编辑；本机改文件树自动刷新。
3. 发消息，claude 在容器内执行（容器内 `ps` 可见）；文件树拖文件进对话框显示
   `/workspace/...` 路径且 CLI 能读；粘贴截图能被 CLI 读到。
4. 停止按钮 → 容器内 claude 进程消失。
5. `docker stop dev-box` 会话中 → 出现「容器已停止」提示。
6. 挂载 `-v ~/dockerhome:/root` 的容器 → 历史列表能显示容器会话；未挂载 → 提示条。
