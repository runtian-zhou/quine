# Releasing Quine

Quine publishes tagged releases through the GitHub Actions workflow in [`.github/workflows/release.yml`](../.github/workflows/release.yml).

## Create a release

1. Ensure `main` is green.
2. Create and push a tag such as `v0.1.0`.
3. Wait for the `Release` workflow to finish.

The workflow builds `quine` for:

- `x86_64-unknown-linux-gnu`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`

It then creates a GitHub release containing:

- platform tarballs for `quine`
- `SHA256SUMS`
- `quine.rb`, a Homebrew formula template pinned to that exact release
- an updated [`Formula/quine.rb`](../Formula/quine.rb) committed back to the default branch for stable releases

## Update a Homebrew tap

This repository can act as its own tap. Because the repository is named `quine` rather than `homebrew-quine`, users need the explicit two-argument tap form:

```bash
brew tap runtian-zhou/quine https://github.com/runtian-zhou/quine
brew install quine
```

Homebrew's one-argument shorthand:

```bash
brew tap runtian-zhou/quine
```

will look for `https://github.com/runtian-zhou/homebrew-quine`, not this repository.

If another tapped formula named `quine` ever conflicts, users can still fall back to the fully qualified name:

```bash
brew install runtian-zhou/quine/quine
```
