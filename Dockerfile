FROM rust:1.99.1-bookworm@sha256:e12566d5abc17a0da741628782cdb504b68b8961f5bfcf677936bf3e8b32de29 AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY assets ./assets
RUN --mount=type=cache,id=artifact-board-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=artifact-board-target-amd64,target=/build/target \
    cargo build --locked --release && cp target/release/artifact-board /build/artifact-board

# Node is present for the separately configured trusted-source build Job.
FROM node:24-trixie-slim@sha256:7f486cabe892bff8fef04c0f7da8dba84a560be3e4e2740b7a86afc57ebec315 AS runtime
RUN apt-get update && apt-get upgrade -y \
    && rm -rf /var/lib/apt/lists/* \
    && npm install --global npm@12.2.0 \
    && mkdir -p /tmp/npm-securitydeps \
    && printf '%s\n' '{"private":true}' > /tmp/npm-securitydeps/package.json \
    && npm install --prefix /tmp/npm-securitydeps --install-strategy=nested --omit=dev \
        --ignore-scripts --no-audit --no-fund --save-exact brace-expansion@5.0.11 undici@6.29.1 \
    && rm -rf /usr/local/lib/node_modules/npm/node_modules/brace-expansion \
        /usr/local/lib/node_modules/npm/node_modules/undici \
    && cp -a /tmp/npm-securitydeps/node_modules/brace-expansion \
        /tmp/npm-securitydeps/node_modules/undici /usr/local/lib/node_modules/npm/node_modules/ \
    && rm -rf /tmp/npm-securitydeps /root/.npm \
    && groupadd --gid 10001 artifact \
    && useradd --uid 10001 --gid artifact --create-home artifact
COPY --from=builder /build/artifact-board /usr/local/bin/artifact-board
USER 10001:10001
ENV ARTIFACT_STORE=firestore
ENV ARTIFACT_BUILD_MODE=container
ENV ARTIFACT_NODE_BIN_DIR=/usr/local/bin
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/artifact-board"]
