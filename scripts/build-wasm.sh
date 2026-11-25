#!/usr/bin/env bash
# Reproducible wasm build (needs rustup; the toolchain is pinned in rust-toolchain.toml).
#
# Why nightly + build-std: stable Rust >= 1.82 emits bulk-memory instructions in the precompiled
# std, which CosmWasm's static validation rejects. Rebuilding std with target-cpu=mvp avoids it.
# Hub exposes capabilities up to cosmwasm_2_2 (wasmd v0.60.10).
#
# Reproducibility: the toolchain is a dated nightly, the dependencies come from the committed Cargo.lock
# (--locked), and the paths of the builder (home directory, checkout, toolchain) are remapped so that
# they do not end up in the binary: another builder with the same toolchain gets the same sha256.
# `artifacts/BUILD-INFO.txt` records what was used. Compare the result with the published checksum and
# with what is stored on chain: `gaiad q wasm code-info <code_id> -o json | jq -r .checksum` (lowercase
# hex, the same as `sha256sum`).
set -euo pipefail
cd "$(dirname "$0")/.."
. "$HOME/.cargo/env"

TOOLCHAIN=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)
SYSROOT=$(rustc +"$TOOLCHAIN" --print sysroot)
CARGO_DIR=${CARGO_HOME:-$HOME/.cargo}

RUSTFLAGS="-C target-cpu=mvp -C link-arg=-s -C link-arg=--allow-undefined \
 --remap-path-prefix=$CARGO_DIR/registry/src=/cargo \
 --remap-path-prefix=$SYSROOT=/rustc \
 --remap-path-prefix=$PWD=/code" \
  cargo +"$TOOLCHAIN" build --locked --release --lib --target wasm32-unknown-unknown -Zbuild-std=std,panic_abort

mkdir -p artifacts
cp target/wasm32-unknown-unknown/release/liatom_hub.wasm artifacts/liatom_hub.wasm
sha256sum artifacts/liatom_hub.wasm | tee artifacts/checksums.txt
{
  echo "rustc: $(rustc +"$TOOLCHAIN" --version)"
  echo "cargo: $(cargo +"$TOOLCHAIN" --version)"
  echo "cosmwasm-check: $(cosmwasm-check --version)"
  echo "git: $(git rev-parse HEAD 2>/dev/null || echo unknown)"
} > artifacts/BUILD-INFO.txt

cosmwasm-check \
  --available-capabilities iterator,staking,stargate,cosmwasm_1_1,cosmwasm_1_2,cosmwasm_1_3,cosmwasm_1_4,cosmwasm_2_0,cosmwasm_2_1,cosmwasm_2_2 \
  artifacts/liatom_hub.wasm
