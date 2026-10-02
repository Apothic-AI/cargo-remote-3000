# cargo-remote-3000

**Run `cargo` on a remote build server over SSH, then bring the artifacts home.**

Compiling a large Rust project on a laptop is slow, and the build machine in the
closet usually has far more CPU and RAM than the one on your desk.
`cargo-remote-3000` rsyncs your sources to a build host, runs the cargo command
there, and optionally copies the resulting `target` directory back.

```console
$ cargo remote-3000 -c build --release
```

Local sources are never modified by the remote build, and a failed remote build
fails your local command with the build server's own exit code.

---

## Contents

- [Why](#why)
- [Install](#install)
- [Requirements](#requirements)
- [Quick start](#quick-start)
- [Usage](#usage)
- [Configuration](#configuration)
- [Watch mode](#watch-mode)
- [Workspaces and path dependencies](#workspaces-and-path-dependencies)
- [How it works](#how-it-works)
- [Troubleshooting](#troubleshooting)
- [Exit codes](#exit-codes)
- [What changed from cargo-remote](#what-changed-from-cargo-remote)
- [Development](#development)
- [License](#license)

## Why

Rebuilding the same dependency tree every time you switch machines is the tax
that makes Rust painful on underpowered hardware. Remote offloading keeps a
single warm `target/` cache on a capable machine, so the second build is fast no
matter which laptop you are on.

This is a maintained fork of
[cargo-remote](https://github.com/sgeisler/cargo-remote), which had been
unmaintained since 2020. All three upstream pull requests are merged and every
open upstream issue is addressed — see
[What changed](#what-changed-from-cargo-remote).

## Install

```bash
cargo install cargo-remote-3000
```

Or from source:

```bash
git clone https://github.com/Apothic-AI/cargo-remote-3000
cd cargo-remote-3000
cargo install --path .
```

Installing puts `cargo-remote-3000` on your `PATH`. Cargo turns any
`cargo-<something>` binary into `cargo <something>`, so the subcommand is
`cargo remote-3000`. `cargo remote` works as an alias, so muscle memory from the
original tool carries over.

## Requirements

**Locally:** `ssh` and `rsync`.

**On the build server:** ssh access, [rustup] with a toolchain matching or close
to your local one, and a C toolchain for linking.

Both machines need the same target architecture and an ABI-compatible libc,
since artifacts are copied back to run locally. If the remote build fails with
`cannot find Scrt1.o`, the server is missing its libc headers — see
[Troubleshooting](#troubleshooting).

## Quick start

You need a machine you can reach over ssh, with [rustup] installed.

```bash
# 1. Confirm plain ssh works, and that cargo is on the remote PATH
ssh build-host 'cargo --version'

# 2. Build remotely
cargo remote-3000 -H user@build-host build

# 3. Build remotely and keep the artifacts locally
cargo remote-3000 -H user@build-host -c build --release
```

Then, once it stops feeling repetitive, put the remote in a config file:

```toml
# .cargo-remote-3000.toml
[[remote]]
name = "workstation"
host = "user@build-host"
```

```bash
cargo remote-3000 build            # single remote, no name needed
cargo remote-3000 -r workstation build
```

[rustup]: https://rustup.rs

## Usage

```
cargo remote-3000 [OPTIONS] <COMMAND> [ARGS]...
```

`<COMMAND>` is the cargo subcommand to run remotely — `build`, `check`, `test`,
`clippy`, `bench`, and so on. Everything after it is passed straight through to
the remote cargo invocation, so **no `--` separator is needed**:

```bash
cargo remote-3000 build --release
cargo remote-3000 test -- --nocapture
cargo remote-3000 build --message-format human
```

Options belonging to cargo-remote-3000 itself go **before** the command:

```bash
cargo remote-3000 -r workstation -c build --release
#             ^^^^^^^^^^^^^^^^^^ remote-3000    ^^^^^^^ cargo
```

### Common invocations

```bash
# Build remotely, keep everything on the build server
cargo remote-3000 build

# Release build, copy artifacts back into ./target
cargo remote-3000 -c build --release

# Copy back a single binary instead of the whole target dir
cargo remote-3000 -c=release/my-app build --release

# Named remote from the config file
cargo remote-3000 -r workstation build

# One-off host with a non-standard port and a specific key
cargo remote-3000 -H user@build-host -p 2222 -i ~/.ssh/build_ed25519 check

# Reach the build server through a bastion host
cargo remote-3000 -H build-host --ssh-option ProxyJump=bastion build

# Keep large binary assets off the wire
cargo remote-3000 -x assets/ -x '*.png' build

# Use a nightly toolchain on the build host
cargo remote-3000 -d nightly build

# Rebuild on every source change, like cargo watch
cargo remote-3000 --watch build
```

`-c` takes its value attached with `=` (`-c=release/my-app`) so a following
command name is never mistaken for the copy-back path.

### Options

#### Choosing the remote

| Option | Default | Description |
| --- | --- | --- |
| `-r, --name <NAME>` | first configured remote | Remote defined in the config file |
| `-H, --host <HOST>` | — | `user@host`, or the name of an ssh config entry |
| `-p, --ssh-port <PORT>` | `22` | ssh port |
| `-i, --identity-file <FILE>` | ssh default | Identity file, passed to `ssh -i` |
| `--ssh-option <OPTION>` | — | Extra `ssh -o` option. Repeatable |

#### Controlling the build

| Option | Default | Description |
| --- | --- | --- |
| `-d, --rustup-default <TOOLCHAIN>` | `stable` | Toolchain selected on the build host |
| `-b, --build-env <VARS>` | `RUST_BACKTRACE=1` | Remote environment variables. Repeatable |
| `-e, --env <SCRIPT>` | `~/.cargo/env` | Script sourced before the build. Repeatable |
| `--manifest-path <PATH>` | `Cargo.toml` | Manifest to build |
| `-w, --working-directory <DIR>` | workspace root | Directory to upload; must contain the workspace |

#### Transferring

| Option | Default | Description |
| --- | --- | --- |
| `-t, --temp-dir <DIR>` | `~/remote-builds` | Remote directory holding per-project build dirs |
| `-x, --exclude <PATTERN>` | — | rsync exclude pattern for the upload. Repeatable |
| `--transfer-hidden` | off | Upload dotfiles, including `.git` |
| `--no-transfer-git` | off | Never upload `.git`, even with `--transfer-hidden` |

#### Getting results back

| Option | Default | Description |
| --- | --- | --- |
| `-c, --copy-back[=<PATH>]` | off | Copy the target dir, or one path within it, back |
| `--no-copy-lock` | off | Do not copy `Cargo.lock` back |

#### Everything else

| Option | Description |
| --- | --- |
| `-W, --watch` | Rebuild on source changes until interrupted |
| `-v, --verbose` | Log every rsync and ssh invocation |
| `-h, --help` | Show help |
| `-V, --version` | Show version |

`RUST_LOG=debug` gives the same detail as `-v`; `RUST_LOG=warn` quiets the rsync
progress output down to just warnings.

## Configuration

Two files are read, the second overriding the first:

1. **User config** — `~/.config/cargo-remote-3000/cargo-remote-3000.toml`
   (on macOS and Windows, the platform config directory)
2. **Project config** — `.cargo-remote-3000.toml`, next to the project's
   `Cargo.toml`

The legacy name `.cargo-remote.toml` is still read, so an existing setup keeps
working after upgrading. New files should use the `cargo-remote-3000` names.

```toml
# .cargo-remote-3000.toml
[[remote]]
name = "workstation"           # omit for a single remote
host = "me@laptop.example"     # or an ssh config Host entry
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

Only `host` is required. Anything omitted falls back to a default, and every
field can be overridden on the command line.

> `-r` selects a remote **by name**; `-H` is the **host** itself. This matches
> the original tool, though some older bug reports used `-r` as though it took
> a host.

Remotes merge **by name**, so a project config can override a single field of a
user-level remote without repeating the rest:

```toml
# .cargo-remote-3000.toml — reuse the personal `workstation` remote, but with
# more disk space for this particular project
[[remote]]
name = "workstation"
temp_dir = "/mnt/big/rust"
```

With exactly one configured remote the name can be omitted entirely. Passing
`-r` with an unknown name lists the names that do exist.

### Environment setup on the build server

The default `env` is `~/.cargo/env`, the file rustup's own installer writes.
Before each build, cargo-remote-3000 runs:

```sh
[ -f ~/.cargo/env ] && . ~/.cargo/env
```

Note the `.` rather than bash's `source`, and the existence check. A build
server running `dash`, or any shell without the `source` builtin, silently ran
the build with no environment at all before this was fixed. Add more scripts by
repeating `-e`:

```bash
cargo remote-3000 -e /etc/profile -e ~/.profile -e ~/.cargo/env build
```

Paths starting with `~` are expanded by the remote shell, not locally — the
build host's home directory has nothing to do with yours.

## Watch mode

```bash
cargo remote-3000 --watch build
```

The initial build runs immediately. After that, changes to the watched source
tree trigger a re-sync and a rebuild, using the platform's native filesystem
notification API, so it reacts in milliseconds rather than polling.

Changes under `target/` and `.git/` are ignored, since neither can affect a
build. A failing rebuild is logged and the session keeps running — fix the code
and it picks up from there.

`--watch` cannot be combined with `-c`. Artifacts are meant to stay on the build
server during a watch session; re-fetching them on every keystroke would defeat
the point.

## Workspaces and path dependencies

The project layout is read from `cargo metadata`, so workspaces behave as
you'd expect. Cargo runs in the crate's directory, while the shared `target/`
and `Cargo.lock` are read from the workspace root — which is where Cargo
actually puts them.

Path dependencies pointing **outside** the workspace are not uploaded, because
the workspace root is the upload root. Use `-w` to widen the upload:

```
my-project/
├── dep/          # path dependency, a sibling of the workspace
└── my-project/   # the workspace
```

```bash
cd my-project
cargo remote-3000 -w .. build
```

`-w` takes any directory that contains the workspace root. The same trick
covers git submodules checked out next to the crate. A custom
`build.target-dir` or `CARGO_TARGET_DIR` is honoured too, so artifacts are
fetched from wherever Cargo would actually have put them.

## How it works

For each project, cargo-remote-3000 derives a stable directory on the build
server by hashing the local project path:

```
<temp_dir>/<hash-of-local-project-path>
```

Because the hash comes from the local path, two checkouts of the same project on
different machines resolve to the *same* remote directory and share one warm
`target/` cache.

Each run then does three things:

1. **Upload** — `rsync --delete` the project tree to the remote directory,
   skipping `target/`, hidden files (unless `--transfer-hidden`) and any
   `-x` patterns. rsync shells out to ssh with the same port, identity file and
   options you passed.
2. **Build** — one ssh invocation running a small POSIX shell script that sources
   the `env` scripts, selects the toolchain, `cd`s into the crate and runs
   `cargo <command> <args>`. Arguments are shell-quoted, and the build's exit
   status is propagated back to your shell.
3. **Copy back** — optionally `rsync` the target directory (or one path in it)
   home, skipping `deps/`, `build/`, `.fingerprint/` and `incremental/`, which
   are either regenerated locally or large enough to dominate transfer time.
   `Cargo.lock` comes back too unless you pass `--no-copy-lock`.

Everything the tool needs from the build server is ssh plus `cargo`. There is no
daemon, no agent and no state beyond that one directory per project.

## Troubleshooting

**`cannot find Scrt1.o` or `cannot find crti.o` on the build server**

The server is missing its libc headers, so `cc` cannot link:

| Platform | Package |
| --- | --- |
| Debian, Ubuntu, Mint | `sudo apt install build-essential` (or `libc6-dev`) |
| Fedora, RHEL | `sudo dnf install glibc-devel` |
| Arch | `sudo pacman -S base-devel` |
| Alpine | `sudo apk add build-base musl-dev` |

**`cargo: command not found` on the build server**

The environment script was not found or not sourced. Check that
`~/.cargo/env` exists on the server, or point `-e` at the right path. Running
`cargo remote-3000 -v build` prints the exact ssh invocation if you want to see
what was tried.

**`failed to load source for a dependency` pointing outside the workspace**

A path dependency lives above the workspace root. Upload more than the
workspace with `-w`, for example `-w ..`.

**`error: Found argument '--message-format' which wasn't expected`**

Options that belong to cargo-remote-3000 must come *before* the cargo command.
Anything after it is forwarded to the remote cargo, including flags like
`--message-format`.

**Builds are slow and rsync transfers huge amounts of data**

Hidden files and `target` are skipped by default. Use `-x` for large binary
assets, `--no-transfer-git` with `--transfer-hidden` to keep `.git` local, and
`-c=path/to/one/artifact` instead of `-c` to avoid pulling the whole target
directory back.

**macOS rsync errors on an unknown option**

Fixed here — the old `--info=progress2` flag does not exist in the BSD rsync
that ships with macOS. If you hit an rsync option error anyway, installing a
current rsync with `brew install rsync` and putting it first on `PATH` resolves
it.

**Two projects colliding, or a build directory you cannot find**

Each project gets its own hashed directory. Run with `-v` to print the exact
path, or set `-t` to a directory you control.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success |
| `2` | Usage error, for example `--watch` together with `-c` |
| `3` | Setup error: no remote configured, unreadable config, bad manifest |
| `4` | Transfer error: ssh or rsync failed |
| other | The remote build's own exit status |

A failing remote build therefore reports its own code, which is what makes this
usable as a CI drop-in.

## What changed from cargo-remote

Everything below was an open issue or pull request on
[cargo-remote](https://github.com/sgeisler/cargo-remote).

**Merged pull requests**

- [#21 — `--working-directory`](https://github.com/sgeisler/cargo-remote/pull/21)
  for path dependencies outside the workspace.
- [#24 — copy-back skips `target/{deps,build}`](https://github.com/sgeisler/cargo-remote/pull/24),
  which dominated copy-back time. `.fingerprint/` and `incremental/` are skipped
  too.
- [#25 — `--no-transfer-git`](https://github.com/sgeisler/cargo-remote/pull/25)
  so a large `.git` can stay local even with `--transfer-hidden`.

**Fixed issues**

| Issue | Resolution |
| --- | --- |
| [#2 — compile with watch](https://github.com/sgeisler/cargo-remote/issues/2) | `--watch` mode. The rsync progress flag that BSD rsync rejects is gone too |
| [#4 — exclude folders](https://github.com/sgeisler/cargo-remote/issues/4) | `-x` on the command line, `exclude` in the config |
| [#12 — local path dependencies](https://github.com/sgeisler/cargo-remote/issues/12) | `-w`, plus a clear error when a dependency really is missing |
| [#14 — workspace support](https://github.com/sgeisler/cargo-remote/issues/14) | Target directory comes from `cargo metadata`, so workspace `target/` and a custom `build.target-dir` both work |
| [#15 — specify the port](https://github.com/sgeisler/cargo-remote/issues/15) | `-p`, propagated to both ssh and the ssh rsync spawns |
| [#16](https://github.com/sgeisler/cargo-remote/issues/16), [#26 — environment profile](https://github.com/sgeisler/cargo-remote/issues/26) | Sourced with POSIX `.` and only when the file exists; default is `~/.cargo/env`, not `/etc/profile` |
| [#19 — remote options rejected](https://github.com/sgeisler/cargo-remote/issues/19) | Everything after the cargo command is forwarded, so `cargo remote-3000 build --message-format human` works |
| [#22 — identity file](https://github.com/sgeisler/cargo-remote/issues/22) | `-i` on the command line, `identity_file` in the config |
| [#23 — missing libc headers](https://github.com/sgeisler/cargo-remote/issues/23) | Documented in [Requirements](#requirements) and [Troubleshooting](#troubleshooting) |

**Other repairs**

- The remote build's exit status is propagated, so a failed build fails your
  local command. `ssh -t` previously masked it, which made the tool unusable in
  CI.
- `--build-env` values are validated as `NAME=value`. They were interpolated
  unquoted into a remote shell script, so a value could execute arbitrary
  commands on the build host.
- Paths and arguments reaching the remote shell are quoted.
- A `~` in `temp_dir` is expanded by the remote shell. It was expanded against
  your *local* `$HOME`, sending artifacts to the wrong machine.
- Errors name the failing step instead of panicking in an `unwrap()`.
- Copy-back no longer emits a doubled path separator for the whole target
  directory.
- Rebuilt on clap 4, `cargo_metadata` 0.23, edition 2021, `anyhow` and `dirs`.
  The abandoned `structopt`, `config` 0.11 and `xdg` 2 dependencies are gone.
- Config files merge per remote by name, so a project can override one field.
- 80 unit and end-to-end tests, and CI across Linux, macOS and Windows.
- `cargo remote` works as an alias for `cargo remote-3000`.

Upstream authorship is preserved: this began as Sebastian Geisler's
[cargo-remote](https://github.com/sgeisler/cargo-remote), and the original MIT
license and copyright remain.

## Development

```bash
cargo test              # unit + end-to-end tests
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
cargo build --release
```

The end-to-end tests in `tests/cli.rs` run the real binary against a fake `rsync`
and `ssh` on `PATH` that record their arguments, so they assert on exactly what
would be transferred and run without needing a build server.

Minimum supported Rust version: **1.85** (edition 2021).

## License

MIT — see [LICENSE](LICENSE).

[cargo-remote]: https://github.com/sgeisler/cargo-remote