//! Building and running the rsync/ssh commands that talk to the build server.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use log::{debug, info};

use crate::config::{shell_quote, Remote};

/// Everything needed to move sources up and artifacts back down.
pub struct Session<'a> {
    pub remote: &'a Remote,
    /// Local directory whose contents get uploaded.
    pub project_root: &'a Path,
    /// Local workspace root, may sit below `project_root`.
    pub workspace_root: &'a Path,
    /// Remote directory that mirrors `project_root`.
    pub build_root: String,
    /// Remote directory that mirrors `workspace_root`.
    pub remote_workspace_root: String,
    /// Remote directory the cargo command runs in.
    pub remote_crate_dir: String,
    /// Local directory cargo would put `target` in.
    pub local_target_dir: PathBuf,
    /// Remote counterpart of `local_target_dir`, when it can be derived.
    pub remote_target_dir: Option<String>,
    /// Patterns to keep out of the upload.
    pub upload_excludes: Vec<String>,
    pub transfer_hidden: bool,
    pub no_transfer_git: bool,
    pub verbose: bool,
}

impl Session<'_> {
    /// Upload the project sources.
    pub fn upload(&self) -> Result<()> {
        info!("Transferring sources to {}", self.remote.host);
        let mut cmd = self.rsync_command();
        cmd.arg("--delete");
        // Never upload a local target directory; the remote has its own.
        cmd.arg("--exclude").arg("target");

        if !self.transfer_hidden {
            cmd.arg("--exclude").arg(".*");
        }
        if self.no_transfer_git {
            // `.git` is hidden, so this only matters together with --transfer-hidden.
            cmd.arg("--exclude").arg(".git");
        }
        for pattern in &self.upload_excludes {
            cmd.arg("--exclude").arg(pattern);
        }

        // rsync runs the destination path through the remote shell, so the
        // directory has to exist before the transfer starts.
        cmd.arg("--rsync-path").arg(format!(
            "mkdir -p {} && rsync",
            shell_quote(&self.build_root)
        ));
        cmd.arg(format!("{}/", self.project_root.display()));
        cmd.arg(format!("{}:{}", self.remote.host, self.build_root));

        self.run(&mut cmd, "transfer sources to the build server")
    }

    /// Copy `relative` out of the remote target directory back to the local one.
    pub fn download_target(&self, relative: &str) -> Result<()> {
        let remote_target = self
            .remote_target_dir
            .clone()
            .unwrap_or_else(|| format!("{}/target", self.remote_crate_dir));

        // `deps/` and `build/` hold intermediates that are either regenerated
        // locally or large enough to dominate transfer time. See PR #24.
        // The patterns are unanchored so they match inside every profile
        // directory, which is where cargo puts them.
        let mut cmd = self.rsync_command();
        cmd.arg("--delete");
        cmd.arg("--exclude").arg("deps/");
        cmd.arg("--exclude").arg("build/");
        cmd.arg("--exclude").arg(".fingerprint/");
        cmd.arg("--exclude").arg("incremental/");

        // An empty `relative` means "the whole target directory". Only add the
        // separator when there is something to put after it.
        let relative = relative.trim_matches('/');
        let (remote_path, local_path) = if relative.is_empty() {
            (
                format!("{}:{}/", self.remote.host, remote_target),
                format!("{}/", self.local_target_dir.display()),
            )
        } else {
            (
                format!("{}:{}/{}/", self.remote.host, remote_target, relative),
                format!("{}/{}/", self.local_target_dir.display(), relative),
            )
        };
        cmd.arg(remote_path);
        cmd.arg(local_path);

        self.run(&mut cmd, "copy the target directory back")
    }

    /// Copy the workspace `Cargo.lock` back, if the remote produced one.
    pub fn download_lock_file(&self) -> Result<()> {
        info!("Transferring Cargo.lock back to the client");
        let mut cmd = self.rsync_command();
        // `--ignore-missing-args` keeps a lock-less project (a virtual workspace
        // manifest, for instance) from failing the whole run.
        cmd.arg("--ignore-missing-args");
        cmd.arg(format!(
            "{}:{}/Cargo.lock",
            self.remote.host, self.remote_workspace_root
        ));
        cmd.arg(format!("{}/Cargo.lock", self.workspace_root.display()));
        self.run(&mut cmd, "copy Cargo.lock back")
    }

    /// Run the cargo command on the build server.
    ///
    /// `build_env` holds `KEY=value` pairs, `command` is the cargo subcommand
    /// and `args` the arguments for it.
    pub fn run_remote_command(
        &self,
        build_env: &[String],
        toolchain: &str,
        command: &str,
        args: &[String],
    ) -> Result<std::process::ExitStatus> {
        let script = self.build_script(build_env, toolchain, command, args)?;
        info!("Starting remote build");

        let mut cmd = Command::new("ssh");
        cmd.args(self.remote.ssh_args());
        // Only ask for a pseudo terminal when we have one to give. With `-t`
        // ssh reports success even when the remote command failed, which hides
        // build errors from scripts and CI.
        if is_interactive_terminal() {
            cmd.arg("-t");
        }
        // Build output is on stderr already; ssh's own chatter would only get in the
        // way. Skip it when the user asked for a specific LogLevel.
        if !self
            .remote
            .ssh_options
            .iter()
            .any(|option| option.starts_with("LogLevel="))
        {
            cmd.arg("-o").arg("LogLevel=ERROR");
        }
        cmd.arg(&self.remote.host).arg(script);
        cmd.stdin(Stdio::inherit());
        cmd.stdout(Stdio::inherit());
        cmd.stderr(Stdio::inherit());

        if self.verbose {
            debug!("running: {cmd:?}");
        }

        cmd.status()
            .with_context(|| format!("failed to run `ssh` to {}", self.remote.host))
    }

    /// Compose the shell script executed on the build server.
    fn build_script(
        &self,
        build_env: &[String],
        toolchain: &str,
        command: &str,
        args: &[String],
    ) -> Result<String> {
        let mut script = String::new();

        // Issue #26: `source` is not POSIX, so on a dash/sh build host it fails
        // and the rest of the line runs without the environment set up. `.` is
        // portable, and the file is only sourced when it actually exists.
        for script_path in &self.remote.env {
            script.push_str(&format!(
                "[ -f {path} ] && . {path}; ",
                path = shell_quote(script_path)
            ));
        }

        if !toolchain.is_empty() {
            script.push_str(&format!(
                "rustup default {} >/dev/null; ",
                shell_quote(toolchain)
            ));
        }

        script.push_str(&format!("cd {}; ", shell_quote(&self.remote_crate_dir)));

        for assignment in build_env {
            validate_env_assignment(assignment)?;
            script.push_str(assignment);
            script.push(' ');
        }

        script.push_str(&format!("cargo {}", shell_quote(command)));
        for arg in args {
            script.push(' ');
            script.push_str(&shell_quote(arg));
        }

        Ok(script)
    }

    /// Base rsync invocation with flags shared by every transfer.
    fn rsync_command(&self) -> Command {
        let mut cmd = Command::new("rsync");
        // `-a` preserves permissions and times, `-P` keeps partial transfers
        // resumable and shows per-file progress. `-P` is deliberately used
        // instead of `--info=progress2`, which only exists in rsync >= 3.1 and
        // made the BSD rsync shipped with macOS fail outright (issue #2).
        cmd.arg("-aP").arg("--compress").arg("--safe-links");
        cmd.arg("-e").arg(self.remote.rsync_ssh_command());
        cmd.stdin(Stdio::inherit());
        cmd.stdout(Stdio::inherit());
        cmd.stderr(Stdio::inherit());
        cmd
    }

    /// Run an rsync command, turning its exit status into an error.
    fn run(&self, cmd: &mut Command, what: &str) -> Result<()> {
        if self.verbose {
            debug!("running: {cmd:?}");
        }
        let status = cmd
            .status()
            .with_context(|| format!("failed to run `rsync` to {what}"))?;
        if !status.success() {
            bail!(
                "failed to {what} (rsync exited with {})",
                status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".to_string())
            );
        }
        Ok(())
    }
}

