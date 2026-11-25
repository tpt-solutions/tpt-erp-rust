# Contributing to TPT ERP RUST

Thanks for your interest in contributing! This document covers the basics.

## Code of Conduct

Be respectful and constructive. We are building a professional ERP framework used in
safety- and money-sensitive domains; rigor and kindness both matter.

## Getting started

1. Install the pinned toolchain: `rustup show` (reads `rust-toolchain.toml`).
2. Build: `cargo build --workspace`.
3. Test: `cargo test --workspace`.
4. Lint & format: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings`.

## Conventions

- **Edition 2024** across the workspace.
- **Dual-licensed** `MIT OR Apache-2.0`; every crate's `Cargo.toml` must carry
  `license = "MIT OR Apache-2.0"`.
- **No `unsafe`** unless absolutely necessary and clearly justified in a comment.
- **Tests are required** for public behavior, especially for `tpt-erp-primitives`
  (precision, rounding, currency mismatch, ID mixups) and macro-generated code.
- Prefer **compile-time guarantees** over runtime checks. When you can make an invalid
  state unrepresentable, do it.

## Contributing via issues

Contributions are accepted **through GitHub issues only**. Pull requests are not
accepted.

- Search existing issues first to avoid duplicates.
- Open an issue to report a bug, propose a feature, or suggest a change. Include the
  crate or example affected, the steps to reproduce (for bugs), and the expected
  behavior.
- Keep each issue focused on one problem or proposal.
- Roadmap work is tracked in [`todo.md`](./todo.md); reference it in an issue if your
  report relates to a roadmap item.

## License

By opening an issue with suggested code or text, you agree that it may be incorporated
into the project under the MIT and Apache-2.0 licenses, consistent with the rest of
the project.
