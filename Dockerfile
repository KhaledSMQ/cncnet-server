FROM rust:slim-bookworm AS builder

RUN apt-get update && \
    apt-get install -y bash build-essential && \
    apt-get clean && \
    rm -rf /var/lib/apt/lists/*

COPY . /app
WORKDIR /app

ARG TARGETARCH
RUN if [ "$TARGETARCH" = "arm64" ]; then \
        export RUSTFLAGS="-C target-cpu=neoverse-n1 -C opt-level=3 -C embed-bitcode=yes -C panic=abort"; \
    else \
        export RUSTFLAGS="-C target-cpu=native -C opt-level=3 -C embed-bitcode=yes -C panic=abort"; \
    fi && \
    cargo build --release

FROM debian:bookworm-slim
COPY --from=builder /app/target/release/cncnet-server /usr/local/bin/cncnet-server

RUN useradd -r -s /bin/false cncnet
USER cncnet

EXPOSE 50001 50000 8054 3478 9090

CMD ["cncnet-server"]
