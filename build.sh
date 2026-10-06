#!/bin/sh
# Builds the wasm client into web/ and the server binary.
set -e
cd "$(dirname "$0")"
rustup target add wasm32-unknown-unknown >/dev/null 2>&1 || true
cargo build --release -p client --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/client.wasm web/client.wasm
cargo build --release -p server
echo "run: ./target/release/server --addr 0.0.0.0:8080 --bots 4"
