# Build recipe only; no image was built or deployed in this environment.
FROM rust:1.99.1-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY assets ./assets
RUN cargo build --locked --release

# Node is present for the separately configured trusted-source build Job.
FROM node:24-bookworm-slim AS runtime
RUN groupadd --gid 10001 artifact && useradd --uid 10001 --gid artifact --create-home artifact
COPY --from=builder /build/target/release/artifact-board /usr/local/bin/artifact-board
USER 10001:10001
ENV ARTIFACT_STORE=firestore
ENV ARTIFACT_BUILD_MODE=container
ENV ARTIFACT_NODE_BIN_DIR=/usr/local/bin
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/artifact-board"]
