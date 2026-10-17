# shader-slang-sys

FFI bindings for the [Slang](https://github.com/shader-slang/slang/) shader
language compiler, used by the [`shader-slang`](../README.md) crate. The
bindings are generated from Slang's `slang.h` and checked in; by default the
build downloads the pinned Slang release for the target, verifies it, and
embeds its library files, which are opened at run time. See the main README
for the features, the build-time settings and what happens at run time.

Licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
