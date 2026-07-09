//! Docker container backend: path mapping between container paths (what the
//! UI and the in-container CLI see) and host paths (what std::fs operates on).
//!
//! Precondition (spec §1): every opened container directory lives under a
//! bind mount, so both address spaces refer to the same files on disk.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

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
    if s.len() > 1 {
        s.trim_end_matches('/')
    } else {
        s
    }
}

/// Component-aware prefix check: "/workspace" is a prefix of "/workspace/x"
/// and of itself, but NOT of "/workspace2". Returns the remainder without a
/// leading slash.
fn prefix_rest<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
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

    /// Container path → host path. None if the path is under no bind mount.
    pub fn to_host(&self, container_path: &str) -> Option<PathBuf> {
        for m in &self.mounts {
            if let Some(rest) = prefix_rest(container_path, &m.destination) {
                let mut p = PathBuf::from(&m.source);
                if !rest.is_empty() {
                    p.push(rest);
                }
                return Some(p);
            }
        }
        None
    }

    /// Host path → container path. Longest matching host prefix wins.
    pub fn to_container(&self, host_path: &Path) -> Option<String> {
        let host_str = host_path.to_string_lossy();
        let mut best: Option<(usize, String)> = None;
        for m in &self.mounts {
            if let Some(rest) = prefix_rest(&host_str, &m.source) {
                let mapped = if rest.is_empty() {
                    m.destination.clone()
                } else {
                    format!("{}/{}", m.destination, rest)
                };
                if best.as_ref().is_none_or(|(l, _)| m.source.len() > *l) {
                    best = Some((m.source.len(), mapped));
                }
            }
        }
        best.map(|(_, s)| s)
    }
}

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

    /// Find a registered project by its container name. Used when the frontend
    /// explicitly passes a container name for a session spawn.
    pub async fn project_by_container(&self, name: &str) -> Option<DockerProject> {
        let projects = self.projects.lock().await;
        projects.iter().find(|p| p.container == name).cloned()
    }

    /// Returns a clone of the mapper of whichever project maps the given
    /// container path, or None if no project owns it. Used by fs commands
    /// (file tree DFS, watcher callback) to remap host paths back to container
    /// space synchronously after snapshotting the mapper.
    pub async fn mapper_for_container_path(&self, path: &str) -> Option<PathMapper> {
        let projects = self.projects.lock().await;
        projects
            .iter()
            .find(|p| p.mapper.to_host(path).is_some())
            .map(|p| p.mapper.clone())
    }
}

/// Host path string → container path string. Passthrough (returns `host`
/// unchanged) when there is no mapper or the host path is under no mount.
/// Shared by the file-tree DFS and the watcher callback.
pub fn remap_path_out(mapper: &Option<PathMapper>, host: &str) -> String {
    match mapper {
        Some(m) => m
            .to_container(Path::new(host))
            .unwrap_or_else(|| host.to_string()),
        None => host.to_string(),
    }
}

