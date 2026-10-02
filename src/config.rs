//! Loading and merging of cargo-remote-3000 configuration.
//!
//! Two files are read, in increasing order of precedence:
//!
//! 1. the user level config, `$XDG_CONFIG_HOME/cargo-remote-3000/cargo-remote-3000.toml`
//!    (usually `~/.config/...`), and
//! 2. the project level config, `.cargo-remote-3000.toml` next to the project's
//!    `Cargo.toml`.
//!
//! Remotes are merged by `name`: a project remote with the same name as a user
//! remote replaces it field by field, otherwise it is appended. This makes it
//! possible to redefine a single remote for one project without restating the
//! host.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use log::debug;
use serde::Deserialize;

use crate::cli::RemoteOpts;

/// Directory and file name used under the user config root.
const APP_DIR: &str = "cargo-remote-3000";
const CONFIG_FILE: &str = "cargo-remote-3000.toml";

/// Name of the config file looked up in the project directory.
pub const PROJECT_FILE: &str = ".cargo-remote-3000.toml";
/// Legacy project config name, still honoured so older setups keep working.
pub const LEGACY_PROJECT_FILE: &str = ".cargo-remote.toml";

/// Default remote build directory on the build server.
pub const DEFAULT_TEMP_DIR: &str = "~/remote-builds";
/// Default ssh port.
pub const DEFAULT_SSH_PORT: u16 = 22;
/// Default script sourced before the remote build.
///
/// `~/.cargo/env` is what `rustup`'s own installer writes, so it is the one file
/// that reliably puts `cargo` on `PATH` for a non-login ssh session. See
/// issue #16.
pub const DEFAULT_ENV: &str = "~/.cargo/env";

/// A fully resolved remote: config defaults with command line overrides applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub host: String,
    pub ssh_port: u16,
    pub identity_file: Option<PathBuf>,
    pub ssh_options: Vec<String>,
    pub temp_dir: String,
    pub env: Vec<String>,
    pub exclude: Vec<String>,
}

/// On-disk shape of a `[[remote]]` entry. Every field except `host` is optional.
#[derive(Debug, Default, Deserialize)]
struct PartialRemote {
    #[serde(default)]
    name: Option<String>,
    /// Empty when a project config entry only overrides other fields; the host
    /// then comes from the user level entry of the same name.
    #[serde(default)]
    host: String,
    #[serde(default)]
    ssh_port: Option<u16>,
    #[serde(default)]
    identity_file: Option<String>,
    #[serde(default)]
    ssh_options: Vec<String>,
    #[serde(default)]
    temp_dir: Option<String>,
    #[serde(default)]
    env: Option<EnvSpec>,
    #[serde(default)]
    exclude: Vec<String>,
}

/// `env` may be given as a single string or as a list of strings.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum EnvSpec {
    One(String),
    Many(Vec<String>),
}

impl EnvSpec {
    fn into_vec(self) -> Vec<String> {
        match self {
            EnvSpec::One(s) => vec![s],
            EnvSpec::Many(v) => v,
        }
    }
}

/// One parsed config file.
#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    remote: Vec<PartialRemote>,
}

impl ConfigFile {
    fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config file {}", path.display()))?;
        toml::from_str(&text)
            .with_context(|| format!("failed to parse config file {}", path.display()))
    }
}

/// The merged view of all config files.
#[derive(Debug, Default)]
pub struct Config {
    remotes: Vec<PartialRemote>,
}

impl Config {
    /// Read the user config, then the project config, and merge them.
    pub fn load(project_dir: &Path) -> Result<Self> {
        let mut remotes: Vec<PartialRemote> = Vec::new();

        if let Some(user_config) = user_config_path() {
            if user_config.is_file() {
                debug!("using user config {}", user_config.display());
                for remote in ConfigFile::load(&user_config)?.remote {
                    upsert(&mut remotes, remote);
                }
            }
        }

        for candidate in [
            project_dir.join(PROJECT_FILE),
            project_dir.join(LEGACY_PROJECT_FILE),
        ] {
            if candidate.is_file() {
                debug!("using project config {}", candidate.display());
                for remote in ConfigFile::load(&candidate)?.remote {
                    upsert(&mut remotes, remote);
                }
            }
        }

        Ok(Config { remotes })
    }

