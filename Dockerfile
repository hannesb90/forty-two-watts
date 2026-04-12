# Multi-stage build for home-ems
FROM rust:latest AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
RUN cargo build --release

# Runtime — minimal
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /build/target/release/home-ems /app/home-ems
COPY drivers/ /app/drivers/
COPY web/ /app/web/
VOLUME /app/data
EXPOSE 8080
ENTRYPOINT ["/app/home-ems"]
CMD ["/app/data/config.yaml"]
