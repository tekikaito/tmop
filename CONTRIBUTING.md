# Contributing to TMOP

Thanks for helping. Bug reports, ideas, translations of wording, skins and code are all welcome.

## Reporting a bug or asking for a feature

Open an [issue](https://github.com/tekikaito/tmop/issues/new/choose). For a bug, please include
your operating system and TMOP version (shown in *Settings*, in the About section). If TMOP does not start
on Windows, attach `%LOCALAPPDATA%\TMOP\startup-error.log`. Security problems go through
[SECURITY.md](SECURITY.md), not an issue.

## Building

Install [Rust](https://rustup.rs) and run:

```sh
cargo build --release
cargo test
```

## Screenshots

`TMOP --screenshot=out.png --page=performance --device=disk --window-size=1584x877` makes the
window render its own frame to a PNG after `--wait=` seconds (default 25, so the graphs have
history) and exit; nothing is captured from the desktop. Add `--theme=dark|light`,
`--settings-section=graphs`, or `--frames=N --every=MS` for numbered frames (`out-0001.png`, ...).
The run reads real data from your machine but uses a private settings folder (`--shot-home=<dir>`,
else a temporary one), shows a neutral host and user name and no IP address. Look at every image
before you commit it: the Processes, Services and Connections pages list your running programs.

## Before you open a pull request

CI runs the same checks on Windows, Linux and macOS, so please run them first:

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

- Keep a pull request to one change, and say what it does and why.
- TMOP runs on Windows, Linux and macOS. Where a system can't provide something, TMOP hides it
  instead of showing zeros; please keep to that.
- Wording shown to users is plain, direct English.
- By contributing you agree that your work is released under the project's
  [GPL-3.0](LICENSE) license.
