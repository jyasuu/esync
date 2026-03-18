# ── Build stage ──────────────────────────────────────────────────────────────
FROM rust:1.78-slim-bookworm AS builder

WORKDIR /build

# Cache dependencies separately
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main(){}' > src/main.rs
RUN cargo build --release 2>/dev/null; rm -f target/release/esync

# Real build
COPY src ./src
RUN cargo build --release

# ── Runtime stage ─────────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /build/target/release/esync /usr/local/bin/esync

EXPOSE 4000
ENTRYPOINT ["esync"]
CMD ["serve"]
