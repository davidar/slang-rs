# Changelog

## Unreleased

### Slang travels with the crate

- **Embedded Slang (the default `embed` feature).** The build downloads the official Slang 2026.14.1 release for the target and checks it against a pinned SHA-256. The compiler library and its SPIR-V optimiser module are embedded in the binary, compressed. At run time they are written once into `$XDG_CACHE_HOME/shader-slang/<version>-<hash>/` and opened from there, so a dependent binary needs no Slang install and no environment variables. `SLANG_RELEASE_ARCHIVE` builds offline from a prefetched archive, and `SLANG_RELEASE_ASSET` picks another release variant. Downloads are cached by checksum. See the README for targets, sizes and the cache layout.
- **Slang is opened at run time, not linked.** The C functions are resolved from the library when it opens, the `spReflection*` reflection API included. `library::load(path)` opens a given file. The `system` feature keeps the old link-time behaviour against `SLANG_DIR` / `SLANG_LIB_DIR` / `VULKAN_SDK`.
- **Checked-in bindings.** The bindgen output for Slang 2026.14.1 is now source (`slang-sys/src/bindings.rs`, `slang-sys/src/functions.rs`). A build needs neither libclang nor a Slang header. The `bindgen` feature and `slang-sys/generate-bindings.sh` regenerate it.

### API

- **Diagnostics on success.** `Diagnostics` and `..._with_diagnostics` variants of module loading (from a name, a source string, a source blob, an IR blob), composite creation, link, layout, and entry-point and target code. They report warnings and recoverable errors on success too. A zero-warnings build needs them. So does detecting a missing SPIR-V optimiser, which only shows as an error on a successful compile.
- **New wrappers.**
  - `Module::find_and_check_entry_point` is slangc's `-entry`/`-stage`.
  - `Module::serialize` and `Module::write_to_file` produce IR.
  - `Session::load_module_from_source`, `loaded_module_count`, `loaded_module`, `loaded_modules`, `global_session` and `is_binary_module_up_to_date`.
  - `Blob::from_slice` reloads stored IR.
  - `GlobalSession::try_new` and `GlobalSession::try_create_session` give the reason for a failure.
- **Shared-library loaders.** `GlobalSession::set_shared_library_loader` and `shared_library_loader` take a `SharedLibraryLoader`, which can be built from a Rust closure. `SharedLibrary::new` answers symbol lookups from Rust, and `SharedLibrary::open` opens a file as Slang's own loader would.
- **Raw pointers without transmutes.** `Interface::from_raw`, `from_raw_borrowed`, `into_raw`, `as_ptr` and `cast`.
- **`ResourceShape` is bit flags.** It is now a newtype over the integer instead of an enum, because Slang combines a base shape with array, multisample, shadow, feedback and combined flags. A combination that is not a declared variant was undefined behaviour. The variant names remain as associated constants, so `ResourceShape::SlangTexture2d` and matches on it still work. New: `base_shape`, `flags`, `contains`, `is_array`, `is_multisample`, `is_shadow`, `is_feedback`, `is_combined`, `bits`, `from_bits`.
- **Changed: `SessionDesc::search_paths` takes Rust paths** (`impl IntoIterator<Item = impl AsRef<Path>>`) and keeps its own C strings. The old `&[*const i8]` let the strings die before the descriptor was used (upstream issue 31). `.search_paths(&ptrs)` becomes `.search_paths(["dir"])`. `SessionDesc` is no longer `#[repr(transparent)]`; it still derefs to `slang_SessionDesc`.
- **Added: `Error::Load`**, for a library that could not be opened. A `match` on `Error` without a wildcard arm needs it.

### Fixes

- `GlobalSession::new` and `create_session` honour their result codes.
- Failure paths no longer unwrap a possibly null diagnostics blob. The plain methods release the diagnostics blob they leaked on success.
- C strings Slang returns (module name and path, build tag, dependency paths) read as `""` when null instead of being dereferenced.
- A NUL inside a name is an error, not a panic, in the methods that return a `Result`.
