FROM rust:1.84-slim AS builder
WORKDIR /build
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/cpfp-me /usr/local/bin/
COPY static/ /app/static/
WORKDIR /app
ENTRYPOINT ["cpfp-me"]
CMD ["config.toml"]
