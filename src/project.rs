//! Locating the project being built.
//!
//! Three directories matter and they are easy to confuse:
//!
//! * `project_root` — the directory whose contents get uploaded. Usually the
//!   workspace root, but issue #12 needed a way to upload a larger tree so that
//!   path dependencies outside the workspace come along too.
//! * `workspace_root` — what cargo considers the workspace root. This is where
//!   the shared `Cargo.lock` and `target/` live.
//! * `crate_relative` — where the actual crate being built sits relative to
//!   `project_root`, so the remote cargo invocation runs in the right place.
//!
//! Issue #14 is about the target directory: in a workspace, `target/` is at the
//! workspace root, not next to the crate. Getting this wrong means artifacts
//! are copied back into a directory cargo never reads.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use log::debug;

use crate::cli::Remote;

pub struct Project {
    project_root: PathBuf,
    workspace_root: PathBuf,
    crate_dir: PathBuf,
    target_dir: PathBuf,
}

impl Project {
    /// Inspect the manifest and work out all the directories involved.
    pub fn resolve(args: &Remote) -> Result<Self> {
        let manifest_path = absolute(&args.manifest_path)?;

        let mut metadata_cmd = cargo_metadata::MetadataCommand::new();
        metadata_cmd.manifest_path(&manifest_path).no_deps();
        debug!("running cargo metadata for {}", manifest_path.display());

        let metadata = metadata_cmd
            .exec()
            .with_context(|| format!("failed to read {}", manifest_path.display()))?;

        // cargo_metadata hands back `camino::Utf8PathBuf`; everything else in
        // this crate works with `std::path`.
        let workspace_root = to_path_buf(&metadata.workspace_root);
        let target_dir = to_path_buf(&metadata.target_directory);
        let crate_dir = manifest_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| workspace_root.clone());

        // Issue #12: `--working-directory` uploads a larger tree so that path
        // dependencies and git submodules outside the workspace are included.
        let project_root = match &args.working_directory {
            Some(dir) => {
                let dir = absolute(dir).with_context(|| {
                    format!("working directory {} does not exist", dir.display())
                })?;
                if !dir.is_dir() {
                    bail!("working directory {} is not a directory", dir.display());
                }
                // The upload has to contain the whole workspace, so the working
                // directory must be the same as, or above, the workspace root.
                if !workspace_root.starts_with(&dir) {
                    bail!(
                        "working directory {} must be an ancestor of the workspace root {}",
                        dir.display(),
                        workspace_root.display()
                    );
                }
                dir
            }
            None => workspace_root.clone(),
        };

        debug!("project root: {}", project_root.display());
        debug!("workspace root: {}", workspace_root.display());
        debug!("crate directory: {}", crate_dir.display());
        debug!("local target directory: {}", target_dir.display());

        Ok(Project {
            project_root,
            workspace_root,
            crate_dir,
            target_dir,
        })
    }

    /// The directory whose contents are uploaded.
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// Cargo's workspace root, where `Cargo.lock` and `target/` live.
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// The local directory cargo would write `target/` into.
    ///
    /// This follows `CARGO_TARGET_DIR` and the manifest's `build.target-dir`,
    /// which `cargo metadata` already reports, so a workspace with a custom
    /// target directory works without extra configuration.
    pub fn local_target_dir(&self) -> &Path {
        &self.target_dir
    }

    /// Workspace root relative to the uploaded tree.
    pub fn workspace_relative(&self) -> &Path {
        relative_to(&self.workspace_root, &self.project_root)
    }

    /// The crate directory relative to the uploaded tree.
    pub fn crate_relative(&self) -> &Path {
        relative_to(&self.crate_dir, &self.project_root)
    }

    /// Where the remote target directory sits, given the remote project root.
    ///
    /// Only meaningful when the local target directory is inside the uploaded
    /// tree. A `CARGO_TARGET_DIR` pointing outside the project, or
    /// `--working-directory` narrower than the workspace, yields `None` and the
    /// caller falls back to `<crate>/target`.
    pub fn remote_target_dir(&self, remote_project_root: &str) -> Option<String> {
        let relative = self.target_dir.strip_prefix(&self.project_root).ok()?;
        let mut path = remote_project_root.trim_end_matches('/').to_string();
        if !relative.as_os_str().is_empty() {
            path.push('/');
            path.push_str(&relative.to_string_lossy());
        }
        Some(path)
    }
}

