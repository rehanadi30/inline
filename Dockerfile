# ── Stage 1: build the Rust binary ───────────────────────────────────────
FROM rust:1-slim AS build
WORKDIR /app

# Build dependencies first so they cache between code changes.
COPY Cargo.toml ./
COPY Cargo.lock* ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && \
    cargo build --release && \
    rm -rf src

# Now build the real sources.
COPY src ./src
RUN touch src/main.rs && cargo build --release

# ── Stage 2: tiny runtime image ──────────────────────────────────────────
FROM debian:bookworm-slim
WORKDIR /app

COPY --from=build /app/target/release/inline ./inline
COPY public ./public
COPY config.json ./config.json

ENV INLINE_BIND=0.0.0.0:8080
EXPOSE 8080

CMD ["./inline"]
