ARG RUST_IMAGE=docker.io/library/rust:1.99.1-alpine3.23
FROM ${RUST_IMAGE} AS build

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates/sigen-modbus ./crates/sigen-modbus
COPY crates/sigen-server ./crates/sigen-server

# The Alpine toolchain produces a static musl binary; no libc or shell is needed
# in the runtime image. The dashboard assets are embedded in the executable.
ENV CARGO_PROFILE_RELEASE_STRIP=symbols \
    CARGO_PROFILE_RELEASE_LTO=thin \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
RUN cargo build --locked --release --package sigen-server --bin sigen-server
RUN mkdir -p /out/data && chown 65532:65532 /out/data

FROM scratch
COPY --from=build /build/target/release/sigen-server /sigen-server
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=build --chown=65532:65532 /out/data /data

USER 65532:65532
WORKDIR /data
ENV SIGEN_BIND=0.0.0.0:3000 \
    SIGEN_ALERTS_FILE=/data/alerts.json \
    SIGEN_WRITE_AUDIT_FILE=/data/writes.jsonl \
    SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
EXPOSE 3000
STOPSIGNAL SIGTERM
ENTRYPOINT ["/sigen-server"]
