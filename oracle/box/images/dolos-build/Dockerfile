FROM rust:1.98.0-trixie@sha256:8aea3a845c48caac4091a8d0e6494d213061f8f014dc2f5a5f1a88a1a8c9f301
RUN printf 'APT::Snapshot "20260905T160000Z";\nAcquire::Check-Valid-Until "false";\n' >/etc/apt/apt.conf.d/snapshot \
 && apt-get update \
 && apt-get install -y --no-install-recommends protobuf-compiler cmake clang pkg-config libssl-dev \
 && rm -rf /var/lib/apt/lists/*
