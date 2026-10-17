#!/bin/sh
# Runs the crate's tests where no Slang install can be reached: under
# `env -i`, with PATH=/usr/bin:/bin, no SLANG_DIR, SLANG_LIB_DIR,
# VULKAN_SDK or LD_LIBRARY_PATH, and a fresh, empty HOME, so the embedded
# library is written out from scratch.
#
#   scripts/test-no-env.sh [extra cargo test arguments]
#
# The build itself runs in the caller's environment (it needs cargo, and
# the network or the download cache); only the test binaries are scrubbed.
# The test `no_env::environment_is_clean` runs only here and checks the
# claim from inside: no Slang variable set, no slangc on PATH, the library
# opened from the fresh HOME's cache.
set -eu

repo=$(cd "$(dirname "$0")/.." && pwd)
target=${CARGO_TARGET_DIR:-$repo/target}
home=$target/no-env-home
minimal_path=/usr/bin:/bin

if env -i PATH="$minimal_path" sh -c 'command -v slangc' >/dev/null 2>&1; then
	echo "slangc is on $minimal_path; this run would not prove anything" >&2
	exit 1
fi

executables=$(cargo test --manifest-path "$repo/Cargo.toml" --no-run --message-format=json "$@" |
	sed -n 's/.*"executable":"\([^"]*\)".*/\1/p')
[ -n "$executables" ] || { echo "no test executables built" >&2; exit 1; }

status=0
for exe in $executables; do
	if command -v ldd >/dev/null 2>&1 && ldd "$exe" | grep -i slang; then
		echo "$exe links a Slang library" >&2
		status=1
	fi
	rm -rf "$home"
	mkdir -p "$home"
	echo "== $exe (HOME=$home, PATH=$minimal_path)"
	(cd "$repo" && env -i HOME="$home" PATH="$minimal_path" SHADER_SLANG_TEST_NO_ENV=1 "$exe") || status=1
done
exit $status
