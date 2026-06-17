#!/usr/bin/env bash
# Build the browser (wasm) client and serve it locally.
set -e
cd "$(dirname "$0")"
echo "building wasm (release)…"
cargo build -p ironvein --target wasm32-unknown-unknown --release
cp target/wasm32-unknown-unknown/release/ironvein.wasm ./ironvein.wasm
echo "ironvein.wasm: $(du -h ironvein.wasm | cut -f1)"
PORT="${1:-8080}"
echo
echo "serving on http://localhost:$PORT  —  open it in your browser (Ctrl-C to stop)"
python3 -m http.server "$PORT"
