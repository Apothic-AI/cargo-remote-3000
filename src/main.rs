//! cargo-remote-3000: run `cargo` on a remote build server and bring the
//! artifacts home.

mod cli;
mod config;
mod project;
mod remote;
mod watch;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::process::ExitCode;

use anyhow::{bail, Result};
use clap::Parser;
use log::{debug, info};

use cli::{Cli, Command, Remote as RemoteArgs};
use config::Config;
use project::Project;
use remote::Session;

/// Exit codes. A failing remote build propagates its own status instead.
mod exit {
    pub const USAGE: u8 = 2;
    pub const SETUP: u8 = 3;
    pub const TRANSFER: u8 = 4;
}

/// An error plus the exit code it should produce.
struct Failure {
    code: u8,
    error: anyhow::Error,
}

impl Failure {
    fn new(code: u8, error: anyhow::Error) -> Self {
        Self { code, error }
    }
}

impl From<Failure> for ExitCode {
    fn from(value: Failure) -> Self {
        ExitCode::from(value.code)
    }
}

type Outcome = std::result::Result<ExitCode, Failure>;

fn main() -> ExitCode {
    init_logging();

    match run() {
        Ok(code) => code,
        Err(failure) => {
            // `{:#}` prints the whole context chain, which is what makes a
            // nested transfer error understandable.
            eprintln!("error: {:#}", failure.error);
            ExitCode::from(failure.code)
        }
    }
}

/// Set up logging before arguments are parsed.
///
/// `RUST_LOG` wins; otherwise `-v` on the command line raises the level to
/// debug, which is what prints each rsync and ssh invocation.
fn init_logging() {
    let verbose = std::env::args().any(|arg| arg == "-v" || arg == "--verbose");
    let level = match std::env::var("RUST_LOG").ok().as_deref() {
        Some("trace") | Some("debug") => log::LevelFilter::Debug,
        Some("warn") => log::LevelFilter::Warn,
        Some("error") => log::LevelFilter::Error,
        _ if verbose => log::LevelFilter::Debug,
        _ => log::LevelFilter::Info,
    };
    let _ = env_logger::Builder::new().filter_level(level).try_init();
}

fn run() -> Outcome {
    let Command::Remote(args) = Cli::parse().command;

    if args.verbose > 0 {
        debug!("parsed arguments: {args:?}");
    }

    let project = Project::resolve(&args).map_err(|error| Failure::new(exit::SETUP, error))?;

    let conf =
        Config::load(project.project_root()).map_err(|error| Failure::new(exit::SETUP, error))?;
    let remote = conf
        .get_remote(&args.remote)
        .map_err(|error| Failure::new(exit::SETUP, error))?;

    let build_root = remote_build_root(&remote.temp_dir, project.project_root());
    let session = Session {
        remote: &remote,
        project_root: project.project_root(),
        workspace_root: project.workspace_root(),
        build_root: build_root.clone(),
        remote_workspace_root: join_remote(&build_root, project.workspace_relative()),
        remote_crate_dir: join_remote(&build_root, project.crate_relative()),
        local_target_dir: project.local_target_dir().to_path_buf(),
        remote_target_dir: project.remote_target_dir(&build_root),
        upload_excludes: remote.exclude.clone(),
        transfer_hidden: args.transfer_hidden,
        no_transfer_git: args.no_transfer_git,
        verbose: args.verbose > 0,
    };

    info!("Build path: {build_root}");
    info!("Remote crate directory: {}", session.remote_crate_dir);

    if args.watch {
        return run_watch(&args, &session);
    }

    session
        .upload()
        .map_err(|error| Failure::new(exit::TRANSFER, error))?;

    let status = session
        .run_remote_command(
            &args.build_env,
            &args.rustup_default,
            &args.command,
            &args.args,
        )
        .map_err(|error| Failure::new(exit::TRANSFER, error))?;

    copy_back(&args, &session).map_err(|error| Failure::new(exit::TRANSFER, error))?;

    if !status.success() {
        info!("Remote build exited with status {status}");
        // Mirror the remote status so CI and shell scripts see the truth.
        let code = status
            .code()
            .and_then(|c| u8::try_from(c).ok())
            .unwrap_or(exit::SETUP);
        return Ok(ExitCode::from(code));
    }

    Ok(ExitCode::SUCCESS)
}

