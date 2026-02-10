FROM rust:1.83-bookworm AS builder

RUN apt-get update && apt-get install -y musl-tools && \
    rustup target add aarch64-unknown-linux-musl

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main(){}' > src/main.rs && \
    cargo build --release --target aarch64-unknown-linux-musl && \
    rm -rf src

COPY src ./src
RUN touch src/main.rs && \
    cargo build --release --target aarch64-unknown-linux-musl

FROM alpine:3.19
RUN apk add --no-cache ca-certificates
COPY --from=builder /app/target/aarch64-unknown-linux-musl/release/github-project-sync /usr/local/bin/
EXPOSE 3000
CMD ["github-project-sync"]
