//! The `embed` feature: fetch the pinned official Slang release for the
//! target, verify it, and stage the library files `src/embedded.rs` includes.
//!
//! Every archive is checked against a SHA-256 pinned in [`ASSETS`] before a
//! byte of it is read as an archive, whether it came from the network, the
//! download cache or `SLANG_RELEASE_ARCHIVE`. Only the files named in the
//! table are taken out of it, by their names in the table (never by a path
//! from the archive), into a fresh directory under `OUT_DIR`. Nothing from
//! the archive is ever run.

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
	/// The files to embed, as archive paths (without a leading `./`). The
	/// first is the compiler library the process opens; Slang finds the
	/// others beside it.
	files: &'static [&'static str],
}

const LINUX_FILES: &[&str] = &[
	"lib/libslang-compiler.so.0.2026.14.1",
	"lib/libslang-glslang-2026.14.1.so",
];
const MACOS_FILES: &[&str] = &[
	"lib/libslang-compiler.0.2026.14.1.dylib",
	"lib/libslang-glslang-2026.14.1.dylib",
];
// slang-glsl-module.dll is the GLSL module, a separate file on Windows only
// (built into the compiler library elsewhere).
const WINDOWS_FILES: &[&str] = &[
	"bin/slang-compiler.dll",
	"bin/slang-glslang.dll",
	"bin/slang-glsl-module.dll",
];

