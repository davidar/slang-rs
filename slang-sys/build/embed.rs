//! The `embed` feature: fetch the pinned official Slang release for the
//! target, verify it, and stage the library files `src/embedded.rs` includes.
//!
//! Every archive is checked against a SHA-256 pinned in [`ASSETS`] before a
//! byte of it is read as an archive, whether it came from the network, the
//! download cache or `SLANG_RELEASE_ARCHIVE`. Only the files named in the
//! table are taken out of it, by their names in the table (never by a path
//! from the archive), each checked against its own pinned SHA-256, into a
//! fresh directory under `OUT_DIR`. Nothing from the archive is ever run.
//!
//! Compressing the files is most of this script's running time (it is not
//! an optimised build), so the compressed files are kept in the user's
//! cache, keyed by the archive's hash. A later build on the machine reuses
//! them after decompressing each and checking it against its pinned hash,
//! which is a few times faster than compressing it again.

use std::env;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The Slang release the bindings were generated from and the payload is
/// taken from.
const VERSION: &str = "2026.14.1";

const URL_BASE: &str = "https://github.com/shader-slang/slang/releases/download";

/// Upper bound on what is accepted from the network or read from an
/// archive, against a runaway response or a hostile archive.
const MAX_ARCHIVE_BYTES: u64 = 256 << 20;
const MAX_FILE_BYTES: u64 = 256 << 20;

struct Asset {
	/// The part of the release file name after `slang-<version>-`.
	name: &'static str,
	/// SHA-256 of `slang-<version>-<name>.tar.gz`, as published with the
	/// release and checked on download.
	sha256: &'static str,
	/// The files to embed. The first is the compiler library the process
	/// opens; Slang finds the others beside it.
	files: &'static [EmbeddedFile],
}

struct EmbeddedFile {
	/// The file's path in the archive, without a leading `./`.
	path: &'static str,
	/// SHA-256 of the file's bytes, checked when it is taken out of the
	/// archive and when it is reused from the payload cache.
	sha256: &'static str,
}

const fn file(path: &'static str, sha256: &'static str) -> EmbeddedFile {
	EmbeddedFile { path, sha256 }
}

const LINUX_COMPILER: &str = "lib/libslang-compiler.so.0.2026.14.1";
const LINUX_GLSLANG: &str = "lib/libslang-glslang-2026.14.1.so";
const MACOS_COMPILER: &str = "lib/libslang-compiler.0.2026.14.1.dylib";
const MACOS_GLSLANG: &str = "lib/libslang-glslang-2026.14.1.dylib";
// slang-glsl-module.dll is the GLSL module, a separate file on Windows only:
// the release ships one for the other platforms too, but there it is also
// built into the compiler library and `import glsl` works without it.
const WINDOWS_COMPILER: &str = "bin/slang-compiler.dll";
const WINDOWS_GLSLANG: &str = "bin/slang-glslang.dll";
const WINDOWS_GLSL_MODULE: &str = "bin/slang-glsl-module.dll";

