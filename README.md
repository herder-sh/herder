# herder

herder runs coding agents — Claude Code, Codex, Cursor, and later others — across many
Linux machines. It drives each vendor's own unmodified CLI; it never touches provider
login tokens. A daemon on each machine hosts agent sessions, a vault keeps them durable,
and you watch and steer them from a terminal UI or native apps for macOS, iOS, Linux and
Android, all built on one shared Rust client core.

**Status:** pre-alpha, under construction. Nothing works yet.

## Build and test

Requires Rust (the toolchain is pinned in `rust-toolchain.toml`).

```sh
cargo build --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p herder -- --version
```

## Contributing

Read [AGENTS.md](AGENTS.md) before opening a PR.

## License

Apache-2.0. See [LICENSE](LICENSE).
