#!/usr/bin/env bash
# Builds release downloads into dist/: one .tar.gz per Linux target holding a
# static `server` binary (runs on any distro) and the web/ files it serves,
# plus SHA256SUMS. CI runs this; it works locally too.
#   ./package.sh                         # x86_64 + aarch64
#   ./package.sh x86_64-unknown-linux-musl
# Set RC_BUILD to stamp a version into `server --version`.
set -euo pipefail
cd "$(dirname "$0")"

targets=("$@")
if [ ${#targets[@]} -eq 0 ]; then
  targets=(x86_64-unknown-linux-musl aarch64-unknown-linux-musl)
fi
rustup target add wasm32-unknown-unknown "${targets[@]}" >/dev/null

cargo build --release -p client --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/client.wasm web/client.wasm

# musl targets link statically with Rust's bundled linker, so no cross C
# toolchain is needed for aarch64.
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld

rm -rf dist
mkdir -p dist
for t in "${targets[@]}"; do
  cargo build --release -p server --target "$t"
  name="retro-cycles-linux-${t%%-*}"
  mkdir -p "dist/$name/web"
  cp "target/$t/release/server" "dist/$name/"
  cp web/index.html web/main.js web/client.wasm "dist/$name/web/"
  cp README.md "dist/$name/"
  tar -C dist -czf "dist/$name.tar.gz" "$name"
  rm -rf "dist/${name:?}"
done
(cd dist && sha256sum -- *.tar.gz > SHA256SUMS)
ls -l dist
