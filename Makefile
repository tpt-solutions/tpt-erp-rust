# Convenience wrapper around common cargo / docker / tpt commands.
# Uses `.RECIPEPREFIX` so recipes are indented with `>` instead of tabs
# (requires GNU Make >= 3.82 or any BSD make). Run `make help`.

.RECIPEPREFIX = >

.PHONY: help build check fmt clippy test test-ignored doc \
         run-server seed token plugin-build plugin-validate \
         docker-up docker-down

help:
> @echo "TPT ERP RUST — common tasks (make <target>):"
> @echo "  build          Build the whole workspace"
> @echo "  check          cargo check --workspace --all-targets"
> @echo "  fmt            cargo fmt --all"
> @echo "  clippy         cargo clippy --workspace --all-targets -D warnings"
> @echo "  test           cargo test --workspace"
> @echo "  test-ignored   cargo test --workspace -- --ignored (benchmarks/load tests)"
> @echo "  doc            Build API docs (cargo doc --workspace --no-deps)"
> @echo "  run-server     Run the reference ledger server (JWT auth if TPT_JWT_SECRET set)"
> @echo "  seed           Generate demo data (tpt seed-demo)"
> @echo "  token          Mint a dev JWT (tpt token mint; pass ARGS=...)"
> @echo "  plugin-build   Build a wasm plugin (PLUGIN=pricing)"
> @echo "  plugin-validate  Validate a built plugin (PLUGIN=pricing)"
> @echo "  docker-up      docker compose up --build (full local stack)"
> @echo "  docker-down    docker compose down"

build:
> cargo build --workspace

check:
> cargo check --workspace --all-targets

fmt:
> cargo fmt --all

clippy:
> cargo clippy --workspace --all-targets -- -D warnings

test:
> cargo test --workspace

test-ignored:
> cargo test --workspace -- --ignored

doc:
> cargo doc --workspace --no-deps

run-server:
> TPT_JWT_SECRET=$${TPT_JWT_SECRET:-dev-secret} cargo run -p server

seed:
> cargo run -p tpt-erp-cli -- seed-demo

token:
> cargo run -p tpt-erp-cli -- token mint $(ARGS)

plugin-build:
> cargo build --manifest-path examples/plugins/$(PLUGIN)/Cargo.toml --target wasm32-unknown-unknown --release

plugin-validate: plugin-build
> cargo run -p tpt-erp-cli -- token validate examples/plugins/$(PLUGIN)/target/wasm32-unknown-unknown/release/$(PLUGIN).wasm

docker-up:
> docker compose up --build

docker-down:
> docker compose down