/// Every release asset this crate can embed, with the hashes of the archive
/// and of each embedded file. The default per target is in
/// [`default_asset`]; `SLANG_RELEASE_ASSET` picks another by name.
const ASSETS: &[Asset] = &[
	Asset {
		name: "linux-x86_64-glibc-2.28",
		sha256: "07cf4274f5f1f976d75554cc5c1cc4bd993a9a92442a7726fa701aa457183a26",
		files: &[
			file(
				LINUX_COMPILER,
				"5a1dca614ec234cf55d42f956923c4c10b0a1a12662a3dbec472bf7f2f4b8374",
			),
			file(
				LINUX_GLSLANG,
				"8d6acd24542202269e8950179186122cf385a66a3f0ec72d7ed311e0f9e532df",
			),
		],
	},
	Asset {
		name: "linux-x86_64-glibc-2.27",
		sha256: "427d9985aac9e88912429bf9e7c1a3543bf35a61aaa855c890eeb96920175e0c",
		files: &[
			file(
				LINUX_COMPILER,
				"aba57be5bccd5490c539e3ff19b307bc464243f60714e5894e62bad8c2cac9ba",
			),
			file(
				LINUX_GLSLANG,
				"45ba97ae7c780eeca9f45b7e30348df968b1031ca541663431295bea4e5d8032",
			),
		],
	},
	Asset {
		name: "linux-x86_64",
		sha256: "21f2d7847385a770e569fb61b1507a7794d742d97850bce0432bff0032ca005f",
		files: &[
			file(
				LINUX_COMPILER,
				"b1090648853ef4ca4a7e76b329572030d595587af4a707cb4cf27d6d9543958a",
			),
			file(
				LINUX_GLSLANG,
				"53ddf1bf48ac2ed44254abc81bdd08f7ccf8f92637e446525595f9e89cf362c6",
			),
		],
	},
	Asset {
		name: "linux-aarch64-glibc-2.28",
		sha256: "ebbc011a210f16902c180ef82f94b3e5b52ea0c80d7827a8a830961be3aafa3c",
		files: &[
			file(
				LINUX_COMPILER,
				"9c8fe282f0681322dd23c009381765c0fa51e5dac4cf68ea6ec5dd196c574ac1",
			),
			file(
				LINUX_GLSLANG,
				"384e566dfa98abd98dc8c2a4a2e8ef65fbf7f5791686927bf11e06b0f4cf22ce",
			),
		],
	},
	Asset {
		name: "linux-aarch64",
		sha256: "438325c6529ba658f571614d9d551cdff09e730836d29c26c290afc8ed996985",
		files: &[
			file(
				LINUX_COMPILER,
				"751f54c1d3e9ffdc8cdd331386ea1a09d0a69c2c6d99f4e721ee72b2b76fccfd",
			),
			file(
				LINUX_GLSLANG,
				"57708e83d8a846a6ab831ffb628dca0afda691cfa5ec47179438687667299e48",
			),
		],
	},
	Asset {
		name: "macos-aarch64",
		sha256: "92da7ab6226dd951037cd85397f830ae78fe40fbbb8928882e0b2654e468fdd4",
		files: &[
			file(
				MACOS_COMPILER,
				"4fadae0d56d4538dc2a0099086de3d2a5350e12da4591679ee8cbb571c5db7de",
			),
			file(
				MACOS_GLSLANG,
				"a4a9cfe6bc4766227d537fa5dfb11ca4d0e5eeac87525245025af880cfc9be8b",
			),
		],
	},
	Asset {
		name: "macos-x86_64",
		sha256: "adc5e179f5584ca572293e93612df9f0b6be8a46dcb53622238efc7a62b1da2f",
		files: &[
			file(
				MACOS_COMPILER,
				"c87e37121416f54b1d9b988fb74241b0bc662cd03f9e4ce823b314271a9d7016",
			),
			file(
				MACOS_GLSLANG,
				"81c847c535b9c0c0466005ba89af595f553a04c965d01882e93b3442a65f5f1b",
			),
		],
	},
	Asset {
		name: "windows-x86_64",
		sha256: "791fe8400fe3cabdaf054089e6f5bffa745fbf5912b5b85e2ee4dc78c6b9aff7",
		files: &[
			file(
				WINDOWS_COMPILER,
				"2271ca931ffa18fb59a649bb91b22f36afd6ec34584e17f8bb28e00143614ca4",
			),
			file(
				WINDOWS_GLSLANG,
				"c3c0684ccbb1ed812a199699d6c69c6693a2b53aa46ab0b2c921190ca84a0b0f",
			),
			file(
				WINDOWS_GLSL_MODULE,
				"f0206a0c50a8c3c86ad7473cc5b25106e1131dda5c371b12744d6554688080e1",
			),
		],
	},
	Asset {
		name: "windows-aarch64",
		sha256: "0bb38b38883111483b7bef7a2ab32acfdc99dedaed1d97af8a392d14dd043c87",
		files: &[
			file(
				WINDOWS_COMPILER,
				"066d048df8f9c73eb1cfd77e64dd285e1b1e9766eda6c170a29350bfb76fdb9a",
			),
			file(
				WINDOWS_GLSLANG,
				"f9d2f0e3ccbb7a2031f8e5715b087e3b7e2cfe5a82048d6dbc38210370d053ed",
			),
			file(
				WINDOWS_GLSL_MODULE,
				"41bad5edd0b55a85595e0480b7daa21bdd66b5a936f2ac84c996b3b6e65fb2f1",
			),
		],
	},
];

