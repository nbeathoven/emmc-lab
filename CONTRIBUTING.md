# Contributing

Thanks for contributing to `emmc-lab`.

## Scope

Contributions should preserve the core design goals:

- one CLI application, not a loose script bundle
- Linux-first behavior for Raspberry Pi OS / Debian Bookworm
- native Rust hot-path I/O
- graceful fallback when optional features are unavailable
- clear separation of logical syscall bytes and storage-layer bytes
- no misleading claims about physical NAND targeting

## Development Setup

```bash
cargo test
cargo run -- doctor
```

Recommended before opening a pull request:

```bash
cargo fmt
cargo test
```

## Pull Request Guidelines

- keep changes focused
- include tests when behavior changes
- update docs when CLI behavior, installer behavior, or report fields change
- preserve safety checks for destructive raw-device workflows
- do not silently weaken feature-unavailable messaging

## Commit and Release Expectations

- use concise commit messages
- document user-visible changes in `CHANGELOG.md` for release-oriented updates
- do not break the Raspberry Pi installer without updating `rpi/install.sh` and `rpi/README.md`

## Reporting Issues

When filing an issue, include:

- OS and kernel version
- Raspberry Pi model if relevant
- command used
- profile used, if applicable
- expected behavior
- actual behavior
- logs or exported session data if available