/// Every release asset this crate can embed. The default per target is in
/// [`default_asset`]; `SLANG_RELEASE_ASSET` picks another by name.
const ASSETS: &[Asset] = &[
	Asset {
		name: "linux-x86_64-glibc-2.28",
		sha256: "07cf4274f5f1f976d75554cc5c1cc4bd993a9a92442a7726fa701aa457183a26",
		files: LINUX_FILES,
	},
	Asset {
		name: "linux-x86_64-glibc-2.27",
		sha256: "427d9985aac9e88912429bf9e7c1a3543bf35a61aaa855c890eeb96920175e0c",
		files: LINUX_FILES,
	},
	Asset {
		name: "linux-x86_64",
		sha256: "21f2d7847385a770e569fb61b1507a7794d742d97850bce0432bff0032ca005f",
		files: LINUX_FILES,
	},
	Asset {
		name: "linux-aarch64-glibc-2.28",
		sha256: "ebbc011a210f16902c180ef82f94b3e5b52ea0c80d7827a8a830961be3aafa3c",
		files: LINUX_FILES,
	},
	Asset {
		name: "linux-aarch64",
		sha256: "438325c6529ba658f571614d9d551cdff09e730836d29c26c290afc8ed996985",
		files: LINUX_FILES,
	},
	Asset {
		name: "macos-aarch64",
		sha256: "92da7ab6226dd951037cd85397f830ae78fe40fbbb8928882e0b2654e468fdd4",
		files: MACOS_FILES,
	},
	Asset {
		name: "macos-x86_64",
		sha256: "adc5e179f5584ca572293e93612df9f0b6be8a46dcb53622238efc7a62b1da2f",
		files: MACOS_FILES,
	},
	Asset {
		name: "windows-x86_64",
		sha256: "791fe8400fe3cabdaf054089e6f5bffa745fbf5912b5b85e2ee4dc78c6b9aff7",
		files: WINDOWS_FILES,
	},
	Asset {
		name: "windows-aarch64",
		sha256: "0bb38b38883111483b7bef7a2ab32acfdc99dedaed1d97af8a392d14dd043c87",
		files: WINDOWS_FILES,
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

	let archive = archive_bytes(asset)?;

	// A fresh directory per build, so nothing from an earlier archive can
	// survive into this one.
	let stage = out_dir.join("slang-payload");
	if stage.exists() {
		std::fs::remove_dir_all(&stage)
			.map_err(|e| format!("cannot clear {}: {e}", stage.display()))?;
	}
	std::fs::create_dir_all(&stage)
		.map_err(|e| format!("cannot create {}: {e}", stage.display()))?;

	let files = extract(asset, &archive)?;
	let mut staged = Vec::new();
	for (name, bytes) in files {
		let compressed = stage.join(format!("{name}.zlib"));
		let mut encoder =
			flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
		encoder.write_all(&bytes).map_err(|e| e.to_string())?;
		let data = encoder.finish().map_err(|e| e.to_string())?;
		std::fs::write(&compressed, data)
			.map_err(|e| format!("cannot write {}: {e}", compressed.display()))?;
		staged.push(Staged {
			name,
			size: bytes.len() as u64,
			sha256: hex(&Sha256::digest(&bytes)),
			compressed,
		});
	}

	write_manifest(out_dir, Some(asset), &staged);
	Ok(())
}

/// The verified archive: `SLANG_RELEASE_ARCHIVE` if set, else the download
/// cache, else the network (filling the cache).
fn archive_bytes(asset: &Asset) -> Result<Vec<u8>, String> {
	let file_name = format!("slang-{VERSION}-{}.tar.gz", asset.name);

	if let Some(path) = env::var_os("SLANG_RELEASE_ARCHIVE").filter(|p| !p.is_empty()) {
		let path = PathBuf::from(path);
		println!("cargo:rerun-if-changed={}", path.display());
		let bytes = read_limited(&path)?;
		check(
			&bytes,
			asset,
			&format!("SLANG_RELEASE_ARCHIVE={}", path.display()),
		)?;
		return Ok(bytes);
	}

	let cached = cache_dir().map(|dir| dir.join(format!("{}-{file_name}", asset.sha256)));
	if let Some(path) = &cached
		&& let Ok(bytes) = read_limited(path)
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

fn read_limited(path: &Path) -> Result<Vec<u8>, String> {
	let file =
		std::fs::File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
	let mut bytes = Vec::new();
	file.take(MAX_ARCHIVE_BYTES + 1)
		.read_to_end(&mut bytes)
		.map_err(|e| format!("cannot read {}: {e}", path.display()))?;
	if bytes.len() as u64 > MAX_ARCHIVE_BYTES {
		return Err(format!(
			"{} is larger than any Slang release archive",
			path.display()
		));
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

/// The user-level download cache: `$SHADER_SLANG_CACHE_DIR`, else
/// `$XDG_CACHE_HOME/shader-slang/downloads`, else `~/.cache/...` (or
/// `%LOCALAPPDATA%\...` on Windows), else `$CARGO_HOME/shader-slang/...`.
/// Entries are named by their SHA-256, so a stale or foreign file is never
/// mistaken for the asset.
fn cache_dir() -> Option<PathBuf> {
	let absolute = |var: &str| {
		env::var_os(var)
			.map(PathBuf::from)
			.filter(|p| p.is_absolute())
	};
	if let Some(dir) = absolute("SHADER_SLANG_CACHE_DIR") {
		return Some(dir.join("downloads"));
	}
	absolute("XDG_CACHE_HOME")
		.or_else(|| absolute("HOME").map(|home| home.join(".cache")))
		.or_else(|| absolute("LOCALAPPDATA"))
		.or_else(|| absolute("CARGO_HOME"))
		.map(|root| root.join("shader-slang").join("downloads"))
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

/// The asset's files, in table order, as (file name, bytes).
fn extract(asset: &Asset, archive: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
	let mut found: Vec<Option<Vec<u8>>> = vec![None; asset.files.len()];
	let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
	let entries = tar
		.entries()
		.map_err(|e| format!("cannot read the release archive: {e}"))?;
	for entry in entries {
		let mut entry = entry.map_err(|e| format!("cannot read the release archive: {e}"))?;
		let path = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
		let path = path.trim_start_matches("./");
		let Some(index) = asset.files.iter().position(|f| *f == path) else {
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
		.map(|(path, bytes)| {
			let bytes =
				bytes.ok_or_else(|| format!("{path} is missing from the release archive"))?;
			let name = path.rsplit('/').next().unwrap_or(path).to_string();
			Ok((name, bytes))
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
