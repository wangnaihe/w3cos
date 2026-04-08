# W3C OS — Hugging Face Spaces Live Demo
# Builds w3cos-shell (desktop environment) and serves via noVNC.

FROM rust:bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends git && rm -rf /var/lib/apt/lists/*

WORKDIR /w3cos
ARG CACHE_BUST=2026040402
RUN git clone --depth 1 https://github.com/wangnaihe/w3cos.git .

# Build with CPU rendering (no GPU needed for HF Spaces)
RUN cargo build --release -p w3cos-shell --no-default-features --features cpu-render 2>&1 | tail -10

RUN strip target/release/w3cos-shell && ls -lh target/release/w3cos-shell

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    xvfb x11vnc novnc websockify libfontconfig1 fonts-dejavu-core \
    fonts-noto-cjk dbus-x11 xdotool openbox && rm -rf /var/lib/apt/lists/*

RUN useradd -m -s /bin/bash demo

COPY --from=builder /w3cos/target/release/w3cos-shell /usr/bin/w3cos-shell
COPY start.sh /start.sh
RUN chmod +x /start.sh /usr/bin/w3cos-shell

ENV PORT=7860
EXPOSE 7860

USER demo
CMD ["/start.sh"]
