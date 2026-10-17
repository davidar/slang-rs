<div align="center">

# shader-slang
**Rust bindings for the [Slang](https://github.com/shader-slang/slang/) shader language compiler**

</div>

Supports both the modern compilation and reflection API.

This is a fork of [FloatyMonkey/slang-rs](https://github.com/FloatyMonkey/slang-rs). By default the crate carries Slang itself: a binary that depends on it compiles shaders on a machine with no Slang installed and no environment variables set. The [CHANGELOG](CHANGELOG.md) lists what differs from upstream, and what is known not to work yet.

Upstream mostly reflects the needs of FloatyMonkey's own [engine](https://github.com/FloatyMonkey/engine); contributions are more than welcome.

## Example

```rust
let global_session = slang::GlobalSession::new().unwrap();

// All compiler options are available through this builder.
let session_options = slang::CompilerOptions::default()
	.optimization(slang::OptimizationLevel::High)
	.matrix_layout_row(true);

let target_desc = slang::TargetDesc::default()
	.format(slang::CompileTarget::Spirv)
	.profile(global_session.find_profile("glsl_450"));

let targets = [target_desc];

let session_desc = slang::SessionDesc::default()
	.targets(&targets)
	.search_paths(["shaders/directory"])
	.options(&session_options);

let session = global_session.create_session(&session_desc).unwrap();
let module = session.load_module("filename.slang").unwrap();
let entry_point = module.find_entry_point_by_name("main").unwrap();

let program = session
	.create_composite_component_type(&[module.into(), entry_point.into()])
	.unwrap();

let linked_program = program.link().unwrap();

// Entry point to the reflection API.
let reflection = linked_program.layout(0).unwrap();

let shader_bytecode = linked_program.entry_point_code(0, 0).unwrap();
```

### Diagnostics on success

Slang reports warnings, and some errors it recovers from, on calls that succeed. The plain methods above report diagnostics only when they fail. Each has a `..._with_diagnostics` sibling that appends everything Slang said to a `Diagnostics`, on success and failure alike:

```rust
let mut diagnostics = slang::Diagnostics::new();
let module = session.load_module_with_diagnostics("filename.slang", &mut diagnostics)?;
let entry_point = module.find_and_check_entry_point_with_diagnostics(
	"main",
	slang::Stage::Compute,
	&mut diagnostics,
)?;
let program = session.create_composite_component_type_with_diagnostics(
	&[module.into(), entry_point.into()],
	&mut diagnostics,
)?;
let linked_program = program.link_with_diagnostics(&mut diagnostics)?;
let code = linked_program.entry_point_code_with_diagnostics(0, 0, &mut diagnostics)?;
assert!(diagnostics.is_empty(), "{diagnostics}");
```

A build that treats warnings as errors needs these. So does any caller that wants to know its SPIR-V was optimised. When Slang cannot load its SPIR-V optimiser module (`slang-glslang`), the compile still succeeds. It returns different, unoptimised code, and only an `error[E00100]: failed to load downstream compiler 'spirv-opt'` in the diagnostics says so.

## Installation

Add `shader-slang` to the `[dependencies]` section of your `Cargo.toml`. Nothing else is needed on the supported targets.

### What the build downloads

With the default `embed` feature, the build script of `shader-slang-sys` downloads the official Slang release archive for the target from GitHub:

```
https://github.com/shader-slang/slang/releases/download/v2026.14.1/slang-2026.14.1-<asset>.tar.gz
```

| Target | Asset | Embedded files |
|---|---|---|
| `x86_64-unknown-linux-gnu` | `linux-x86_64-glibc-2.28` | `libslang-compiler.so.0.2026.14.1`, `libslang-glslang-2026.14.1.so` |
| `aarch64-unknown-linux-gnu` | `linux-aarch64-glibc-2.28` | the same |
| `aarch64-apple-darwin` | `macos-aarch64` | `libslang-compiler.0.2026.14.1.dylib`, `libslang-glslang-2026.14.1.dylib` |
| `x86_64-apple-darwin` | `macos-x86_64` | the same |
| `x86_64-pc-windows-*` | `windows-x86_64` | `slang-compiler.dll`, `slang-glslang.dll`, `slang-glsl-module.dll` |
| `aarch64-pc-windows-*` | `windows-aarch64` | the same |

Every archive is checked against a SHA-256 pinned per asset in `slang-sys/build/embed.rs` before it is read. The pins match the digests GitHub publishes with the release. A mismatch fails the build. Only the files in the table are taken out of the archive, each checked against its own pinned SHA-256, into a fresh directory under `OUT_DIR`, and nothing from it is run at build time. The files are compiled into the binary zlib-compressed.

On Linux the glibc-2.28 builds are the default. Their libraries need only GLIBC 2.17 and GLIBCXX 3.4.22, where the plain builds need GLIBC 2.34 and GLIBCXX 3.4.29. The plain x86_64 archive is also 77 MB against 24 MB, because it carries `slang-llvm`, which this crate does not use. Other targets, musl among them, have no official release; build those with `default-features = false` and one of the other ways below.

Build-time settings:

- **`SLANG_RELEASE_ARCHIVE=<path>`** uses a prefetched copy of the archive instead of the network, for offline and air-gapped builds. It is held to the same pinned hash.
- **`SLANG_RELEASE_ASSET=<asset>`** picks another variant of the release: `linux-x86_64`, `linux-x86_64-glibc-2.27` or `linux-aarch64`.
- **The build cache** is `$XDG_CACHE_HOME/shader-slang`, else `~/.cache/shader-slang`, `%LOCALAPPDATA%\shader-slang` or `$CARGO_HOME/shader-slang`; `SHADER_SLANG_CACHE_DIR` overrides it. `downloads/` holds the archives, named by their SHA-256, so repeated builds and other projects never download again. `payload/` holds the compressed files, keyed by the archive's SHA-256: compressing them is most of the build script's time in an unoptimised build, so a later build on the machine decompresses each cached file, checks it against its pinned hash, and uses it. A fresh build of `shader-slang-sys` takes about 11 s with an empty cache and 2.5 s with a full one.

The download goes through `ureq` with `rustls`, and honours the usual proxy variables. docs.rs builds embed nothing.

### What happens at run time

A process cannot open a library from memory. The first use of Slang in a process, normally `GlobalSession::new`, writes the embedded files once into:

```
$XDG_CACHE_HOME/shader-slang/2026.14.1-<content hash>/
```

The fallbacks are `~/.cache`, then `%LOCALAPPDATA%`, then the system temporary directory. The directory is written into a sibling and renamed into place, so no process ever sees one half written, and two processes racing both succeed. A directory already in place is checked, every file present at its size, rather than rewritten. The compiler library is then opened from it by absolute path, and each global session is told to look there for `slang-glslang`, so Slang asks for it by absolute path on every platform. A root where the library will not open, such as a `noexec` mount, is skipped for the next.

| | Bytes |
|---|---|
| Embedded payload, compressed (Linux x86_64) | 17,869,409 |
| Cache directory, uncompressed | 43,536,328 |
| Release test binary of this crate | 19,169,976 |

`shader_slang::embedded` exposes the payload: its files, version, asset, the cache directory, and `verify` for a byte-for-byte check.

### Other ways to reach Slang

- **`system` feature** links Slang at build time, as earlier versions did. Set `SLANG_DIR`, or `SLANG_INCLUDE_DIR` and `SLANG_LIB_DIR`, or install the [LunarG Vulkan SDK](https://vulkan.lunarg.com), which bundles Slang and sets `VULKAN_SDK`. The library must then be on the platform's search path at run time. On Windows, copy `slang.dll` to your executable's directory. `system` takes precedence over `embed`; turn off default features to skip the download.
- **Neither feature** (`default-features = false`): the library is opened at run time by its platform name (`libslang-compiler.so`, `libslang-compiler.dylib`, `slang-compiler.dll`, then the older `slang` names), from the platform's usual search path.
- **`shader_slang::library::load(path)`** opens a given library file, with any feature but `system`, if called before anything else uses Slang.

To compile to DXIL bytecode, also put `dxil.dll` and `dxcompiler.dll` from the [Microsoft DirectXShaderCompiler](https://github.com/microsoft/DirectXShaderCompiler/releases) beside your executable.

The bindings were generated from the 2026.14.1 headers and must be used with that release. They are checked in as `slang-sys/src/bindings.rs` and `slang-sys/src/functions.rs`, with the safe crate's enums in `src/enums.rs`, so a build needs neither libclang nor a Slang header. The `bindgen` feature regenerates them at build time from `SLANG_INCLUDE_DIR`, `SLANG_DIR` or `VULKAN_SDK`. To refresh the checked-in copies:

```sh
SLANG_DIR=/path/to/slang slang-sys/generate-bindings.sh
```

The C functions, the reflection API among them, are resolved by name when the library opens. Calling one the library does not export panics with its name. Likewise an enum value the bindings do not know, which only a different Slang release can return, panics with the enum and the value. The one function `slang.h` declares with C++ linkage, `spReflection_GetSession`, is looked up under its Itanium and MSVC mangled names.

## Shared-library loaders

Slang loads parts of itself by name at run time, chiefly `slang-glslang`. `GlobalSession::set_shared_library_loader` installs a `SharedLibraryLoader` made from a Rust closure. It can hand back a `SharedLibrary` of its own, with symbols from Rust, open one from elsewhere, defer to the default, or refuse:

```rust
let loader = slang::SharedLibraryLoader::new(|path: &str, default: &slang::DefaultLoader<'_>| {
	if path.contains("glslang") {
		Err(slang::Error::Code(shader_slang_sys::SLANG_E_NOT_FOUND))
	} else {
		default.load(path)
	}
});
global_session.set_shared_library_loader(Some(&loader));
```

## Testing

`cargo test` runs the tests. `scripts/test-no-env.sh` builds them, then runs the test binaries under `env -i` with `PATH=/usr/bin:/bin` and a fresh, empty `HOME`. No `SLANG_DIR`, `LD_LIBRARY_PATH` or `VULKAN_SDK` is set, and no `slangc` is on the path. A test that runs only there checks from inside the process that every Slang library mapped into it came from the fresh cache.

CI (`.github/workflows/ci.yml`) runs fmt, clippy and the docs with warnings denied, the tests with the embedded default on Linux x86_64 and aarch64, macOS aarch64 and Windows x86_64, and on Linux the `system` feature and the no-default-features build against the unpacked release.

## Credits

Maintained by Lauro Oyen ([@laurooyen](https://github.com/laurooyen)).

Licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
