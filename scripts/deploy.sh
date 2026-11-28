#!/usr/bin/env bash
# Deploy the home-video container.
#
# Run ON the NAS, from the repo checkout:
#   ssh dadisc01@nasomv \
#     'cd ~/Documents/git/home-video && bash scripts/deploy.sh'
#
# What it does:
#   1. Keep this checkout fast-forward with origin/main (git is the source of
#      truth for what we deploy — no local-only edits).
#   2. Rebuild the Docker image and restart the home-video container.
#   3. Wait for /health and report the result.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

CONTAINER="home-video"
PORT="${HOST_PORT:-8080}"

echo "==> Pulling latest from origin"
git fetch origin
git pull --ff-only origin main

echo "==> Building and restarting the home-video container"
docker compose up -d --build

echo "==> Waiting for /health"
for i in $(seq 1 72); do
    if curl -fsS "http://127.0.0.1:${PORT}/health" 2>/dev/null; then
        echo ""
        echo "==> /health OK: home-video is live on port ${PORT}"
        docker ps --format "table {{.Names}}\t{{.Status}}\t{{.Ports}}" | grep -E "home-video|NAMES"
        exit 0
    fi
    echo "  still waiting ($((i * 5))s)..."
    sleep 5
done

echo "!! /health did not come up within 6 minutes" >&2
docker ps --format "table {{.Names}}\t{{.Status}}\t{{.Ports}}" | grep -E "home-video|NAMES" >&2 || true
docker logs "${CONTAINER}" --tail 50 >&2 || true
exit 1
