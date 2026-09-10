# Contributing

Senda Link is a CoreAudio HAL driver. It runs inside `coreaudiod`, so a fault in it affects
every application's audio. Changes are reviewed with that in mind, and every release ships
signed, so reviews can take time.

- Bug reports and questions: open an issue.
- Small fixes: open a pull request.
- New features or behaviour changes: open an issue first so we can agree the approach.

## Build and test

Needs a stable Rust toolchain and Xcode Command Line Tools.

    cargo test --workspace
    cargo xtask bundle

`cargo fmt` and `cargo clippy --workspace --all-targets -- -D warnings` must be clean.
`src/engine/` forbids `unsafe`; all `unsafe` lives in `src/ffi/` with a `// SAFETY:` comment.

## Sign your commits

Every commit needs a [Developer Certificate of Origin](https://developercertificate.org/)
sign-off, which certifies you have the right to submit the change under this project's licence:

    git commit -s

Unsigned commits fail the DCO check. Use `git rebase --signoff` to fix a branch, then force-push.

## Security

Do not open a public issue for a vulnerability. See [SECURITY.md](SECURITY.md).