/// The asset for the target being built.
///
/// On Linux the glibc-2.28 builds are the default: their libraries need
/// only GLIBC 2.17 and GLIBCXX 3.4.22, where the plain builds need GLIBC 2.34
/// and GLIBCXX 3.4.29, and the plain x86_64 archive also carries slang-llvm
/// (77 MB against 24 MB), which this crate does not use. (The "glibc-2.27"
/// x86_64 build, despite its name, needs GLIBCXX 3.4.29 too.)
fn default_asset() -> Result<&'static str, String> {
	let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
	let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
	let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
	Ok(match (os.as_str(), arch.as_str(), target_env.as_str()) {
		("linux", "x86_64", "gnu") => "linux-x86_64-glibc-2.28",
		("linux", "aarch64", "gnu") => "linux-aarch64-glibc-2.28",
		("macos", "aarch64", _) => "macos-aarch64",
		("macos", "x86_64", _) => "macos-x86_64",
		("windows", "x86_64", _) => "windows-x86_64",
		("windows", "aarch64", _) => "windows-aarch64",
		_ => {
			return Err(format!(
				"Slang publishes no release for the target {}, so the `embed` feature cannot \
				 embed one. Build with `default-features = false` and point the crate at a \
				 Slang library (the `system` feature, or `library::load` at run time).",
				env::var("TARGET").unwrap_or_default()
			));
		}
	})
}

pub fn run() {
	for var in [
		"SLANG_RELEASE_ARCHIVE",
		"SLANG_RELEASE_ASSET",
		"SHADER_SLANG_CACHE_DIR",
		"DOCS_RS",
	] {
		println!("cargo:rerun-if-env-changed={var}");
	}
	println!("cargo:rerun-if-changed=build/embed.rs");

	let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set for build scripts"));

	// docs.rs builds without network access; the documentation does not
	// need the library.
	if env::var_os("DOCS_RS").is_some() {
		write_manifest(&out_dir, None, &[]);
		return;
	}

	if let Err(err) = embed(&out_dir) {
		panic!("shader-slang-sys: {err}");
	}
}

struct Staged {
	name: String,
	size: u64,
	sha256: String,
	compressed: PathBuf,
}

fn embed(out_dir: &Path) -> Result<(), String> {
	let asset_name = match env::var("SLANG_RELEASE_ASSET") {
		Ok(name) if !name.is_empty() => name,
		_ => default_asset()?.to_string(),
	};
	let asset = ASSETS
		.iter()
		.find(|a| a.name == asset_name)
		.ok_or_else(|| {
			format!(
				"SLANG_RELEASE_ASSET={asset_name:?} is not a known release asset; known: {}",
				ASSETS.iter().map(|a| a.name).collect::<Vec<_>>().join(", ")
			)
		})?;

	// A fresh directory per build, so nothing from an earlier archive can
	// survive into this one.
	let stage = out_dir.join("slang-payload");
	if stage.exists() {
		std::fs::remove_dir_all(&stage)
			.map_err(|e| format!("cannot clear {}: {e}", stage.display()))?;
	}
	std::fs::create_dir_all(&stage)
		.map_err(|e| format!("cannot create {}: {e}", stage.display()))?;

	let payload_cache = cache_dir().map(|dir| dir.join("payload").join(asset.sha256));
	let staged = match payload_cache
		.as_deref()
		.and_then(|dir| reuse_payload(asset, dir, &stage))
	{
		Some(staged) => staged,
		None => build_payload(asset, payload_cache.as_deref(), &stage)?,
	};

	write_manifest(out_dir, Some(asset), &staged);
	Ok(())
}

/// The file name a path in the archive is embedded under.
fn file_name(path: &str) -> &str {
	path.rsplit('/').next().unwrap_or(path)
}

/// The compressed files from the payload cache, copied into `stage`, if
/// every one is there and decompresses to the bytes its pinned hash names.
/// Anything else empties the cache entry so the next build rebuilds it.
fn reuse_payload(asset: &Asset, cache: &Path, stage: &Path) -> Option<Vec<Staged>> {
	let mut staged = Vec::new();
	for file in asset.files {
		let name = file_name(file.path);
		let cached = cache.join(format!("{name}.zlib"));
		let compressed = read_limited(&cached, MAX_FILE_BYTES).ok()?;
		let mut bytes = Vec::new();
		flate2::read::ZlibDecoder::new(&compressed[..])
			.take(MAX_FILE_BYTES + 1)
			.read_to_end(&mut bytes)
			.ok()?;
		let sha256 = hex(&Sha256::digest(&bytes));
		if bytes.len() as u64 > MAX_FILE_BYTES || sha256 != file.sha256 {
			let _ = std::fs::remove_dir_all(cache);
			return None;
		}
		let target = stage.join(format!("{name}.zlib"));
		std::fs::write(&target, &compressed).ok()?;
		staged.push(Staged {
			name: name.to_string(),
			size: bytes.len() as u64,
			sha256,
			compressed: target,
		});
	}
	Some(staged)
}

