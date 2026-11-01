FROM rust:1-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
        curl \
        ca-certificates \
        perl \
        make \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Cache dependency compilation by copying manifests first
COPY Cargo.toml Cargo.lock ./
COPY vendor/ vendor/
COPY crates/inbrowse-dom/Cargo.toml       crates/inbrowse-dom/Cargo.toml
COPY crates/inbrowse-net/Cargo.toml       crates/inbrowse-net/Cargo.toml
COPY crates/inbrowse/Cargo.toml   crates/inbrowse/Cargo.toml
COPY crates/inbrowse-cdp/Cargo.toml       crates/inbrowse-cdp/Cargo.toml
COPY crates/inbrowse-js/Cargo.toml        crates/inbrowse-js/Cargo.toml
COPY crates/inbrowse-mcp/Cargo.toml       crates/inbrowse-mcp/Cargo.toml
COPY crates/inbrowse-render/Cargo.toml    crates/inbrowse-render/Cargo.toml
COPY crates/inbrowse-cli/Cargo.toml       crates/inbrowse-cli/Cargo.toml
COPY crates/inbrowse-sdk/Cargo.toml           crates/inbrowse-sdk/Cargo.toml

# Create stub src files so cargo can resolve the dependency graph
RUN for crate in inbrowse-dom inbrowse-net inbrowse inbrowse-cdp inbrowse-js inbrowse-mcp inbrowse-render inbrowse; do \
        mkdir -p crates/$crate/src && echo "// stub" > crates/$crate/src/lib.rs; \
    done && \
    mkdir -p crates/inbrowse-cli/src && \
    echo "fn main() {}" > crates/inbrowse-cli/src/main.rs && \
    echo "fn main() {}" > crates/inbrowse-cli/src/worker.rs

RUN cargo build --release --features render --bin inbrowse --bin inbrowse-worker 2>/dev/null || true

ARG INBROWSE_VERSION

# Copy real sources and build
COPY crates/ crates/
RUN echo "Building Inbrowse version ${INBROWSE_VERSION:-from Cargo.toml}" && \
    touch crates/*/src/*.rs && cargo build --release --features render --bin inbrowse --bin inbrowse-worker

# ---

# distroless/cc: glibc + libgcc + CA certs only — no shell, no package manager.
#
# `:nonroot` runs as uid/gid 65532 instead of root. Inbrowse executes untrusted
# page JavaScript in-process through V8, so a V8 exploit lands with the
# process's privileges; there is no reason for those to be root's. The image
# needs no privileged operation: it binds an unprivileged port, reads the CA
# bundle, and writes only to the storage dir and a temp dir.
#
# The tag is deliberately not pinned to a digest. distroless is rebuilt often
# with base-layer security patches, and tracking the tag picks those up; a
# digest pin would freeze them until someone remembers to bump it, which for a
# *base* image trades a real ongoing risk for a theoretical one.
FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder /build/target/release/inbrowse /inbrowse
COPY --from=builder /build/target/release/inbrowse-worker /inbrowse-worker

EXPOSE 9222

# Bind to 0.0.0.0 *inside the container*: a container-loopback bind is
# unreachable through `-p`, so this is required for the port to work at all.
# It is not a licence to expose the port. Non-loopback binds require
# INBROWSE_CDP_TOKEN; publish to host loopback as an additional boundary.
# The native binary still defaults to 127.0.0.1.
#
ENTRYPOINT ["/inbrowse"]
CMD ["serve", "--port", "9222", "--host", "0.0.0.0"]