/// Convert a `cargo_metadata` UTF-8 path into a `std::path::PathBuf`.
fn to_path_buf(path: &camino::Utf8Path) -> PathBuf {
    PathBuf::from(path.as_str())
}

/// Make a path absolute without requiring it to exist.
fn absolute(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(normalize(path));
    }
    let cwd = std::env::current_dir().context("failed to read the current directory")?;
    Ok(normalize(&cwd.join(path)))
}

/// Collapse `.` and `..` lexically, which `canonicalize` cannot do for a path
/// that does not exist yet.
fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Only pop a real directory name, never a root or a prefix.
                let popped = match out.components().next_back() {
                    Some(Component::Normal(_)) => out.pop(),
                    _ => false,
                };
                if !popped {
                    out.push(component);
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// `child` expressed relative to `base`.
fn relative_to<'a>(child: &'a Path, base: &Path) -> &'a Path {
    child.strip_prefix(base).unwrap_or(Path::new(""))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(target: &str) -> Project {
        Project {
            project_root: PathBuf::from("/home/me/repo"),
            workspace_root: PathBuf::from("/home/me/repo"),
            crate_dir: PathBuf::from("/home/me/repo"),
            target_dir: PathBuf::from(target),
        }
    }

    #[test]
    fn normalize_collapses_dot_segments() {
        assert_eq!(normalize(Path::new("/a/./b")), PathBuf::from("/a/b"));
        assert_eq!(normalize(Path::new("/a/b/../c")), PathBuf::from("/a/c"));
        assert_eq!(normalize(Path::new("/a/b/../../c")), PathBuf::from("/c"));
    }

    #[test]
    fn normalize_keeps_leading_slash() {
        assert!(normalize(Path::new("/../a")).starts_with("/"));
    }

    #[test]
    fn single_crate_layout_uses_the_workspace_root() {
        let p = project("/home/me/repo/target");
        assert_eq!(p.workspace_relative(), Path::new(""));
        assert_eq!(p.crate_relative(), Path::new(""));
        assert_eq!(
            p.remote_target_dir("/remote/abc"),
            Some("/remote/abc/target".to_string())
        );
    }

    #[test]
    fn workspace_crate_gets_its_own_remote_directory() {
        // Issue #14: `target/` is at the workspace root, the crate is not.
        let mut p = project("/home/me/repo/target");
        p.crate_dir = PathBuf::from("/home/me/repo/crates/foo");
        assert_eq!(p.crate_relative(), Path::new("crates/foo"));
        assert_eq!(p.workspace_relative(), Path::new(""));
        assert_eq!(
            p.remote_target_dir("/remote/abc"),
            Some("/remote/abc/target".to_string())
        );
    }

    #[test]
    fn custom_target_dir_is_honoured() {
        // Issue #14: `build.target-dir` in .cargo/config.toml.
        let p = project("/home/me/repo/build/out");
        assert_eq!(
            p.remote_target_dir("/remote/abc"),
            Some("/remote/abc/build/out".to_string())
        );
    }

    #[test]
    fn working_directory_above_the_workspace_shifts_both() {
        // Issue #12: uploading the parent directory so `../dep` is included.
        let mut p = project("/home/me/tree/main/target");
        p.project_root = PathBuf::from("/home/me/tree");
        p.workspace_root = PathBuf::from("/home/me/tree/main");
        p.crate_dir = PathBuf::from("/home/me/tree/main");
        assert_eq!(p.project_root(), Path::new("/home/me/tree"));
        assert_eq!(p.workspace_relative(), Path::new("main"));
        assert_eq!(p.crate_relative(), Path::new("main"));
        assert_eq!(
            p.remote_target_dir("/remote/abc"),
            Some("/remote/abc/main/target".to_string())
        );
    }

    #[test]
    fn target_dir_outside_the_uploaded_tree_has_no_remote_path() {
        let mut p = project("/elsewhere/target");
        p.project_root = PathBuf::from("/home/me/repo");
        assert_eq!(p.remote_target_dir("/remote/abc"), None);
    }

    #[test]
    fn relative_to_falls_back_to_empty() {
        assert_eq!(
            relative_to(Path::new("/a/b"), Path::new("/c")),
            Path::new("")
        );
    }
}