/// Takes the files out of the verified archive, checks each against its
/// pinned hash, compresses them into `stage`, and keeps a copy of each in
/// the payload cache for the next build.
fn build_payload(asset: &Asset, cache: Option<&Path>, stage: &Path) -> Result<Vec<Staged>, String> {
	let archive = archive_bytes(asset)?;
	let files = extract(asset, &archive)?;
	let mut staged = Vec::new();
	for (file, bytes) in asset.files.iter().zip(files) {
		let sha256 = hex(&Sha256::digest(&bytes));
		if sha256 != file.sha256 {
			return Err(format!(
				"{} in slang-{VERSION}-{}.tar.gz has SHA-256 {sha256}, not the pinned {}",
				file.path, asset.name, file.sha256
			));
		}
		let name = file_name(file.path);
		let mut encoder =
			flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
		encoder.write_all(&bytes).map_err(|e| e.to_string())?;
		let data = encoder.finish().map_err(|e| e.to_string())?;
		let compressed = stage.join(format!("{name}.zlib"));
		std::fs::write(&compressed, &data)
			.map_err(|e| format!("cannot write {}: {e}", compressed.display()))?;
		if let Some(cache) = cache {
			store(&cache.join(format!("{name}.zlib")), &data);
		}
		staged.push(Staged {
			name: name.to_string(),
			size: bytes.len() as u64,
			sha256,
			compressed,
		});
	}
	Ok(staged)
}

/// The verified archive: `SLANG_RELEASE_ARCHIVE` if set, else the download
/// cache, else the network (filling the cache).
fn archive_bytes(asset: &Asset) -> Result<Vec<u8>, String> {
	let file_name = format!("slang-{VERSION}-{}.tar.gz", asset.name);

	if let Some(path) = env::var_os("SLANG_RELEASE_ARCHIVE").filter(|p| !p.is_empty()) {
		let path = PathBuf::from(path);
		println!("cargo:rerun-if-changed={}", path.display());
		let bytes = read_limited(&path, MAX_ARCHIVE_BYTES)?;
		check(
			&bytes,
			asset,
			&format!("SLANG_RELEASE_ARCHIVE={}", path.display()),
		)?;
		return Ok(bytes);
	}

	let cached = cache_dir().map(|dir| {
		dir.join("downloads")
			.join(format!("{}-{file_name}", asset.sha256))
	});
	if let Some(path) = &cached
		&& let Ok(bytes) = read_limited(path, MAX_ARCHIVE_BYTES)
	{
		if check(&bytes, asset, "").is_ok() {
			return Ok(bytes);
		}
		// Not what the name promises (a truncated or altered file): drop it
		// and fetch again.
		let _ = std::fs::remove_file(path);
	}

	let url = format!("{URL_BASE}/v{VERSION}/{file_name}");
	let bytes = download(&url)?;
	check(&bytes, asset, &url)?;

	if let Some(path) = &cached {
		store(path, &bytes);
	}
	Ok(bytes)
}

fn check(bytes: &[u8], asset: &Asset, origin: &str) -> Result<(), String> {
	let got = hex(&Sha256::digest(bytes));
	if got == asset.sha256 {
		Ok(())
	} else {
		Err(format!(
			"{origin}: SHA-256 {got} does not match the pinned {} of slang-{VERSION}-{}.tar.gz",
			asset.sha256, asset.name
		))
	}
}

fn read_limited(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
	let file =
		std::fs::File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
	let mut bytes = Vec::new();
	file.take(limit + 1)
		.read_to_end(&mut bytes)
		.map_err(|e| format!("cannot read {}: {e}", path.display()))?;
	if bytes.len() as u64 > limit {
		return Err(format!("{} is implausibly large", path.display()));
	}
	Ok(bytes)
}

fn download(url: &str) -> Result<Vec<u8>, String> {
	println!("cargo:warning=shader-slang-sys: downloading {url}");
	let mut response = ureq::get(url)
		.call()
		.map_err(|e| format!("cannot download {url}: {e}. For an offline build, set SLANG_RELEASE_ARCHIVE to a copy of that file."))?;
	response
		.body_mut()
		.with_config()
		.limit(MAX_ARCHIVE_BYTES)
		.read_to_vec()
		.map_err(|e| format!("cannot download {url}: {e}"))
}

