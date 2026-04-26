# Quine

Quine is a self-bootstrapping AI agent harness with a local daemon, CLI, and SDKs.

## Install with Homebrew

This repository acts as its own Homebrew tap. Because the repository is named `quine` rather than `homebrew-quine`, use the explicit tap URL:

```bash
brew tap runtian-zhou/quine https://github.com/runtian-zhou/quine
brew install quine
```

Verify the install:

```bash
quine version
```

## Build from Source

```bash
cargo build
cargo run --bin quine -- chat
```

## Releasing

See [docs/releasing.md](docs/releasing.md) for the tagged release and Homebrew update flow.