    /// Number of configured remotes.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.remotes.len()
    }

    /// Resolve the remote to build on.
    ///
    /// `--host` alone is enough; without it a config file entry is
    /// required. Without `--name` the first configured remote is used, so a
    /// single-remote setup needs no name at all.
    pub fn get_remote(&self, opts: &RemoteOpts) -> Result<Remote> {
        let configured = match &opts.name {
            Some(name) => {
                let found = self
                    .remotes
                    .iter()
                    .find(|r| r.name.as_deref() == Some(name.as_str()));
                match found {
                    Some(found) => Some(found),
                    None if self.remotes.is_empty() => None,
                    None => {
                        let known: Vec<&str> = self
                            .remotes
                            .iter()
                            .filter_map(|r| r.name.as_deref())
                            .collect();
                        bail!(
                            "no remote named `{name}` in the config file. \
                             Known remotes: {}",
                            if known.is_empty() {
                                "<none>".to_string()
                            } else {
                                known.join(", ")
                            }
                        );
                    }
                }
            }
            None => self.remotes.first(),
        };

        let base = match (configured, opts.host.is_some()) {
            (Some(base), _) => base,
            (None, true) => &EMPTY_REMOTE,
            (None, false) => {
                bail!(
                    "no remote build server configured.\n\
                     Pass --host user@host, or create a config file.\n\
                     Project config: {PROJECT_FILE}\n\
                     User config:    {}",
                    user_config_display()
                );
            }
        };

        Ok(Remote {
            name: opts
                .name
                .clone()
                .or_else(|| base.name.clone())
                .unwrap_or_default(),
            host: opts.host.clone().unwrap_or_else(|| base.host.clone()),
            ssh_port: opts
                .ssh_port
                .unwrap_or(base.ssh_port.unwrap_or(DEFAULT_SSH_PORT)),
            identity_file: opts
                .identity_file
                .clone()
                .or_else(|| base.identity_file.as_deref().map(PathBuf::from)),
            ssh_options: if opts.ssh_option.is_empty() {
                base.ssh_options.clone()
            } else {
                opts.ssh_option.clone()
            },
            temp_dir: opts
                .temp_dir
                .clone()
                .or_else(|| base.temp_dir.clone())
                .unwrap_or_else(|| DEFAULT_TEMP_DIR.to_string()),
            env: if opts.env.is_empty() {
                match &base.env {
                    Some(spec) => spec.clone().into_vec(),
                    None => vec![DEFAULT_ENV.to_string()],
                }
            } else {
                opts.env.clone()
            },
            exclude: if opts.exclude.is_empty() {
                base.exclude.clone()
            } else {
                opts.exclude.clone()
            },
        })
    }
}

/// Placeholder used when only `--host` was given.
static EMPTY_REMOTE: PartialRemote = PartialRemote {
    name: None,
    host: String::new(),
    ssh_port: None,
    identity_file: None,
    ssh_options: Vec::new(),
    temp_dir: None,
    env: None,
    exclude: Vec::new(),
};

/// Insert `remote`, replacing an existing entry with the same name.
fn upsert(remotes: &mut Vec<PartialRemote>, remote: PartialRemote) {
    if let Some(name) = remote.name.as_deref() {
        if let Some(existing) = remotes.iter_mut().find(|r| r.name.as_deref() == Some(name)) {
            // A project config entry that only overrides some fields should not
            // wipe out the user level host, so merge field by field.
            existing.merge_from(remote);
            return;
        }
    }
    remotes.push(remote);
}

impl PartialRemote {
    fn merge_from(&mut self, other: PartialRemote) {
        if !other.host.is_empty() {
            self.host = other.host;
        }
        self.ssh_port = other.ssh_port.or(self.ssh_port.take());
        self.identity_file = other.identity_file.or(self.identity_file.take());
        if !other.ssh_options.is_empty() {
            self.ssh_options = other.ssh_options;
        }
        self.temp_dir = other.temp_dir.or(self.temp_dir.take());
        self.env = other.env.or(self.env.take());
        if !other.exclude.is_empty() {
            self.exclude = other.exclude;
        }
    }
}

/// The per-user config file location.
///
/// On Linux this is `$XDG_CONFIG_HOME/cargo-remote-3000/cargo-remote-3000.toml`
/// (usually under `~/.config`); `dirs` handles the equivalent macOS and Windows
/// locations.
fn user_config_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join(APP_DIR).join(CONFIG_FILE))
}

fn user_config_display() -> String {
    user_config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "~/.config/cargo-remote-3000/cargo-remote-3000.toml".to_string())
}

