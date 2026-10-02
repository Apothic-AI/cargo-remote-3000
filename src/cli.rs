//! Command line interface definition.
//!
//! The binary is a Cargo subcommand, so it is invoked as `cargo remote-3000 ...`.
//! The single `Remote` subcommand holds every option; remote Cargo arguments are
//! collected verbatim into a trailing vector.

use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand};

/// Cargo subcommand to build Rust projects on a remote build server over SSH.
#[derive(Debug, Parser)]
#[command(
    name = "cargo-remote-3000",
    // Cargo invokes this binary as `cargo <subcommand>`.
    bin_name = "cargo",
    version,
    about,
    // So that `cargo remote-3000 --version` works, not just `cargo-remote-3000 --version`.
    propagate_version = true,
    long_about = "Transfers a Rust project to a remote build server over SSH, runs cargo there,\n\
                  and optionally copies the resulting artifacts back.\n\n\
                  Configure the remote in `.cargo-remote-3000.toml` (see the README) or pass the\n\
                  connection details directly with --host and friends."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Transfer the project, run cargo on the remote, and copy artifacts back
    #[command(
        name = "remote-3000",
        // Without this, clap would render the propagated version as
        // `cargo-remote-3000-remote-3000`.
        display_name = "cargo-remote-3000",
        // `cargo remote` keeps working for anyone who used the original tool.
        visible_alias = "remote",
        after_help = "EXAMPLES:\n  \
            cargo remote-3000 build\n  \
            cargo remote-3000 -c build --release\n  \
            cargo remote-3000 -r workstation build --message-format human\n  \
            cargo remote-3000 -H user@build-host -p 2222 check\n  \
            cargo remote-3000 -c=release/my-app build --release\n  \
            cargo remote-3000 -w .. build   # path dependencies outside the workspace\n  \
            cargo remote-3000 --watch build\n\n\
            Anything after the cargo command is passed to the remote cargo, so a\n\
            `--` separator is not needed."
    )]
    Remote(Remote),
}

/// Options describing how to reach and use a remote build server.
#[derive(Debug, Args, Clone, Default)]
pub struct RemoteOpts {
    /// Name of the remote defined in the config file
    #[arg(short = 'r', long, value_name = "NAME")]
    pub name: Option<String>,

    /// Remote ssh build server as `user@host`, or the name of an ssh config entry
    #[arg(short = 'H', long, value_name = "HOST")]
    pub host: Option<String>,

    /// Port used to talk to the build server over ssh
    #[arg(short = 'p', long, value_name = "PORT")]
    pub ssh_port: Option<u16>,

    /// Identity file passed to `ssh -i`
    #[arg(short = 'i', long, value_name = "FILE")]
    pub identity_file: Option<PathBuf>,

    /// Extra option forwarded verbatim to `ssh`, e.g. `--ssh-option ProxyJump=bastion`
    #[arg(long, value_name = "OPTION")]
    pub ssh_option: Vec<String>,

    /// Directory on the remote host that holds the per-project build directories
    #[arg(short = 't', long, value_name = "DIR")]
    pub temp_dir: Option<String>,

    /// Script sourced before the remote build, e.g. `~/.cargo/env`. Repeatable.
    #[arg(short, long, value_name = "SCRIPT")]
    pub env: Vec<String>,

    /// Exclude pattern passed to rsync when uploading sources. Repeatable.
    #[arg(short = 'x', long, value_name = "PATTERN")]
    pub exclude: Vec<String>,
}

#[derive(Debug, Args)]
pub struct Remote {
    #[command(flatten)]
    pub remote: RemoteOpts,

