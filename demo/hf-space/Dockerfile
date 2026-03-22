# W3C OS — Hugging Face Spaces Live Demo
# Self-contained: clones from GitHub and builds everything.

# ============================================================
# Stage 1: Build the w3cos compiler + showcase binary
# ============================================================
FROM rust:1.85-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends git && rm -rf /var/lib/apt/lists/*

WORKDIR /w3cos
RUN git clone --depth 1 https://github.com/wangnaihe/w3cos.git .

RUN cargo build --release --workspace 2>&1 | tail -5

RUN ./target/release/w3cos build examples/showcase/app.tsx -o /w3cos/showcase --release \
    || cp /w3cos/target/release/w3cos-app /w3cos/showcase 2>/dev/null \
    || true

RUN ./target/release/w3cos build examples/counter/app.tsx -o /w3cos/counter --release \
    || cp /w3cos/target/release/w3cos-app /w3cos/counter 2>/dev/null \
    || true

RUN ls -lh /w3cos/showcase /w3cos/counter 2>/dev/null; \
    strip /w3cos/showcase 2>/dev/null || true; \
    strip /w3cos/counter 2>/dev/null || true

# Ensure both binaries exist (create dummy if counter failed)
RUN test -f /w3cos/counter || cp /w3cos/showcase /w3cos/counter

# ============================================================
# Stage 2: Runtime with virtual display + noVNC
# ============================================================
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    xvfb \
    x11vnc \
    novnc \
    websockify \
    libfontconfig1 \
    fonts-dejavu-core \
    fonts-noto-cjk \
    dbus-x11 \
    xdotool \
    openbox \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -m -s /bin/bash demo

COPY --from=builder /w3cos/showcase /usr/bin/w3cos-showcase
COPY --from=builder /w3cos/counter  /usr/bin/w3cos-counter
COPY start.sh /start.sh
RUN chmod +x /start.sh /usr/bin/w3cos-showcase /usr/bin/w3cos-counter 2>/dev/null || true

ENV PORT=7860
EXPOSE 7860

USER demo
CMD ["/start.sh"]