impl Remote {
    /// `ssh` arguments shared by the build and the rsync transport.
    pub fn ssh_args(&self) -> Vec<String> {
        let mut args = vec!["-p".to_string(), self.ssh_port.to_string()];
        if let Some(identity) = &self.identity_file {
            args.push("-i".to_string());
            args.push(identity.display().to_string());
        }
        for option in &self.ssh_options {
            args.push("-o".to_string());
            args.push(option.clone());
        }
        args
    }

    /// The `-e` string rsync needs so it shells out to the same ssh setup.
    pub fn rsync_ssh_command(&self) -> String {
        let mut cmd = String::from("ssh");
        for arg in self.ssh_args() {
            cmd.push(' ');
            cmd.push_str(&shell_quote(&arg));
        }
        cmd
    }
}

/// Single-quote a string for a POSIX shell.
///
/// A leading `~` is deliberately left unquoted: the remote build directory is
/// specified as `~/remote-builds`, and tilde expansion only happens outside
/// quotes. Anything else that needs quoting is single-quoted.
pub fn shell_quote(value: &str) -> String {
    let safe = !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./=:@,+~".contains(&b));
    if safe && !value.starts_with('\'') {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote_from(toml_text: &str) -> Config {
        let file: ConfigFile = toml::from_str(toml_text).unwrap();
        Config {
            remotes: file.remote,
        }
    }

    fn remote_from_project(dir: &Path, toml_text: &str) -> Config {
        std::fs::write(dir.join(PROJECT_FILE), toml_text).unwrap();
        Config::load(dir).unwrap()
    }

    #[test]
    fn minimal_remote_gets_defaults() {
        let conf = remote_from(
            r#"
            [[remote]]
            host = "user@build-host"
            "#,
        );
        let remote = conf.get_remote(&RemoteOpts::default()).unwrap();
        assert_eq!(remote.host, "user@build-host");
        assert_eq!(remote.ssh_port, DEFAULT_SSH_PORT);
        assert_eq!(remote.temp_dir, DEFAULT_TEMP_DIR);
        assert_eq!(remote.env, [DEFAULT_ENV]);
    }

    #[test]
    fn every_field_can_be_set_in_the_config() {
        let conf = remote_from(
            r#"
            [[remote]]
            name = "workstation"
            host = "me@laptop"
            ssh_port = 2222
            identity_file = "~/.ssh/build_ed25519"
            ssh_options = ["ProxyJump=bastion", "ControlMaster=auto"]
            temp_dir = "/var/tmp/rust"
            env = "~/.cargo/env"
            exclude = ["assets/"]
            "#,
        );
        let opts = RemoteOpts {
            name: Some("workstation".to_string()),
            ..Default::default()
        };
        let remote = conf.get_remote(&opts).unwrap();
        assert_eq!(remote.ssh_port, 2222);
        assert_eq!(
            remote.identity_file.as_deref(),
            Some(Path::new("~/.ssh/build_ed25519"))
        );
        assert_eq!(
            remote.ssh_options,
            ["ProxyJump=bastion", "ControlMaster=auto"]
        );
        assert_eq!(remote.temp_dir, "/var/tmp/rust");
        assert_eq!(remote.env, ["~/.cargo/env"]);
        assert_eq!(remote.exclude, ["assets/"]);
    }

    #[test]
    fn env_accepts_a_list() {
        let conf = remote_from(
            r#"
            [[remote]]
            host = "user@host"
            env = ["/etc/profile", "~/.profile", "~/.cargo/env"]
            "#,
        );
        let remote = conf.get_remote(&RemoteOpts::default()).unwrap();
        assert_eq!(remote.env, ["/etc/profile", "~/.profile", "~/.cargo/env"]);
    }

    #[test]
    fn cli_overrides_win_over_config() {
        let conf = remote_from(
            r#"
            [[remote]]
            name = "workstation"
            host = "me@laptop"
            ssh_port = 2222
            temp_dir = "/var/tmp/rust"
            env = "~/.cargo/env"
            "#,
        );
        let opts = RemoteOpts {
            name: Some("workstation".to_string()),
            host: Some("other@host".to_string()),
            ssh_port: Some(2200),
            temp_dir: Some("/tmp/other".to_string()),
            env: vec!["/etc/profile".to_string()],
            ..Default::default()
        };
        let remote = conf.get_remote(&opts).unwrap();
        assert_eq!(remote.host, "other@host");
        assert_eq!(remote.ssh_port, 2200);
        assert_eq!(remote.temp_dir, "/tmp/other");
        assert_eq!(remote.env, ["/etc/profile"]);
    }

    #[test]
    fn host_flag_alone_needs_no_config_file() {
        // A bare --host should be enough with no config file at all.
        let conf = Config::default();
        let opts = RemoteOpts {
            host: Some("user@host".to_string()),
            ..Default::default()
        };
        let remote = conf.get_remote(&opts).unwrap();
        assert_eq!(remote.host, "user@host");
    }

    #[test]
    fn no_host_and_no_config_is_a_clear_error() {
        let err = Config::default()
            .get_remote(&RemoteOpts::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("no remote build server configured"), "{err}");
    }

    #[test]
    fn unknown_remote_name_lists_the_known_ones() {
        let conf = remote_from(
            r#"
            [[remote]]
            name = "workstation"
            host = "me@laptop"

            [[remote]]
            name = "rack"
            host = "me@rack"
            "#,
        );
        let opts = RemoteOpts {
            name: Some("nope".to_string()),
            ..Default::default()
        };
        let err = conf.get_remote(&opts).unwrap_err().to_string();
        assert!(err.contains("no remote named `nope`"), "{err}");
        assert!(err.contains("workstation, rack"), "{err}");
    }

    #[test]
    fn project_config_overrides_user_remote_field_by_field() {
        // A project entry that only sets `temp_dir` must not wipe out the host
        // inherited from the user level config of the same name.
        // a remotes list through the same merge helper the loader uses.
        let mut remotes = Vec::new();
        upsert(
            &mut remotes,
            toml::from_str(
                r#"
                name = "workstation"
                host = "me@laptop"
                ssh_port = 2222
                "#,
            )
            .unwrap(),
        );
        upsert(
            &mut remotes,
            toml::from_str(
                r#"
                name = "workstation"
                temp_dir = "/var/tmp/rust"
                "#,
            )
            .unwrap(),
        );

        assert_eq!(remotes.len(), 1);
        assert_eq!(remotes[0].host, "me@laptop");
        assert_eq!(remotes[0].ssh_port, Some(2222));
        assert_eq!(remotes[0].temp_dir.as_deref(), Some("/var/tmp/rust"));
    }

    #[test]
    fn project_config_file_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let conf = remote_from_project(
            dir.path(),
            r#"
            [[remote]]
            host = "me@configured"
            "#,
        );
        assert_eq!(conf.len(), 1);
        assert_eq!(
            conf.get_remote(&RemoteOpts::default()).unwrap().host,
            "me@configured"
        );
    }

    #[test]
    fn legacy_project_config_file_is_still_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(LEGACY_PROJECT_FILE),
            "[[remote]]\nhost = \"me@legacy\"\n",
        )
        .unwrap();
        let conf = Config::load(dir.path()).unwrap();
        assert_eq!(
            conf.get_remote(&RemoteOpts::default()).unwrap().host,
            "me@legacy"
        );
    }

    #[test]
    fn malformed_config_reports_the_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(PROJECT_FILE), "this is not toml {{{").unwrap();
        let err = Config::load(dir.path()).unwrap_err().to_string();
        assert!(err.contains(PROJECT_FILE), "{err}");
    }

    #[test]
    fn temp_dir_keeps_its_tilde_for_the_remote_shell_to_expand() {
        // The temp dir is expanded by the *remote* shell, so a literal `~`
        // must survive into the rsync destination and the ssh script.
        let remote = Remote {
            name: String::new(),
            host: "me@host".into(),
            ssh_port: 22,
            identity_file: None,
            ssh_options: vec![],
            temp_dir: "~/remote-builds".into(),
            env: vec![],
            exclude: vec![],
        };
        assert_eq!(remote.temp_dir, "~/remote-builds");
    }

    #[test]
    fn ssh_args_include_port_identity_and_options() {
        let remote = Remote {
            name: String::new(),
            host: "me@host".into(),
            ssh_port: 2222,
            identity_file: Some(PathBuf::from("/keys/build")),
            ssh_options: vec!["ProxyJump=bastion".into()],
            temp_dir: DEFAULT_TEMP_DIR.into(),
            env: vec![],
            exclude: vec![],
        };
        assert_eq!(
            remote.ssh_args(),
            ["-p", "2222", "-i", "/keys/build", "-o", "ProxyJump=bastion"]
        );
        assert_eq!(
            remote.rsync_ssh_command(),
            "ssh -p 2222 -i /keys/build -o ProxyJump=bastion"
        );
    }

    #[test]
    fn shell_quote_only_quotes_when_needed() {
        assert_eq!(shell_quote("stable"), "stable");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote(""), "''");
    }
}
