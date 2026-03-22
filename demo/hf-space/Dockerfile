# W3C OS — Hugging Face Spaces Live Demo
# Self-contained: clones from GitHub and builds everything.

# ============================================================
# Stage 1: Build the demo binary directly from workspace
# ============================================================
FROM rust:1.85-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends git && rm -rf /var/lib/apt/lists/*

WORKDIR /w3cos
RUN git clone --depth 1 https://github.com/wangnaihe/w3cos.git .

# Build the demo binary as a workspace member — no temp projects, no arch issues
RUN cargo build --release -p w3cos-demo 2>&1 | tail -5

RUN strip target/release/w3cos-demo && ls -lh target/release/w3cos-demo

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

COPY --from=builder /w3cos/target/release/w3cos-demo /usr/bin/w3cos-demo
COPY start.sh /start.sh
RUN chmod +x /start.sh /usr/bin/w3cos-demo

ENV PORT=7860
EXPOSE 7860

USER demo
CMD ["/start.sh"]
