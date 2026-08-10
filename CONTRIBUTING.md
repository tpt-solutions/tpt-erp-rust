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

## Submitting changes

- Keep PRs focused.
- Ensure CI (fmt, clippy with `-D warnings`, tests) passes.
- Update [`todo.md`](./todo.md) checkboxes for any roadmap items you complete.

## License

By contributing, you agree that your contributions will be dual-licensed under the
MIT and Apache-2.0 licenses, consistent with the rest of the project.
