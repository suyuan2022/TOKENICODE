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
}