/// Reject anything that is not a plain `KEY=value` assignment.
///
/// These values are interpolated into a shell script, so a stray quote or
/// `$(...)` would be command injection. Being strict here is the point.
fn validate_env_assignment(assignment: &str) -> Result<()> {
    let Some((key, _)) = assignment.split_once('=') else {
        bail!("invalid build environment entry {assignment:?}, expected KEY=value");
    };
    let valid = !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if !valid {
        bail!(
            "invalid build environment variable name {key:?}, \
             expected an uppercase-style identifier such as RUST_BACKTRACE"
        );
    }

    // The value is interpolated into the remote script unquoted, so it must
    // not contain anything the shell could interpret.
    let value = assignment
        .split_once('=')
        .map(|(_, value)| value)
        .unwrap_or("");
    let safe = value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-_./:,+@= ".contains(&b));
    if !safe {
        bail!(
            "unsupported value {value:?} for build environment variable {key:?}; \
             allowed characters are letters, digits and - _ . / : , + @ = and spaces"
        );
    }
    Ok(())
}

/// True when stdout is a terminal, which is when ssh should get a pty.
fn is_interactive_terminal() -> bool {
    #[cfg(unix)]
    {
        use std::io::IsTerminal;
        std::io::stdout().is_terminal()
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::RemoteOpts;
    use crate::config::Config;

    fn remote() -> Remote {
        Config::default()
            .get_remote(&RemoteOpts {
                host: Some("user@build-host".to_string()),
                ..Default::default()
            })
            .unwrap()
    }

    fn session(root: &Path) -> Session<'_> {
        let remote = Box::leak(Box::new(remote()));
        Session {
            remote,
            project_root: root,
            workspace_root: root,
            build_root: "/tmp/remote-builds/12345".to_string(),
            remote_workspace_root: "/tmp/remote-builds/12345".to_string(),
            remote_crate_dir: "/tmp/remote-builds/12345".to_string(),
            local_target_dir: root.join("target"),
            remote_target_dir: Some("/tmp/remote-builds/12345/target".to_string()),
            upload_excludes: vec![],
            transfer_hidden: false,
            no_transfer_git: false,
            verbose: false,
        }
    }

    #[test]
    fn env_assignments_must_look_like_shell_variables() {
        assert!(validate_env_assignment("RUST_BACKTRACE=1").is_ok());
        assert!(validate_env_assignment("CC=clang").is_ok());
        assert!(validate_env_assignment("RUST_BACKTRACE").is_err());
        assert!(validate_env_assignment("=1").is_err());
        assert!(validate_env_assignment("A B=1").is_err());
        // Command injection attempts are rejected rather than escaped.
        assert!(validate_env_assignment("A=$(id)").is_err());
        assert!(validate_env_assignment("A='x'").is_err());
    }

    #[test]
    fn build_script_sources_env_posix_style() {
        // Issue #26: `source` is a bashism and silently did nothing on sh hosts.
        let dir = tempfile::tempdir().unwrap();
        let s = session(dir.path());
        let script = s
            .build_script(
                &["RUST_BACKTRACE=1".to_string()],
                "stable",
                "build",
                &["--release".to_string()],
            )
            .unwrap();
        assert!(
            script.contains("[ -f ~/.cargo/env ] && . ~/.cargo/env;"),
            "{script}"
        );
        assert!(!script.contains(" source "), "{script}");
        assert!(script.contains("cd /tmp/remote-builds/12345;"), "{script}");
        assert!(script.ends_with("cargo build --release"), "{script}");
    }

    #[test]
    fn build_script_skips_missing_env_files() {
        let dir = tempfile::tempdir().unwrap();
        let s = session(dir.path());
        let script = s.build_script(&[], "", "check", &[]).unwrap();
        assert!(
            script.contains("[ -f ~/.cargo/env ] && . ~/.cargo/env;"),
            "{script}"
        );
        // An empty toolchain means "use whatever the remote default is".
        assert!(!script.contains("rustup"), "{script}");
    }

    #[test]
    fn build_script_quotes_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let s = session(dir.path());
        let script = s
            .build_script(&[], "nightly", "test", &["--some flag".to_string()])
            .unwrap();
        assert!(script.contains("rustup default nightly"), "{script}");
        assert!(script.ends_with("cargo test '--some flag'"), "{script}");
    }

    #[test]
    fn build_script_rejects_bad_env() {
        let dir = tempfile::tempdir().unwrap();
        let s = session(dir.path());
        assert!(s
            .build_script(&["not-an-assignment".to_string()], "stable", "build", &[])
            .is_err());
    }
}
