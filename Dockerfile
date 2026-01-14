# 1. Build Stage
FROM rust:latest AS builder

WORKDIR /app
COPY . .

# Build release
RUN cargo build --release

# 2. Runtime Stage
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app

# Copy binary from builder (matches the name in Cargo.toml)
COPY --from=builder /app/target/release/apex_omega /app/apex_omega

# Run the binary
CMD ["./apex_omega"]
