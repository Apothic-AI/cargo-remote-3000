//! End-to-end tests.
//!
//! These run the real binary against a fake `rsync` and `ssh` placed first on
//! `PATH`. The fakes record their argv to a log file and exit successfully,
//! which lets the tests assert on exactly what would be transferred and run on
//! the remote without needing a build server.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// Set up a fake rsync/ssh pair and return the log file they append to.
struct Harness {
    _bin: TempDir,
    _project: TempDir,
    log: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let bin = tempfile::tempdir().unwrap();
        let log = bin.path().join("commands.log");

        // Both fakes append one JSON-ish line per invocation. `cat` is a
        // dependency-free way to serialise concurrent appends on Linux.
        for (name, exit_code) in [("rsync", 0), ("ssh", 0)] {
            let script = format!(
                r#"#!/bin/sh
printf '%s' "{name}" >> "{log}"
for arg in "$@"; do
    printf ' %s' "$arg" >> "{log}"
done
printf '\n' >> "{log}"
exit {exit_code}
"#,
                log = log.display(),
            );
            let path = bin.path().join(name);
            std::fs::write(&path, script).unwrap();
            make_executable(&path);
        }

        let project = tempfile::tempdir().unwrap();
        write_crate(project.path());

        Harness {
            _bin: bin,
            _project: project,
            log,
        }
    }

    fn project_dir(&self) -> &Path {
        self._project.path()
    }

    /// Run cargo-remote-3000 with the fake transport on PATH.
    fn run(&self, args: &[&str]) -> std::process::Output {
        self.command(args)
            .output()
            .expect("failed to run cargo-remote-3000")
    }

    /// Stderr from a run with debug logging enabled.
    fn stderr(&self, args: &[&str]) -> String {
        let output = self
            .command(args)
            .env("RUST_LOG", "debug")
            .output()
            .expect("failed to run cargo-remote-3000");
        String::from_utf8_lossy(&output.stderr).into_owned()
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cargo-remote-3000"));
        cmd.arg("remote-3000")
            .args(args)
            .current_dir(self.project_dir())
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self._bin.path().display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            // Keep a developer's real user config out of the test, whichever
            // platform's config lookup is in play.
            .env("XDG_CONFIG_HOME", self._bin.path())
            .env("HOME", self._bin.path())
            .env("APPDATA", self._bin.path());
        cmd
    }

    /// Every rsync/ssh invocation, one entry per line.
    fn commands(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The argument string of the first ssh invocation.
    fn ssh_command(&self) -> String {
        self.commands()
            .into_iter()
            .find(|line| line.starts_with("ssh "))
            .expect("expected an ssh invocation")
    }

    /// The argument string of the nth rsync invocation.
    fn rsync_command(&self, index: usize) -> String {
        self.commands()
            .into_iter()
            .filter(|line| line.starts_with("rsync "))
            .nth(index)
            .unwrap_or_else(|| panic!("expected at least {} rsync invocations", index + 1))
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

fn write_crate(dir: &Path) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn demo() {}\n").unwrap();
}

/// The remote path an invocation refers to, with the `host:` prefix stripped.
fn remote_dir(invocation: &str) -> String {
    let path = invocation
        .split_whitespace()
        .find(|word| word.contains("/remote-builds/"))
        .unwrap_or_else(|| panic!("expected a remote path in {invocation}"));
    let path = path.rsplit(':').next().unwrap_or(path);
    path.trim_end_matches([';', '\'', '"']).to_string()
}

fn assert_success(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "command failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn builds_with_a_host_on_the_command_line() {
    let harness = Harness::new();
    let output = harness.run(&["-H", "user@build-host", "build"]);
    assert_success(&output);

    let ssh = harness.ssh_command();
    assert!(ssh.contains(" cargo build"), "{ssh}");
    // Issue #26: the environment script is sourced with POSIX `.`, not `source`.
    assert!(
        ssh.contains("[ -f ~/.cargo/env ] && . ~/.cargo/env"),
        "{ssh}"
    );
}

#[test]
fn remote_flags_after_the_command_are_passed_through() {
    // Issue #19: no `--` separator required.
    let harness = Harness::new();
    assert_success(&harness.run(&[
        "-H",
        "user@build-host",
        "build",
        "--message-format",
        "human",
    ]));
    assert!(harness
        .ssh_command()
        .contains("cargo build --message-format human"));
}

#[test]
fn a_double_dash_separator_still_works() {
    let harness = Harness::new();
    assert_success(&harness.run(&["-H", "user@build-host", "build", "--", "--release"]));
    let ssh = harness.ssh_command();
    assert!(ssh.contains("cargo build --release"), "{ssh}");
    assert!(!ssh.contains("-- --release"), "{ssh}");
}

#[test]
fn the_ssh_port_and_identity_file_reach_both_transports() {
    // Issue #15 and #22.
    let harness = Harness::new();
    assert_success(&harness.run(&[
        "-H",
        "user@build-host",
        "-p",
        "2222",
        "-i",
        "/keys/build_ed25519",
        "build",
    ]));

    let ssh = harness.ssh_command();
    assert!(ssh.contains(" -p 2222 -i /keys/build_ed25519 "), "{ssh}");

    // rsync shells out to ssh, so it has to get the same options.
    let rsync = harness.rsync_command(0);
    assert!(
        rsync.contains("ssh -p 2222 -i /keys/build_ed25519"),
        "{rsync}"
    );
}

#[test]
fn sources_are_uploaded_with_the_expected_excludes() {
    let harness = Harness::new();
    assert_success(&harness.run(&["-H", "user@build-host", "-x", "assets/", "build"]));

    let rsync = harness.rsync_command(0);
    assert!(rsync.contains("--exclude target"), "{rsync}");
    // Hidden files are skipped unless asked for (issue #4 related).
    assert!(rsync.contains("--exclude .*"), "{rsync}");
    // Issue #4: user supplied exclude patterns are forwarded.
    assert!(rsync.contains("--exclude assets/"), "{rsync}");
}

#[test]
fn transfer_hidden_opts_in_to_dot_files() {
    let harness = Harness::new();
    assert_success(&harness.run(&["-H", "user@build-host", "--transfer-hidden", "build"]));
    let rsync = harness.rsync_command(0);
    assert!(!rsync.contains("--exclude .*"), "{rsync}");
}

#[test]
fn no_transfer_git_keeps_git_home_when_hidden_files_are_sent() {
    // Behaviour merged from PR #25.
    let harness = Harness::new();
    assert_success(&harness.run(&[
        "-H",
        "user@build-host",
        "--transfer-hidden",
        "--no-transfer-git",
        "build",
    ]));
    let rsync = harness.rsync_command(0);
    assert!(rsync.contains("--exclude .git"), "{rsync}");
    assert!(!rsync.contains("--exclude .*"), "{rsync}");
}

#[test]
fn copy_back_skips_the_intermediate_directories() {
    // Behaviour merged from PR #24.
    let harness = Harness::new();
    assert_success(&harness.run(&["-H", "user@build-host", "-c", "build"]));

    let rsync = harness.rsync_command(1);
    assert!(rsync.contains("--exclude deps/"), "{rsync}");
    assert!(rsync.contains("--exclude build/"), "{rsync}");
    assert!(rsync.contains("/target/"), "{rsync}");
}

#[test]
fn copy_back_can_select_a_single_artifact() {
    let harness = Harness::new();
    assert_success(&harness.run(&[
        "-H",
        "user@build-host",
        "-c=release/demo",
        "build",
        "--release",
    ]));
    let rsync = harness.rsync_command(1);
    assert!(rsync.contains("/target/release/demo"), "{rsync}");
}

#[test]
fn no_copy_lock_suppresses_the_lock_file_transfer() {
    let harness = Harness::new();
    assert_success(&harness.run(&["-H", "user@build-host", "--no-copy-lock", "build"]));

    let commands = harness.commands();
    assert_eq!(
        commands
            .iter()
            .filter(|line| line.starts_with("rsync "))
            .count(),
        1,
        "expected only the upload, got: {commands:#?}"
    );
}

#[test]
fn the_lock_file_is_fetched_by_default() {
    let harness = Harness::new();
    assert_success(&harness.run(&["-H", "user@build-host", "build"]));

    let rsync = harness.rsync_command(1);
    assert!(rsync.contains("Cargo.lock"), "{rsync}");
    assert!(rsync.contains("--ignore-missing-args"), "{rsync}");
}

#[test]
fn the_toolchain_and_build_environment_are_applied_remotely() {
    let harness = Harness::new();
    assert_success(&harness.run(&[
        "-H",
        "user@build-host",
        "-d",
        "nightly",
        "-b",
        "RUST_BACKTRACE=full",
        "build",
    ]));

    let ssh = harness.ssh_command();
    assert!(ssh.contains("rustup default nightly"), "{ssh}");
    assert!(ssh.contains("RUST_BACKTRACE=full cargo build"), "{ssh}");
}

#[test]
fn multiple_env_scripts_are_sourced_in_order() {
    let harness = Harness::new();
    assert_success(&harness.run(&[
        "-H",
        "user@build-host",
        "-e",
        "/etc/profile",
        "-e",
        "~/.cargo/env",
        "build",
    ]));

    let ssh = harness.ssh_command();
    let profile = ssh.find("/etc/profile").expect("profile should be sourced");
    let cargo_env = ssh
        .find("~/.cargo/env")
        .expect("cargo env should be sourced");
    assert!(profile < cargo_env, "{ssh}");
}

#[test]
fn a_configured_remote_is_used_without_a_host_flag() {
    let harness = Harness::new();
    std::fs::write(
        harness.project_dir().join(".cargo-remote-3000.toml"),
        "[[remote]]\nname = \"rack\"\nhost = \"me@rack\"\nssh_port = 2200\n",
    )
    .unwrap();

    assert_success(&harness.run(&["-r", "rack", "build"]));
    assert!(harness.ssh_command().contains("me@rack"));
    assert!(harness.ssh_command().contains(" -p 2200 "));
}

#[test]
fn a_single_configured_remote_needs_no_name() {
    let harness = Harness::new();
    std::fs::write(
        harness.project_dir().join(".cargo-remote-3000.toml"),
        "[[remote]]\nhost = \"me@solo\"\n",
    )
    .unwrap();

    assert_success(&harness.run(&["build"]));
    assert!(harness.ssh_command().contains("me@solo"));
}

#[test]
fn the_legacy_config_file_name_still_works() {
    let harness = Harness::new();
    std::fs::write(
        harness.project_dir().join(".cargo-remote.toml"),
        "[[remote]]\nhost = \"me@legacy\"\n",
    )
    .unwrap();

    assert_success(&harness.run(&["build"]));
    assert!(harness.ssh_command().contains("me@legacy"));
}

#[test]
fn a_missing_remote_is_reported_with_guidance() {
    let harness = Harness::new();
    let output = harness.run(&["build"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no remote build server configured"),
        "{stderr}"
    );
    assert!(stderr.contains(".cargo-remote-3000.toml"), "{stderr}");
}

#[test]
fn an_unknown_remote_name_is_rejected() {
    let harness = Harness::new();
    std::fs::write(
        harness.project_dir().join(".cargo-remote-3000.toml"),
        "[[remote]]\nname = \"rack\"\nhost = \"me@rack\"\n",
    )
    .unwrap();

    let output = harness.run(&["-r", "nope", "build"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no remote named `nope`"), "{stderr}");
    assert!(stderr.contains("rack"), "{stderr}");
}

#[test]
fn shell_metacharacters_in_build_env_are_rejected() {
    // These values land in a remote shell script, so they must not be able to
    // smuggle in a command.
    let harness = Harness::new();
    let output = harness.run(&["-H", "user@build-host", "-b", "RUST_LOG=$(id)", "build"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unsupported value"), "{stderr}");
}

#[test]
fn the_remote_build_exit_code_is_propagated() {
    let bin = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    write_crate(project.path());

    // An ssh that fails the way a failing cargo build would.
    let log = bin.path().join("commands.log");
    let script = format!(
        r#"#!/bin/sh
printf 'ssh\n' >> "{log}"
exit 101
"#,
        log = log.display()
    );
    let ssh = bin.path().join("ssh");
    std::fs::write(&ssh, script).unwrap();
    make_executable(&ssh);
    let rsync = bin.path().join("rsync");
    std::fs::write(
        &rsync,
        format!(
            "#!/bin/sh\nprintf 'rsync\\n' >> \"{log}\"\nexit 0\n",
            log = log.display()
        ),
    )
    .unwrap();
    make_executable(&rsync);

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-remote-3000"))
        .args(["remote-3000", "-H", "user@build-host", "build"])
        .current_dir(project.path())
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.path().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("XDG_CONFIG_HOME", bin.path())
        .env("HOME", bin.path())
        .env("APPDATA", bin.path())
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(101),
        "remote exit code should reach the caller"
    );
}

#[test]
fn the_configured_target_directory_is_honoured() {
    let harness = Harness::new();
    std::fs::create_dir_all(harness.project_dir().join(".cargo")).unwrap();
    std::fs::write(
        harness.project_dir().join(".cargo/config.toml"),
        "[build]\ntarget-dir = \"build/out\"\n",
    )
    .unwrap();

    assert_success(&harness.run(&["-H", "user@build-host", "-c", "build"]));
    // cargo puts target in build/out, so that is where artifacts must be
    // fetched from and written to (issue #14).
    let rsync = harness.rsync_command(1);
    assert!(remote_dir(&rsync).ends_with("/build/out/"), "{rsync}");
}

#[test]
fn a_workspace_build_targets_the_workspace_root_target_directory() {
    // Issue #14: `target/` lives at the workspace root, not beside the crate.
    let harness = Harness::new();
    let workspace = harness.project_dir().join("nested/workspace");
    std::fs::create_dir_all(workspace.join("crates/app/src")).unwrap();
    std::fs::write(
        workspace.join("Cargo.toml"),
        "[workspace]\nresolver = \"2\"\nmembers = [\"crates/app\"]\n",
    )
    .unwrap();
    std::fs::write(
        workspace.join("crates/app/Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(workspace.join("crates/app/src/lib.rs"), "pub fn app() {}\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-remote-3000"))
        .args([
            "remote-3000",
            "-H",
            "user@build-host",
            "--manifest-path",
            "nested/workspace/crates/app/Cargo.toml",
            "build",
        ])
        .current_dir(harness.project_dir())
        .env(
            "PATH",
            format!(
                "{}:{}",
                harness._bin.path().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("XDG_CONFIG_HOME", harness._bin.path())
        .env("HOME", harness._bin.path())
        .env("APPDATA", harness._bin.path())
        .output()
        .unwrap();
    assert_success(&output);

    let commands = harness.commands();
    let ssh = commands.iter().find(|l| l.starts_with("ssh ")).unwrap();

    // Only the workspace root is uploaded, so the crate directory mirrors to
    // `<build dir>/crates/app` rather than keeping the local `nested/` prefix.
    // Only the workspace root is uploaded, so the crate directory mirrors to
    // `<build dir>/crates/app` rather than keeping the local `nested/` prefix.
    let dir = remote_dir(ssh);
    assert!(
        dir.ends_with("/crates/app"),
        "cargo should run in the crate dir, got {dir}"
    );

    let rsync = commands
        .iter()
        .filter(|l| l.starts_with("rsync "))
        .nth(1)
        .expect("a copy-back rsync");
    // The lock file comes from the workspace root, not the crate directory.
    let lock_source = remote_dir(rsync);
    assert!(lock_source.ends_with("/Cargo.lock"), "{rsync}");
}

#[test]
fn watch_mode_refuses_to_mix_with_copy_back() {
    let harness = Harness::new();
    let output = harness.run(&["-H", "user@build-host", "-c", "--watch", "build"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--copy-back is not supported"), "{stderr}");
}

#[test]
fn a_working_directory_outside_the_workspace_is_rejected() {
    let harness = Harness::new();
    std::fs::create_dir_all(harness.project_dir().join("nested")).unwrap();
    let output = harness.run(&["-H", "user@build-host", "-w", "nested", "build"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("must be an ancestor"), "{stderr}");
}

#[test]
fn verbose_logs_the_commands_it_runs() {
    let harness = Harness::new();
    let args = ["-H", "user@build-host", "-v", "build"];
    let stderr = harness.stderr(&args);

    assert!(stderr.contains("running:"), "{stderr}");
    assert!(stderr.contains("rsync"), "{stderr}");
}
