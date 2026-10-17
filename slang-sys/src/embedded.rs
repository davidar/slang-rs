//! The Slang library embedded in this binary (the `embed` feature).
//!
//! At build time the pinned official Slang release for the target was
//! downloaded, checked against its SHA-256 and the library files taken out
//! of it (see `build/embed.rs`); they are compiled into the binary,
//! zlib-compressed. A process cannot open a library from memory, so the
//! first use writes them into a cache directory, once per machine and
//! payload:
//!
//! `$XDG_CACHE_HOME/shader-slang/<version>-<content hash>/` (`~/.cache` when
//! that is unset, `%LOCALAPPDATA%` on Windows without either), with the
//! system temporary directory as the last resort. A directory is written
//! into a sibling and renamed into place, so no process ever sees one half
//! written and two processes doing it at once both succeed; one already in
//! place is checked (every file present at its size) rather than rewritten.
//! The compiler library is then opened from it by absolute path, and Slang
//! finds its SPIR-V optimiser module (`slang-glslang`) beside it.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::library::{Library, LoadError};

/// One embedded file.
pub struct File {
	/// The file name, as in the Slang release.
	pub name: &'static str,
	/// Its size in bytes.
	pub size: u64,
	/// Its SHA-256, in lowercase hex, taken at build time.
	pub sha256: &'static str,
	compressed: &'static [u8],
}

impl File {
	/// The file's bytes, decompressed.
	pub fn bytes(&self) -> io::Result<Vec<u8>> {
		let mut bytes = Vec::with_capacity(self.size as usize);
		flate2::read::ZlibDecoder::new(self.compressed).read_to_end(&mut bytes)?;
		if bytes.len() as u64 != self.size {
			return Err(io::Error::new(
				io::ErrorKind::InvalidData,
				format!("embedded {} decompressed to the wrong size", self.name),
			));
		}
		Ok(bytes)
	}

	/// The size of the file as embedded, compressed.
	pub fn compressed_size(&self) -> usize {
		self.compressed.len()
	}
}

include!(concat!(env!("OUT_DIR"), "/embedded.rs"));

/// The Slang release the payload came from.
pub const fn version() -> &'static str {
	VERSION
}

/// The release asset, such as `linux-x86_64-glibc-2.28`, or `None` when this
/// build carries no payload (a docs.rs build).
pub const fn asset() -> Option<&'static str> {
	ASSET
}

/// SHA-256 of the release archive the payload was taken from.
pub const fn archive_sha256() -> Option<&'static str> {
	ARCHIVE_SHA256
}

/// The embedded files; the first is the compiler library.
pub fn files() -> &'static [File] {
	FILES
}

/// The name of the cache directory: `<version>-<content hash>`.
pub fn dir_name() -> String {
	format!("{VERSION}-{CONTENT_HASH}")
}

/// The roots tried in turn for the cache directory.
fn roots() -> Vec<PathBuf> {
	let absolute = |var: &str| {
		std::env::var_os(var)
			.map(PathBuf::from)
			.filter(|p| p.is_absolute())
	};
	let cache = absolute("XDG_CACHE_HOME")
		.or_else(|| absolute("HOME").map(|home| home.join(".cache")))
		.or_else(|| absolute("LOCALAPPDATA"));
	cache
		.into_iter()
		.chain([std::env::temp_dir()])
		.map(|root| root.join("shader-slang"))
		.collect()
}

/// Whether `dir` holds every embedded file at its size. The directory is
/// named by the content hash and only ever appears whole, so this is what
/// tells a finished directory from a missing one; [`verify`] compares the
/// bytes.
fn is_written(dir: &Path) -> bool {
	FILES.iter().all(|file| {
		std::fs::metadata(dir.join(file.name)).is_ok_and(|m| m.is_file() && m.len() == file.size)
	})
}

/// Whether `dir` holds exactly the embedded files, byte for byte.
pub fn verify(dir: &Path) -> io::Result<bool> {
	for file in FILES {
		let on_disk = match std::fs::read(dir.join(file.name)) {
			Ok(bytes) => bytes,
			Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
			Err(err) => return Err(err),
		};
		if on_disk != file.bytes()? {
			return Ok(false);
		}
	}
	Ok(true)
}

/// Writes the files into a sibling of `dir` and renames it into place.
fn materialise(dir: &Path) -> io::Result<()> {
	let parent = dir.parent().expect("a cache directory has a parent");
	std::fs::create_dir_all(parent)?;
	let nanos = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map_or(0, |d| d.subsec_nanos());
	let staging = parent.join(format!(
		".{}.tmp-{}-{nanos}",
		dir_name(),
		std::process::id()
	));
	let written = write_files(&staging).and_then(|()| std::fs::rename(&staging, dir));
	match written {
		Ok(()) => Ok(()),
		Err(err) => {
			let _ = std::fs::remove_dir_all(&staging);
			// Losing the rename to another process is success.
			if is_written(dir) { Ok(()) } else { Err(err) }
		}
	}
}

fn write_files(dir: &Path) -> io::Result<()> {
	std::fs::create_dir_all(dir)?;
	for file in FILES {
		let path = dir.join(file.name);
		let mut out = std::fs::File::create(&path)?;
		out.write_all(&file.bytes()?)?;
		out.sync_all()?;
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt;
			std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
		}
	}
	Ok(())
}

/// The directory the embedded files were written to, writing them if this
/// is the first use on this machine. Resolved once per process.
///
/// Each root is tried in turn until one holds the files and the compiler
/// library opens from it (a cache on a `noexec` mount fails the second
/// test, and the next root is tried).
pub fn dir() -> Result<&'static Path, LoadError> {
	opened().map(|(dir, _)| dir.as_path())
}

static OPENED: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();

/// The cache directory and the compiler library in it, once one opened.
fn opened() -> Result<&'static (PathBuf, PathBuf), LoadError> {
	if let Some(opened) = OPENED.get() {
		return Ok(opened);
	}
	// Opening goes through the process-wide library, which records the
	// directory here on success.
	crate::library::library()?;
	OPENED.get().ok_or_else(|| {
		LoadError::new(
			"the Slang library in use is not the embedded one (it was opened with library::load)",
		)
	})
}

/// Writes the payload if needed and opens the compiler library from it.
pub(crate) fn open() -> Result<Library, LoadError> {
	let Some(main) = FILES.first() else {
		return Err(LoadError::new(
			"this build of shader-slang-sys carries no embedded Slang library (built for docs.rs)",
		));
	};
	let name = dir_name();
	let mut failures = Vec::new();
	for root in roots() {
		let dir = root.join(&name);
		if !is_written(&dir)
			&& let Err(err) = materialise(&dir)
		{
			failures.push(format!("cannot write {}: {err}", dir.display()));
			continue;
		}
		let path = dir.join(main.name);
		match crate::library::open(&path) {
			Ok(library) => {
				let _ = OPENED.set((dir, path));
				return Ok(library);
			}
			Err(err) => failures.push(err.to_string()),
		}
	}
	Err(LoadError::new(format!(
		"cannot use the embedded Slang library: {}",
		failures.join("; ")
	)))
}
