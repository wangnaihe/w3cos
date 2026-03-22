#!/bin/bash
# Deploy W3C OS demo to Hugging Face Spaces
# Usage: bash demo/deploy-hf.sh <your-hf-token> [hf-username]
set -e

HF_TOKEN="${1:?Usage: bash demo/deploy-hf.sh <hf-token> [hf-username]}"
HF_USER="${2:-wangnaihe}"
SPACE_NAME="w3cos-demo"
REPO_URL="https://${HF_USER}:${HF_TOKEN}@huggingface.co/spaces/${HF_USER}/${SPACE_NAME}"

echo "=== W3C OS → Hugging Face Spaces ==="
echo "Space: ${HF_USER}/${SPACE_NAME}"

# Create Space via API
echo "[1/4] Creating Space..."
curl -s -X POST "https://huggingface.co/api/repos/create" \
  -H "Authorization: Bearer ${HF_TOKEN}" \
  -H "Content-Type: application/json" \
  -d "{\"type\":\"space\",\"name\":\"${SPACE_NAME}\",\"sdk\":\"docker\",\"private\":false}" \
  || echo "(Space may already exist, continuing...)"
echo ""

# Clone / init the Space repo
TMPDIR=$(mktemp -d)
echo "[2/4] Cloning Space to ${TMPDIR}..."
git clone "${REPO_URL}" "${TMPDIR}/space" 2>/dev/null \
  || (mkdir -p "${TMPDIR}/space" && cd "${TMPDIR}/space" && git init && git remote add origin "${REPO_URL}")

# Copy files
echo "[3/4] Copying demo files..."
cp "$(dirname "$0")/hf-space/Dockerfile" "${TMPDIR}/space/Dockerfile"
cp "$(dirname "$0")/hf-space/start.sh"   "${TMPDIR}/space/start.sh"
cp "$(dirname "$0")/hf-space/README.md"  "${TMPDIR}/space/README.md"

# Push
echo "[4/4] Pushing to Hugging Face..."
cd "${TMPDIR}/space"
git add -A
git commit -m "Deploy W3C OS live demo" --allow-empty 2>/dev/null || true
git push origin main 2>/dev/null || git push origin master || git push --set-upstream origin main

echo ""
echo "=== Done! ==="
echo "Space URL: https://huggingface.co/spaces/${HF_USER}/${SPACE_NAME}"
echo ""
echo "First build takes ~15-20 min (Rust compilation)."
echo "After that, visit the URL above to see the live demo."

# Cleanup
rm -rf "${TMPDIR}"
