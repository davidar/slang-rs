#!/bin/sh
# Regenerates src/bindings.rs and src/functions.rs from a Slang install's
# include/slang.h, through the crate's `bindgen` feature (needs libclang).
#
#   SLANG_DIR=/path/to/slang ./generate-bindings.sh
#
# SLANG_INCLUDE_DIR or VULKAN_SDK work as well, as for the build script.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
SHADER_SLANG_SYS_BINDINGS_OUT="$here/src" \
	cargo build --manifest-path "$here/Cargo.toml" --features bindgen "$@"
echo "wrote $here/src/bindings.rs and $here/src/functions.rs"
