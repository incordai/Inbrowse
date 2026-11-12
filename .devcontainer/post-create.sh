#!/usr/bin/env bash
# Native build dependencies from docs/Build-from-source.md, cargo-nextest, and
# the crate sources, so the first `cargo build` only compiles.
set -euo pipefail

sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  build-essential cmake clang libclang-dev llvm-dev nasm pkg-config libvpx-dev python3

# cargo-nextest from its prebuilt release (the supported test runner, see AGENTS.md).
curl -LsSf https://get.nexte.st/latest/linux | tar zxf - -C "${CARGO_HOME:-$HOME/.cargo}/bin"

# libvpx for the `media` feature, linked statically.
vpx_lib_dir="$(dirname "$(dpkg -L libvpx-dev | grep '/libvpx\.a$')")"
{
  echo "export VPX_LIB_DIR=\"$vpx_lib_dir\""
  echo "export VPX_INCLUDE_DIR=\"$(pkg-config --variable=includedir vpx)\""
  echo "export VPX_VERSION=\"$(pkg-config --modversion vpx)\""
  echo "export VPX_STATIC=1"
} >> "$HOME/.bashrc"

cargo fetch --locked
