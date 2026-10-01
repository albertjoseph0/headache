# Build
FROM rust:1.97-slim-bookworm AS build
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends cmake clang && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY src src
COPY data data
COPY examples examples
RUN cargo build --release --locked

# Run
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=build /app/target/release/headache /usr/local/bin/headache
COPY web web
ENV PORT=8080 DATABASE_PATH=/data/headache.db STATIC_DIR=/app/web
VOLUME /data
EXPOSE 8080
CMD ["headache"]
