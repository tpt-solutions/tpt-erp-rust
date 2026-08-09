# Multi-stage build using cargo-chef for dependency layer caching.
# Produces a small final image for the server/CLI binaries.
#
# Build the quickstart (or a server once it exists) example:
#   docker build -t tpt-erp-rust --build-arg BIN=quickstart .

# ---- Stage 1: planning ---------------------------------------------------
FROM --platform=$BUILDPLATFORM ghcr.io/rust-chef/cargo-chef:latest-rust-1.97 AS chef
WORKDIR /app

# ---- Stage 2: dependency caching ----------------------------------------
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
ARG BIN=quickstart
ENV RUSTFLAGS="-C target-feature=+crt-static"
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release --bin ${BIN}

# ---- Stage 3: minimal runtime -------------------------------------------
FROM debian:bookworm-slim AS runtime
WORKDIR /app
COPY --from=builder /app/target/release/${BIN} /usr/local/bin/app
ENTRYPOINT ["/usr/local/bin/app"]
