# cargo-remote-3000

Run `cargo` on a remote build server over SSH and bring the artifacts back
locally.

Compiling a large Rust project on a laptop is slow, and often the machine has
far more CPU and RAM than the one in front of you. This subcommand rsyncs the
sources to a build host, runs the cargo command there, and optionally copies
the resulting `target` directory back.

```bash
cargo remote-3000 -c build --release
```

> This is a maintained fork of [cargo-remote](https://github.com/sgeisler/cargo-remote),
> which had been unmaintained since 2020. All three upstream pull requests are
> merged and every open upstream issue is addressed — see
> [What changed](#what-changed).

## Requirements

**Locally:** `ssh` and `rsync`.

**On the build server:** `ssh` access, `rustup` with a toolchain matching or
close to your local one, and a C toolchain for linking.

If the remote build fails with `cannot find Scrt1.o` or `cannot find crti.o`,
the server is missing its libc headers. On Debian and Ubuntu:

```bash
sudo apt install build-essential   # or: sudo apt install libc6-dev
```

On Fedora and RHEL: `sudo dnf install glibc-devel`. On Arch: the `base-devel`
group. Without these, `cc` cannot link, which surfaces as a confusing error
about missing object files rather than a missing package.

Both machines need the same target architecture and an ABI-compatible libc.
The remote build directory is keyed by the local project path, so two
checkouts of the same project on different machines share one remote build
directory and reuse each other's `target` cache.

## Install

```bash
cargo install cargo-remote-3000
```

Or from source:

```bash
git clone https://github.com/sgeisler/cargo-remote
cd cargo-remote
cargo install --path .
```

Installing puts `cargo-remote-3000` on your `PATH`, and Cargo picks up any
`cargo-<something>` binary as `cargo <something>`. The subcommand also answers
to `cargo remote` as an alias, so old muscle memory keeps working.

## Usage

```
cargo remote-3000 [OPTIONS] <COMMAND> [ARGS]...
```

`<COMMAND>` is the cargo subcommand to run remotely — `build`, `check`,
`test`, `clippy`, `bench`, and so on. Everything after it is passed straight
through to the remote cargo invocation, so no `--` separator is needed:

```bash
cargo remote-3000 build --release
cargo remote-3000 test -- --nocapture
cargo remote-3000 build --message-format human
```

Options that belong to cargo-remote-3000 itself go **before** the command.

### Common invocations

```bash
# Build remotely, keep everything local
cargo remote-3000 build

# Release build, copy artifacts back into ./target
cargo remote-3000 -c build --release

# Copy back a single binary rather than the whole target dir
cargo remote-3000 -c=release/my-app build --release

# Use a named remote from the config file
cargo remote-3000 -r workstation build

# One-off host with a non-standard port and a specific key
cargo remote-3000 -H user@build-host -p 2222 -i ~/.ssh/build_ed25519 check

# Reach the build server through a bastion
cargo remote-3000 -H build-host --ssh-option ProxyJump=bastion build

# Rebuild on every source change, like cargo watch
cargo remote-3000 --watch build
```

`-c` needs its value attached with `=` (`-c=release/my-app`) so that a
following command name is never mistaken for the copy-back path.

### Options

| Option | Description |
| --- | --- |
| `-r, --name <NAME>` | Remote defined in the config file |
| `-H, --host <HOST>` | `user@host`, or the name of an ssh config entry |
| `-p, --ssh-port <PORT>` | ssh port (default 22) |
| `-i, --identity-file <FILE>` | Identity file, passed to `ssh -i` |
| `--ssh-option <OPTION>` | Extra `ssh -o` option. Repeatable |
| `-t, --temp-dir <DIR>` | Remote directory holding per-project build dirs |
| `-e, --env <SCRIPT>` | Script sourced before the remote build. Repeatable |
| `-x, --exclude <PATTERN>` | rsync exclude pattern for the upload. Repeatable |
| `-b, --build-env <VARS>` | Remote environment variables (default `RUST_BACKTRACE=1`) |
| `-d, --rustup-default <TOOLCHAIN>` | Remote toolchain (default `stable`) |
| `-c, --copy-back[=<PATH>]` | Copy the target dir, or one path in it, back |
| `--no-copy-lock` | Do not copy `Cargo.lock` back |
| `--manifest-path <PATH>` | Manifest to build (default `Cargo.toml`) |
| `-w, --working-directory <DIR>` | Directory to upload; must contain the workspace |
| `--transfer-hidden` | Upload dotfiles, including `.git` |
| `--no-transfer-git` | Never upload `.git`, even with `--transfer-hidden` |
| `-W, --watch` | Rebuild on source changes until interrupted |
| `-v, --verbose` | Log every rsync and ssh invocation |

Set `RUST_LOG=debug` for the same detail, or `RUST_LOG=warn` to quieten the
progress output.

## Configuration

Two files are read, each overriding the other:

1. `~/.config/cargo-remote-3000/cargo-remote-3000.toml` — your personal remotes
2. `.cargo-remote-3000.toml` — next to the project's `Cargo.toml`

`.cargo-remote.toml` is still read for backwards compatibility, so an existing
setup keeps working after upgrading. New files should use the
`cargo-remote-3000` names.

```toml
[[remote]]
name = "workstation"          # optional for a single remote
host = "me@laptop.example"    # or an ssh config Host entry
ssh_port = 22
temp_dir = "~/remote-builds"
env = "~/.cargo/env"
exclude = ["assets/", "*.png"]

[[remote]]
name = "rack"
host = "me@rack.example"
ssh_port = 2222
identity_file = "~/.ssh/build_ed25519"
ssh_options = ["ProxyJump=bastion"]
temp_dir = "/var/tmp/rust"
```

Only `host` is required. Anything left out falls back to a default, and every
field can be overridden on the command line.

Note that `-r` selects a remote *by name* while `-H` is the host itself. That is
unchanged from the original tool, though older bug reports used `-r` as though
it took a host.

Remotes are merged by `name`, so a project config entry can override a single
field of a user-level remote without repeating the rest:

```toml
# .cargo-remote-3000.toml — use the personal `workstation` remote, but with
# more disk space on this particular project
[[remote]]
name = "workstation"
temp_dir = "/mnt/big/rust"
```

With one configured remote the name can be omitted; `cargo remote-3000 build`
just uses it. Passing `-r` with an unknown name lists the names that do exist.

### Environment setup on the build server

The default `env` is `~/.cargo/env`, the file rustup's installer writes. Before
each build, cargo-remote-3000 runs:

```sh
[ -f ~/.cargo/env ] && . ~/.cargo/env
```

Note the `.` rather than bash's `source`, and the existence check. A build
server with `dash` or a restrictive non-login shell has no `source` builtin and
would fail silently under the old implementation. Add more scripts with
repeated `-e`:

```bash
cargo remote-3000 -e /etc/profile -e ~/.profile -e ~/.cargo/env build
```

Script paths that start with `~` are expanded by the remote shell, not locally.

## Watch mode

```bash
cargo remote-3000 --watch build
```

The initial build runs immediately; after that, changes to the watched source
tree trigger a re-sync and rebuild. The watcher uses the platform's native file
notification API, so it reacts in milliseconds. Changes inside `target/` and
`.git/` are ignored, since neither affects a build. A failing rebuild is logged
and the session continues — fix the code and it picks up where it left off.

`--watch` cannot be combined with `-c`: artifacts are meant to stay on the
build server, and re-fetching them on every keystroke would defeat the point.

## Workspaces and local path dependencies

`cargo remote-3000` reads the layout from `cargo metadata`, so a workspace
works as you'd expect. Cargo runs in the crate's directory and the shared
`target/` and `Cargo.lock` are read from the workspace root, which is where
Cargo actually puts them.

Path dependencies pointing outside the workspace are not uploaded by default,
because the workspace root is the upload root. Use `-w` to widen the upload:

```
my-project/
├── dep/          # path dependency, sibling of the workspace
└── my-project/   # the workspace
```

```bash
cd my-project
cargo remote-3000 -w .. build
```

`-w` takes a directory that contains the workspace root. The same trick covers
git submodules checked out next to the crate.

## What changed

Everything below was an open issue or pull request on the original
[cargo-remote](https://github.com/sgeisler/cargo-remote).

**Merged pull requests**

- [#21 — `--working-directory`](https://github.com/sgeisler/cargo-remote/pull/21),
  for path dependencies outside the workspace.
- [#24 — copy-back skips `target/{deps,build}`](https://github.com/sgeisler/cargo-remote/pull/24),
  which dominated copy-back time. `.fingerprint/` and `incremental/` are skipped
  too.
- [#25 — `--no-transfer-git`](https://github.com/sgeisler/cargo-remote/pull/25),
  so a large `.git` can be kept local even with `--transfer-hidden`.

**Fixed issues**

- [#2 — compile with watch](https://github.com/sgeisler/cargo-remote/issues/2):
  `--watch` mode. The rsync progress flag that BSD rsync on macOS rejects is
  gone as well.
- [#4 — exclude folders from the transfer](https://github.com/sgeisler/cargo-remote/issues/4):
  `-x` on the command line, `exclude` in the config.
- [#12 — local dependencies outside the workspace](https://github.com/sgeisler/cargo-remote/issues/12):
  `-w`, and a clear error instead of a confusing cargo error when the
  dependency really is missing.
- [#14 — workspace support](https://github.com/sgeisler/cargo-remote/issues/14):
  the target directory now comes from `cargo metadata`, so a workspace-level
  `target/` and a custom `build.target-dir` both work.
- [#15 — specify the port](https://github.com/sgeisler/cargo-remote/issues/15):
  `-p`, propagated to both ssh and the ssh that rsync spawns.
- [#16 and #26 — the environment profile](https://github.com/sgeisler/cargo-remote/issues/16):
  sourced with POSIX `.` and only when the file exists. The old `source
  /etc/profile` silently did nothing on `dash`, which is what #26 reported.
- [#19 — remote options rejected](https://github.com/sgeisler/cargo-remote/issues/19):
  everything after the cargo command is passed through, so `cargo
  remote-3000 build --message-format human` works without `--`.
- [#22 — pass an identity file](https://github.com/sgeisler/cargo-remote/issues/22):
  `-i` on the command line, `identity_file` in the config.
- [#23 — missing libc headers](https://github.com/sgeisler/cargo-remote/issues/23):
  documented under [Requirements](#requirements).

**Other repairs**

- The remote build's exit status is propagated, so `cargo remote-3000 build`
  fails a CI job when the build fails. Previously `ssh -t` masked it.
- `--build-env` values are validated as `NAME=value`. They were interpolated
  into a remote shell script, so a value could run arbitrary commands there.
- Paths and arguments passed to the remote shell are quoted.
- Errors name the thing that failed, and the failing step, instead of printing
  an `unwrap()` panic.
- Rebuilt on clap 4, `cargo_metadata` 0.23 and edition 2021; the old
  `structopt`, `config` and `xdg` 2 dependencies are gone.
- `cargo remote` works as an alias for `cargo remote-3000`.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success |
| 2 | Usage error, for example `--watch` with `-c` |
| 3 | Setup error: no remote configured, unreadable config, bad manifest |
| 4 | Transfer error: ssh or rsync failed |
| other | The remote build's own exit status |

## License

MIT. See [LICENSE](LICENSE).