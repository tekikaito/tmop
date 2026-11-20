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