/// The user-level build cache: `$SHADER_SLANG_CACHE_DIR`, else
/// `$XDG_CACHE_HOME/shader-slang`, else `~/.cache/shader-slang` (or
/// `%LOCALAPPDATA%\shader-slang` on Windows), else `$CARGO_HOME/shader-slang`.
/// `downloads/` holds release archives named by their SHA-256 and `payload/`
/// the compressed files keyed by the archive's SHA-256, so a stale or
/// foreign file is never mistaken for the asset.
fn cache_dir() -> Option<PathBuf> {
	let absolute = |var: &str| {
		env::var_os(var)
			.map(PathBuf::from)
			.filter(|p| p.is_absolute())
	};
	if let Some(dir) = absolute("SHADER_SLANG_CACHE_DIR") {
		return Some(dir);
	}
	absolute("XDG_CACHE_HOME")
		.or_else(|| absolute("HOME").map(|home| home.join(".cache")))
		.or_else(|| absolute("LOCALAPPDATA"))
		.or_else(|| absolute("CARGO_HOME"))
		.map(|root| root.join("shader-slang"))
}

/// Writes into the cache atomically; a cache that cannot be written is
/// skipped (the build already has the bytes).
fn store(path: &Path, bytes: &[u8]) {
	let Some(dir) = path.parent() else { return };
	if std::fs::create_dir_all(dir).is_err() {
		return;
	}
	let tmp = dir.join(format!(
		".{}.tmp-{}",
		path.file_name()
			.and_then(|n| n.to_str())
			.unwrap_or("archive"),
		std::process::id()
	));
	if std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, path).is_ok() {
		return;
	}
	let _ = std::fs::remove_file(&tmp);
}

/// The asset's files' bytes, in table order.
fn extract(asset: &Asset, archive: &[u8]) -> Result<Vec<Vec<u8>>, String> {
	let mut found: Vec<Option<Vec<u8>>> = vec![None; asset.files.len()];
	let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
	let entries = tar
		.entries()
		.map_err(|e| format!("cannot read the release archive: {e}"))?;
	for entry in entries {
		let mut entry = entry.map_err(|e| format!("cannot read the release archive: {e}"))?;
		let path = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
		let path = path.trim_start_matches("./");
		let Some(index) = asset.files.iter().position(|f| f.path == path) else {
			continue;
		};
		if !entry.header().entry_type().is_file() {
			return Err(format!(
				"{path} in the release archive is not a regular file"
			));
		}
		if found[index].is_some() {
			return Err(format!("{path} appears twice in the release archive"));
		}
		let mut bytes = Vec::new();
		(&mut entry)
			.take(MAX_FILE_BYTES + 1)
			.read_to_end(&mut bytes)
			.map_err(|e| format!("cannot read {path} from the release archive: {e}"))?;
		if bytes.len() as u64 > MAX_FILE_BYTES {
			return Err(format!(
				"{path} in the release archive is implausibly large"
			));
		}
		found[index] = Some(bytes);
	}
	asset
		.files
		.iter()
		.zip(found)
		.map(|(file, bytes)| {
			bytes.ok_or_else(|| format!("{} is missing from the release archive", file.path))
		})
		.collect()
}

/// Writes `embedded.rs` for `src/embedded.rs` to include.
fn write_manifest(out_dir: &Path, asset: Option<&Asset>, staged: &[Staged]) {
	// The cache directory's name: the version and a hash over every file's
	// name and content, so each payload has a directory of its own.
	let mut content = Sha256::new();
	for file in staged {
		content.update(file.name.as_bytes());
		content.update([0]);
		content.update(file.sha256.as_bytes());
		content.update([0]);
	}
	let content_hash = &hex(&content.finalize())[..16];

	let mut text = String::new();
	text.push_str("// Written by shader-slang-sys's build script (build/embed.rs).\n\n");
	text.push_str(&format!("const VERSION: &str = {VERSION:?};\n"));
	text.push_str(&format!(
		"const ASSET: Option<&str> = {:?};\n",
		asset.map(|a| a.name)
	));
	text.push_str(&format!(
		"const ARCHIVE_SHA256: Option<&str> = {:?};\n",
		asset.map(|a| a.sha256)
	));
	text.push_str(&format!("const CONTENT_HASH: &str = {content_hash:?};\n"));
	text.push_str("static FILES: &[File] = &[\n");
	for file in staged {
		text.push_str(&format!(
			"\tFile {{ name: {:?}, size: {}, sha256: {:?}, compressed: include_bytes!({:?}) }},\n",
			file.name,
			file.size,
			file.sha256,
			file.compressed.display().to_string()
		));
	}
	text.push_str("];\n");
	let path = out_dir.join("embedded.rs");
	std::fs::write(&path, text).unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
}

fn hex(bytes: &[u8]) -> String {
	bytes.iter().map(|b| format!("{b:02x}")).collect()
}