/// A ready-to-spawn `docker exec` invocation: program plus full argv.
#[derive(Debug)]
pub struct DockerExecSpec {
    pub program: String,
    pub args: Vec<String>,
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Build a `docker exec` spec that runs claude inside the container, reporting
/// its shell PID on stderr first so kill_session can target it directly.
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

/// Recognise the `__TOKENICODE_PID__<n>` marker emitted by build_docker_exec.
pub fn parse_pid_marker(line: &str) -> Option<u32> {
    line.trim().strip_prefix("__TOKENICODE_PID__")?.parse().ok()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn mapper() -> PathMapper {
        PathMapper::new(vec![
            MountEntry {
                source: "/Users/me/proj".into(),
                destination: "/workspace".into(),
            },
            MountEntry {
                source: "/Users/me/data".into(),
                destination: "/workspace/data".into(),
            },
        ])
    }

    #[test]
    fn to_host_maps_prefix() {
        assert_eq!(
            mapper().to_host("/workspace/src/a.rs").unwrap(),
            std::path::PathBuf::from("/Users/me/proj/src/a.rs")
        );
    }

    #[test]
    fn to_host_longest_prefix_wins() {
        // /workspace/data is the more specific nested mount; it must win over /workspace
        assert_eq!(
            mapper().to_host("/workspace/data/x").unwrap(),
            std::path::PathBuf::from("/Users/me/data/x")
        );
    }

    #[test]
    fn to_host_exact_mount_root() {
        assert_eq!(
            mapper().to_host("/workspace").unwrap(),
            std::path::PathBuf::from("/Users/me/proj")
        );
    }

    #[test]
    fn to_host_rejects_non_component_prefix() {
        // /workspace2 must not match the /workspace mount
        assert!(mapper().to_host("/workspace2/x").is_none());
    }

    #[test]
    fn to_host_unmounted_returns_none() {
        assert!(mapper().to_host("/etc/passwd").is_none());
    }

    #[test]
    fn to_container_roundtrip() {
        assert_eq!(
            mapper()
                .to_container(std::path::Path::new("/Users/me/proj/src/a.rs"))
                .unwrap(),
            "/workspace/src/a.rs"
        );
    }

    #[test]
    fn to_container_longest_prefix_wins() {
        assert_eq!(
            mapper()
                .to_container(std::path::Path::new("/Users/me/data/x"))
                .unwrap(),
            "/workspace/data/x"
        );
    }

    #[test]
    fn trailing_slash_normalized() {
        let m = PathMapper::new(vec![MountEntry {
            source: "/Users/me/proj/".into(),
            destination: "/workspace/".into(),
        }]);
        assert_eq!(
            m.to_host("/workspace/a").unwrap(),
            std::path::PathBuf::from("/Users/me/proj/a")
        );
    }

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

    #[test]
    fn remap_tree_paths_rewrites_prefix() {
        let mapper = PathMapper::new(vec![MountEntry {
            source: "/Users/me/proj".into(),
            destination: "/workspace".into(),
        }]);
        // remap_path_out: host path string → container path string (passthrough if unmapped)
        assert_eq!(
            remap_path_out(&Some(mapper.clone()), "/Users/me/proj/src"),
            "/workspace/src"
        );
        assert_eq!(remap_path_out(&None, "/tmp/x"), "/tmp/x");
    }

    #[test]
    fn remap_path_out_passes_through_unmapped_host() {
        let mapper = PathMapper::new(vec![MountEntry {
            source: "/Users/me/proj".into(),
            destination: "/workspace".into(),
        }]);
        // Host path outside any mount stays as-is even with a mapper present.
        assert_eq!(remap_path_out(&Some(mapper), "/tmp/x"), "/tmp/x");
    }

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
        // Empty env → script is the final arg (index 7): exec -i -w /w c sh -c <script>.
        // (Brief's literal `args[9]` assumed the one-env-var layout; corrected here.)
        assert!(spec.args[7].contains(r#"'it'\''s'"#));
    }

    #[test]
    fn pid_marker_parses() {
        assert_eq!(parse_pid_marker("__TOKENICODE_PID__1234"), Some(1234));
        assert_eq!(parse_pid_marker("random stderr"), None);
    }

    #[tokio::test]
    async fn project_by_container_finds_by_name() {
        let mgr = BackendManager::default();
        mgr.register(docker_proj()).await;
        assert_eq!(mgr.project_by_container("dev-box").await.unwrap().container, "dev-box");
        assert!(mgr.project_by_container("nope").await.is_none());
    }

    #[tokio::test]
    async fn mapper_for_container_path_returns_matching_project_mapper() {
        let mgr = BackendManager::default();
        mgr.register(docker_proj()).await;
        let m = mgr.mapper_for_container_path("/workspace/x").await;
        assert!(m.is_some());
        assert_eq!(
            m.unwrap().to_host("/workspace/x").unwrap(),
            std::path::PathBuf::from("/Users/me/proj/x")
        );
        assert!(mgr.mapper_for_container_path("/tmp/x").await.is_none());
    }
}
