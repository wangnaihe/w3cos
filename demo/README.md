# W3C OS — Live Demo (Remote Desktop Preview)

Preview W3C OS applications in your browser — no installation required.

How it works: native binary runs on a server → virtual display (Xvfb) → VNC → noVNC (browser access).

在浏览器中预览 W3C OS 应用，无需安装任何东西。原理：服务器运行原生二进制 → 虚拟显示器 → VNC → noVNC。

## Option 1: Hugging Face Spaces (Recommended, Free)

1. Go to [huggingface.co/new-space](https://huggingface.co/new-space) and create a new Space
2. Select **Docker** SDK
3. Upload all files in this directory along with the project's `Cargo.toml`, `crates/`, and `examples/`
4. Or clone the entire repo into the Space and set the Dockerfile path to `demo/Dockerfile` in Space Settings
5. Wait for the build to complete (~15–20 min for the first build due to Rust compilation)
6. Visit your Space URL to see the remote desktop

**The Space README.md requires a YAML header:**

```yaml
---
title: W3C OS Demo
emoji: 🖥️
colorFrom: indigo
colorTo: purple
sdk: docker
app_port: 7860
pinned: false
---
```

**One-line deploy script (一键部署脚本):**

```bash
bash demo/deploy-hf.sh <your-hf-token> [hf-username]
```

## Option 2: Fly.io (Free Tier)

```bash
# Install Fly CLI
curl -L https://fly.io/install.sh | sh

# Login (requires credit card verification, but won't charge)
fly auth login

# Deploy from the project root
fly launch --config demo/fly.toml --dockerfile demo/Dockerfile

# Check deployment status
fly status

# Open in browser
fly open
```

Free tier: 3 shared-cpu-1x machines, 256 MB RAM.
Auto-stop is enabled — machines hibernate when idle, saving your free quota.

## Option 3: Local Docker

```bash
# Build from the project root
docker build -f demo/Dockerfile -t w3cos-demo .

# Run
docker run -p 7860:7860 w3cos-demo

# Open in browser
open http://localhost:7860/vnc.html?autoconnect=true&resize=remote
```

## Option 4: Render (Free)

1. Create a Web Service on [render.com](https://render.com)
2. Connect your GitHub repository
3. Set Dockerfile Path: `demo/Dockerfile`
4. Set Port: `7860`
5. Select the Free tier

## Architecture

```
┌──────────────────────────────────────────┐
│  Docker Container                        │
│                                          │
│  ┌──────────┐    ┌───────┐    ┌───────┐  │
│  │ w3cos    │───▶│ Xvfb  │───▶│x11vnc │  │
│  │ showcase │    │:99    │    │:5900  │  │
│  └──────────┘    └───────┘    └───┬───┘  │
│                                   │      │
│                            ┌──────▼────┐ │
│                            │websockify │ │
│                            │+ noVNC    │ │
│                            │:7860      │ │
│                            └─────┬─────┘ │
└──────────────────────────────────┼───────┘
                                   │
                            ┌──────▼──────┐
                            │  Browser    │
                            │ (any device)│
                            └─────────────┘
```
