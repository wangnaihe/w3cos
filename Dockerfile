# W3C OS — Multi-stage build
# Stage 1: Build the compiler + runtime
# Stage 2: Minimal image with just the binary

# ============================================================
# Stage 1: Builder
# ============================================================
FROM rust:1.94-bookworm AS builder

WORKDIR /w3cos
COPY . .

RUN cargo build --release --workspace

# Build the showcase example
RUN ./target/release/w3cos build examples/showcase/app.ts -o /w3cos/showcase --release

# ============================================================
# Stage 2: Minimal runtime
# ============================================================
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    libfontconfig1 \
    fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/*

# Copy the W3C OS CLI and examples
COPY --from=builder /w3cos/target/release/w3cos /usr/bin/w3cos
COPY --from=builder /w3cos/showcase /usr/bin/w3cos-showcase
COPY --from=builder /w3cos/examples /apps

WORKDIR /apps

ENTRYPOINT ["w3cos"]
CMD ["--help"]

# ============================================================
# Usage:
#   docker build -t w3cos .
#   docker run w3cos build /apps/hello/app.ts -o /tmp/hello --release
#   docker run w3cos --help
# ============================================================
