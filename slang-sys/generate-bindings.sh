#!/bin/sh
# Regenerates src/bindings.rs and src/functions.rs from a Slang install's
# include/slang.h, through the crate's `bindgen` feature (needs libclang),
# and the safe crate's Rust enums in ../src/enums.rs.
#
#   SLANG_DIR=/path/to/slang ./generate-bindings.sh
#
# SLANG_INCLUDE_DIR or VULKAN_SDK work as well, as for the build script.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
enums="$(cd "$here/.." && pwd)/src/enums.rs"
SHADER_SLANG_SYS_BINDINGS_OUT="$here/src" \
SHADER_SLANG_ENUMS_OUT="$enums" \
	cargo build --manifest-path "$here/Cargo.toml" --features bindgen "$@"
# bindgen formats its own output; the enums are formatted here.
rustfmt --edition 2024 "$enums"
echo "wrote $here/src/bindings.rs, $here/src/functions.rs and $enums"