    /// Remote environment variables, e.g. `RUST_BACKTRACE=1 CC=clang`
    #[arg(
        short,
        long,
        value_name = "VARS",
        default_value = "RUST_BACKTRACE=1",
        allow_hyphen_values = true
    )]
    pub build_env: Vec<String>,

    /// Rustup toolchain used on the remote host, e.g. stable, beta or nightly
    #[arg(short = 'd', long, value_name = "TOOLCHAIN", default_value = "stable")]
    pub rustup_default: String,

    /// Copy the remote target directory back, or just one path within it.
    ///
    /// `-c` copies everything. To copy a single artifact the value has to be
    /// attached with `=`, as in `-c=release/my-binary`, so that a following
    /// cargo command name is never mistaken for the value.
    #[arg(
        short,
        long,
        value_name = "PATH",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = ""
    )]
    pub copy_back: Option<Option<String>>,

    /// Do not copy Cargo.lock back to the local machine
    #[arg(long, action = ArgAction::SetTrue)]
    pub no_copy_lock: bool,

    /// Path to the Cargo.toml to build
    #[arg(long, value_name = "PATH", default_value = "Cargo.toml")]
    pub manifest_path: PathBuf,

    /// Local directory to transfer. Must be an ancestor of the workspace root.
    /// Use this when path dependencies or submodules live outside the workspace.
    #[arg(short, long, value_name = "DIR")]
    pub working_directory: Option<PathBuf>,

    /// Transfer hidden files and directories, including `.git`
    #[arg(long, action = ArgAction::SetTrue)]
    pub transfer_hidden: bool,

    /// Transfer `.git` even when hidden files are skipped
    #[arg(long = "no-transfer-git", action = ArgAction::SetTrue)]
    pub no_transfer_git: bool,

    /// Re-run the remote build whenever a watched source file changes
    #[arg(short = 'W', long, action = ArgAction::SetTrue)]
    pub watch: bool,

    /// Log every rsync/ssh invocation instead of just progress
    #[arg(short, long, action = ArgAction::Count)]
    pub verbose: u8,

    /// The cargo subcommand executed on the remote host, e.g. `build` or `test`
    #[arg(value_name = "COMMAND", required = true)]
    pub command: String,

    /// Arguments appended to the remote cargo invocation. Everything after the
    /// cargo command is passed through, so `--` is not required.
    #[arg(
        value_name = "ARGS",
        allow_hyphen_values = true,
        trailing_var_arg = true
    )]
    pub args: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn parse(args: &[&str]) -> Remote {
        let cli = Cli::try_parse_from(args).expect("args should parse");
        let Command::Remote(remote) = cli.command;
        remote
    }

    #[test]
    fn verify_command() {
        Cli::command().debug_assert();
    }

    #[test]
    fn cargo_subcommand_name_is_used() {
        let cli = Cli::try_parse_from(["cargo", "remote-3000", "build"]).unwrap();
        assert!(matches!(cli.command, Command::Remote(_)));
    }

    #[test]
    fn the_old_subcommand_name_still_works() {
        // Anyone who used the original tool typed `cargo remote`.
        let cli = Cli::try_parse_from(["cargo", "remote", "build"]).unwrap();
        assert!(matches!(cli.command, Command::Remote(_)));
    }

    #[test]
    fn version_works_on_the_subcommand_too() {
        let mut cmd = Cli::command();
        cmd.build();
        let err = cmd
            .try_get_matches_from(["cargo", "remote-3000", "--version"])
            .unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayVersion);
        assert_eq!(
            err.to_string().trim(),
            format!("cargo-remote-3000 {}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn bare_build_works() {
        let remote = parse(&["cargo", "remote-3000", "build"]);
        assert_eq!(remote.command, "build");
        assert!(remote.args.is_empty());
    }

    #[test]
    fn remote_flags_do_not_need_a_double_dash_separator() {
        // Issue #19: this used to require `cargo remote -- build --message-format human`.
        let remote = parse(&[
            "cargo",
            "remote-3000",
            "build",
            "--message-format",
            "human",
            "--release",
        ]);
        assert_eq!(remote.command, "build");
        assert_eq!(remote.args, ["--message-format", "human", "--release"]);
    }

    #[test]
    fn remote_flags_after_a_double_dash_still_work() {
        let remote = parse(&["cargo", "remote-3000", "build", "--", "--release"]);
        assert_eq!(remote.args, ["--release"]);
    }

    #[test]
    fn double_dash_is_not_duplicated_into_args() {
        let remote = parse(&["cargo", "remote-3000", "test", "--", "--release"]);
        assert_eq!(remote.command, "test");
        assert_eq!(remote.args, ["--release"]);
    }

    #[test]
    fn options_before_the_command_are_still_local() {
        let remote = parse(&[
            "cargo",
            "remote-3000",
            "-H",
            "user@host",
            "-p",
            "2222",
            "-c",
            "--no-transfer-git",
            "build",
            "--release",
        ]);
        assert_eq!(remote.remote.host.as_deref(), Some("user@host"));
        assert_eq!(remote.remote.ssh_port, Some(2222));
        assert_eq!(remote.copy_back, Some(Some(String::new())));
        assert!(remote.no_transfer_git);
        assert_eq!(remote.command, "build");
        assert_eq!(remote.args, ["--release"]);
    }

    #[test]
    fn remote_name_and_identity_file_parse() {
        let remote = parse(&[
            "cargo",
            "remote-3000",
            "-r",
            "workstation",
            "-i",
            "/home/me/.ssh/id_ed25519_build",
            "build",
        ]);
        assert_eq!(remote.remote.name.as_deref(), Some("workstation"));
        assert_eq!(
            remote.remote.identity_file.as_deref(),
            Some(std::path::Path::new("/home/me/.ssh/id_ed25519_build"))
        );
    }

    #[test]
    fn copy_back_accepts_an_optional_path() {
        let without = parse(&["cargo", "remote-3000", "-c", "build"]);
        assert_eq!(without.copy_back, Some(Some(String::new())));

        let with = parse(&["cargo", "remote-3000", "-c=release/app", "build"]);
        assert_eq!(with.copy_back, Some(Some("release/app".to_string())));
    }

    #[test]
    fn build_env_is_collected_per_occurrence() {
        let remote = parse(&[
            "cargo",
            "remote-3000",
            "-b",
            "RUST_BACKTRACE=full",
            "-b",
            "CC=clang",
            "build",
        ]);
        assert_eq!(remote.build_env, ["RUST_BACKTRACE=full", "CC=clang"]);
    }

    #[test]
    fn working_directory_parses() {
        let remote = parse(&["cargo", "remote-3000", "-w", "..", "build"]);
        assert_eq!(
            remote.working_directory.as_deref(),
            Some(std::path::Path::new(".."))
        );
    }

    #[test]
    fn cargo_command_is_required() {
        assert!(Cli::try_parse_from(["cargo", "remote-3000"]).is_err());
    }

    #[test]
    fn excludes_repeat() {
        let remote = parse(&[
            "cargo",
            "remote-3000",
            "-x",
            "assets/",
            "-x",
            "*.png",
            "build",
        ]);
        assert_eq!(remote.remote.exclude, ["assets/", "*.png"]);
    }
}
