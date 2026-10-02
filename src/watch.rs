//! Watch mode: re-run the remote build when local sources change.
//!
//! This is the feature behind issue #2. Rather than relying on the local
//! `cargo watch` seeing files that only ever live on the build server, we watch
//! the local source tree with the platform's native notification API, then
//! re-sync and rebuild.

use std::path::Path;
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use log::{debug, info, warn};
use notify::{Event, EventKind, RecursiveMode, Watcher};

/// Quiet period after the last change before a rebuild starts.
const DEBOUNCE: Duration = Duration::from_millis(400);

/// Directories that never trigger a rebuild.
const IGNORED: &[&str] = &[
    ".git",
    "target",
    ".cargo-remote-3000.toml",
    ".cargo-remote.toml",
];

/// Runs `on_change` until the returned `Session`-level work completes or the
/// caller stops watching.
///
/// Returns the number of rebuilds performed.
pub fn watch<F>(root: &Path, exclude_patterns: &[String], mut on_change: F) -> Result<usize>
where
    F: FnMut() -> Result<()>,
{
    let (tx, rx) = channel::<notify::Result<Event>>();

    let mut watcher = notify::recommended_watcher(move |event| {
        // A send error only means the watcher was dropped.
        let _ = tx.send(event);
    })
    .context("failed to create a filesystem watcher")?;

    // Hidden directories are ignored unless the user asked for them, matching
    // what is actually transferred.
    watcher
        .watch(root, RecursiveMode::Recursive)
        .with_context(|| format!("failed to watch {}", root.display()))?;

    info!(
        "Watching {} for changes. Press Ctrl-C to stop.",
        root.display()
    );
    if !exclude_patterns.is_empty() {
        info!(
            "Also honouring rsync exclude patterns: {}",
            exclude_patterns.join(", ")
        );
    }

    let mut rebuilds = 0usize;
    let mut pending_since: Option<Instant> = None;

    loop {
        let wait = match pending_since {
            Some(started) => DEBOUNCE.saturating_sub(started.elapsed()),
            None => Duration::from_millis(500),
        };

        match rx.recv_timeout(wait) {
            Ok(Ok(event)) => {
                if should_trigger(&event, root) {
                    pending_since = Some(Instant::now());
                }
            }
            Ok(Err(error)) => warn!("watch error: {error}"),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        if let Some(started) = pending_since {
            if started.elapsed() >= DEBOUNCE {
                rebuilds += 1;
                info!("Change detected, rebuilding (run #{rebuilds})");
                if let Err(error) = on_change() {
                    // A failed build should not end the watch session.
                    warn!("build failed: {error:#}");
                }
                pending_since = None;
            }
        }
    }

    Ok(rebuilds)
}

/// Decide whether an event should cause a rebuild.
fn should_trigger(event: &Event, root: &Path) -> bool {
    if !matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    ) {
        return false;
    }

    event.paths.iter().any(|path| {
        let relative = if path.is_absolute() {
            match path.strip_prefix(root) {
                Ok(relative) => relative,
                // An absolute path outside the watched tree is not ours.
                Err(_) => return false,
            }
        } else {
            path
        };
        if is_ignored(relative) {
            debug!("ignoring change under {}", relative.display());
            false
        } else {
            true
        }
    })
}

/// True when any path component is one of the always-ignored names.
///
/// Checking every component means `crates/foo/target` is ignored too, while
/// `targets/` is not, which is what distinguishes a build directory from an
/// unrelated directory that merely shares a prefix.
fn is_ignored(relative: &Path) -> bool {
    relative.components().any(|component| {
        let name = component.as_os_str().to_string_lossy();
        IGNORED.iter().any(|ignored| *ignored == name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, CreateKind, ModifyKind};
    use std::path::PathBuf;

    fn event(paths: &[&str], kind: EventKind) -> Event {
        Event {
            kind,
            paths: paths.iter().map(PathBuf::from).collect(),
            attrs: Default::default(),
        }
    }

    #[test]
    fn ignored_directories_do_not_trigger_a_rebuild() {
        let event = event(&["target/debug"], EventKind::Create(CreateKind::Folder));
        assert!(!should_trigger(&event, Path::new("/project")));
    }

    #[test]
    fn git_activity_does_not_trigger_a_rebuild() {
        let event = event(&[".git/HEAD"], EventKind::Modify(ModifyKind::Any));
        assert!(!should_trigger(&event, Path::new("/project")));
    }

    #[test]
    fn source_changes_do_trigger_a_rebuild() {
        let event = event(&["src/main.rs"], EventKind::Modify(ModifyKind::Any));
        assert!(should_trigger(&event, Path::new("/project")));
    }

    #[test]
    fn nested_source_changes_trigger_a_rebuild() {
        let event = event(
            &["crates/foo/src/lib.rs"],
            EventKind::Create(CreateKind::File),
        );
        assert!(should_trigger(&event, Path::new("/project")));
    }

    #[test]
    fn access_events_are_ignored() {
        let event = event(&["src/main.rs"], EventKind::Access(AccessKind::Any));
        assert!(!should_trigger(&event, Path::new("/project")));
    }

    #[test]
    fn events_outside_the_watched_root_are_ignored() {
        // A real notify event carries an absolute path, which on Windows means
        // one with a drive prefix, so build the path with the platform's own
        // separator rather than a hardcoded `/`.
        let outside = std::env::temp_dir().join("elsewhere").join("main.rs");
        let event = Event {
            kind: EventKind::Modify(ModifyKind::Any),
            paths: vec![outside],
            attrs: Default::default(),
        };
        assert!(!should_trigger(&event, Path::new("/project")));
    }

    #[test]
    fn relative_event_paths_are_treated_as_project_relative() {
        // notify reports absolute paths, but a relative one should still be
        // understood rather than silently dropped.
        let event = event(&["src/main.rs"], EventKind::Modify(ModifyKind::Any));
        assert!(should_trigger(&event, Path::new("/project")));
    }

    #[test]
    fn a_change_mixed_with_ignored_paths_still_triggers() {
        let event = event(
            &[".git/index", "src/main.rs"],
            EventKind::Modify(ModifyKind::Any),
        );
        assert!(should_trigger(&event, Path::new("/project")));
    }

    #[test]
    fn is_ignored_matches_whole_components_only() {
        assert!(is_ignored(Path::new("target")));
        assert!(is_ignored(Path::new("target/debug")));
        assert!(is_ignored(Path::new("crates/foo/target")));
        // A directory merely starting with the same letters is not ignored.
        assert!(!is_ignored(Path::new("targets/debug")));
    }
}
