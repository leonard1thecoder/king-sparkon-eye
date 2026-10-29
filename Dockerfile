# King Sparkon Eye: Rust service image (issue #13).
# Builds on the official Rust image and runs on a slim, non-root runtime.
# No credentials are baked in. Provider keys and other secrets are supplied at run time.

# ---- build ----
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
RUN cargo build --release --locked --bin king_sparkon_eye

# ---- runtime ----
FROM debian:bookworm-slim AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 10001 eye \
 && useradd --system --uid 10001 --gid eye --no-create-home --shell /usr/sbin/nologin eye

WORKDIR /opt/eye
COPY --from=build /src/target/release/king_sparkon_eye /usr/local/bin/king_sparkon_eye
# Vendored inference artifacts, verified by tests/artifacts_test.rs against artifacts/manifest.json.
COPY artifacts ./artifacts

# The ONNX Runtime library is loaded dynamically (ort load-dynamic). Path matches the vendored artifact.
ENV ORT_DYLIB_PATH=/opt/eye/artifacts/onnxruntime/1.31.1/linux-x86_64/libonnxruntime.so.1.31.1

USER eye
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/king_sparkon_eye"]
