FROM rust:slim-bookworm AS builder

RUN apt-get update && \
    apt-get install -y bash build-essential && \
    apt-get clean && \
    rm -rf /var/lib/apt/lists/*

COPY . /app
WORKDIR /app
ARG TARGETARCH
RUN if [ "$TARGETARCH" = "arm64" ]; then \
        bash build-arm.sh || true; \
    else \
        bash build.sh || true; \
    fi

FROM debian:bookworm-slim
COPY --from=builder /app/target/release/cncnet-server /usr/local/bin/cncnet-server

# Run as non-root user
RUN useradd -r -s /bin/false cncnet
USER cncnet

CMD ["cncnet-server"]