/// Copy artifacts and the lock file home.
fn copy_back(args: &RemoteArgs, session: &Session) -> Result<()> {
    if let Some(relative) = &args.copy_back {
        let relative = relative.as_deref().unwrap_or("");
        info!(
            "Copying target/{relative} back to {}",
            session.local_target_dir.display()
        );
        session.download_target(relative)?;
    }

    if !args.no_copy_lock {
        // A missing lock file is not a reason to fail the build.
        if let Err(error) = session.download_lock_file() {
            debug!("skipping Cargo.lock transfer: {error:#}");
        }
    }

    Ok(())
}

/// Watch mode: one build, then a rebuild on every source change.
fn run_watch(args: &RemoteArgs, session: &Session) -> Outcome {
    if args.copy_back.is_some() {
        return Err(Failure::new(
            exit::USAGE,
            anyhow::anyhow!(
                "--watch rebuilds continuously and the artifacts stay on the build \
                 server, so --copy-back is not supported in watch mode"
            ),
        ));
    }

    let run_build = || -> Result<()> {
        session.upload()?;
        let status = session.run_remote_command(
            &args.build_env,
            &args.rustup_default,
            &args.command,
            &args.args,
        )?;
        if !status.success() {
            bail!("remote build failed with {status}");
        }
        Ok(())
    };

    if let Err(error) = run_build() {
        // A first build failure is worth reporting loudly.
        return Err(Failure::new(exit::TRANSFER, error));
    }

    match watch::watch(session.project_root, &session.upload_excludes, run_build) {
        Ok(rebuilds) => {
            info!("Watch session ended after {rebuilds} rebuilds");
            Ok(ExitCode::SUCCESS)
        }
        Err(error) => Err(Failure::new(exit::SETUP, error)),
    }
}

/// Stable per-project build directory on the build server.
///
/// Hashing the local path means two checkouts of the same project on different
/// machines still land in one remote directory, and re-running after a reboot
/// reuses the previous target directory instead of rebuilding from scratch.
fn remote_build_root(temp_dir: &str, project_root: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    project_root.hash(&mut hasher);
    // A leading `~` is left for the remote shell to expand: the build host's
    // home directory has nothing to do with the local one.
    let temp_dir = temp_dir.trim_end_matches('/');
    format!("{temp_dir}/{:016x}", hasher.finish())
}

/// Join a remote path fragment onto a remote base directory.
fn join_remote(base: &str, relative: &Path) -> String {
    if relative.as_os_str().is_empty() {
        return base.to_string();
    }
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        relative.to_string_lossy()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_root_is_stable_for_the_same_path() {
        let a = remote_build_root("~/remote-builds", Path::new("/home/me/project"));
        let b = remote_build_root("~/remote-builds", Path::new("/home/me/project"));
        assert_eq!(a, b);
        assert!(a.ends_with(|c: char| c.is_ascii_hexdigit()), "{a}");
    }

    #[test]
    fn build_root_differs_per_project() {
        let a = remote_build_root("~/remote-builds", Path::new("/home/me/one"));
        let b = remote_build_root("~/remote-builds", Path::new("/home/me/two"));
        assert_ne!(a, b);
    }

    #[test]
    fn build_root_normalises_the_temp_dir() {
        let with_slash = remote_build_root("~/remote-builds/", Path::new("/p"));
        let without = remote_build_root("~/remote-builds", Path::new("/p"));
        assert_eq!(with_slash, without);
        assert!(!with_slash.contains("//"), "{with_slash}");
    }

    #[test]
    fn build_root_leaves_the_tilde_for_the_remote_shell() {
        // The build directory lives on the build server, so expanding `~` with
        // the *local* home directory would point at the wrong place.
        let root = remote_build_root("~/remote-builds", Path::new("/p"));
        assert!(root.starts_with("~/remote-builds/"), "{root}");
    }

    #[test]
    fn join_remote_handles_the_workspace_root_itself() {
        assert_eq!(join_remote("/remote/abc", Path::new("")), "/remote/abc");
        assert_eq!(
            join_remote("/remote/abc", Path::new("crates/foo")),
            "/remote/abc/crates/foo"
        );
    }
}
