FROM rust:1.70.0 as builder
COPY . .
RUN cargo build --release

FROM debian:buster-slim
COPY --from=builder /target/release/swarmdon /usr/local/bin/swarmdon
RUN apt-get update && apt-get install -y ca-certificates openssl
ENTRYPOINT ["/usr/local/bin/swarmdon"]
