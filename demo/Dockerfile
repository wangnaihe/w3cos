# W3C OS — Live Demo (Remote Desktop via noVNC)
#
# Builds the showcase binary, then serves it through a virtual display
# accessible from any browser — like a remote desktop.
#
# Targets: Hugging Face Spaces (port 7860), Fly.io, Render, etc.

# ============================================================
# Stage 1: Build the w3cos compiler + showcase binary
# ============================================================
FROM rust:1.85-bookworm AS builder

WORKDIR /w3cos
COPY Cargo.toml Cargo.toml
COPY crates crates
COPY examples examples

RUN cargo build --release --workspace 2>&1 | tail -5

RUN ./target/release/w3cos build examples/showcase/app.tsx -o /w3cos/showcase --release \
    && strip /w3cos/showcase \
    && ls -lh /w3cos/showcase

# Also build counter (interactive demo); fall back to showcase if it fails
RUN ./target/release/w3cos build examples/counter/app.tsx -o /w3cos/counter --release \
    && strip /w3cos/counter \
    || cp /w3cos/showcase /w3cos/counter

# ============================================================
# Stage 2: Minimal runtime with virtual display + noVNC
# ============================================================
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    xvfb \
    x11vnc \
    novnc \
    websockify \
    supervisor \
    libfontconfig1 \
    fonts-dejavu-core \
    fonts-noto-cjk \
    dbus-x11 \
    xdotool \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /w3cos/showcase /usr/bin/w3cos-showcase
COPY --from=builder /w3cos/counter  /usr/bin/w3cos-counter
COPY demo/start.sh /start.sh
COPY demo/supervisord.conf /etc/supervisor/conf.d/w3cos.conf
RUN chmod +x /start.sh

# noVNC serves on this port
ENV PORT=7860
EXPOSE 7860

CMD ["/start.sh"]
