//! How this process reaches the Slang library.
//!
//! The C functions in this crate are resolved from one Slang library per
//! process, opened on first use and kept open until the process exits. Which
//! library depends on the crate's features:
//!
//! - `system`: the library is linked at build time (`SLANG_DIR` and friends
//!   at build time, the platform's library search path at run time), and
//!   there is nothing to open.
//! - `embed` (the default): the library and its SPIR-V optimiser module were
//!   embedded in the binary at build time; the first use writes them into a
//!   cache directory (see [`embedded`](crate::embedded)) and opens the
//!   library from there by absolute path.
//! - neither: the library is opened by its platform name
//!   (`libslang-compiler.so`, `libslang-compiler.dylib`, `slang-compiler.dll`,
//!   then the older `slang` names), which the platform looks up in its usual
//!   search path.
//!
//! In every case [`load`] opens a library at a given path instead, provided
//! it is called before anything else in the process has used Slang.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::Functions;

/// The Slang library this process uses.
pub struct Library {
	path: Option<PathBuf>,
	pub(crate) functions: Functions,
	#[cfg(not(feature = "system"))]
	_handle: libloading::Library,
}

impl Library {
	/// The file the library was opened from, or `None` when it is linked
	/// (the `system` feature).
	pub fn path(&self) -> Option<&Path> {
		self.path.as_deref()
	}
}

impl fmt::Debug for Library {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Library").field("path", &self.path).finish()
	}
}

/// Why the Slang library could not be opened.
#[derive(Debug, Clone)]
pub struct LoadError {
	message: String,
}

impl LoadError {
	pub(crate) fn new(message: impl Into<String>) -> Self {
		Self {
			message: message.into(),
		}
	}
}

impl fmt::Display for LoadError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(&self.message)
	}
}

impl std::error::Error for LoadError {}

static LIBRARY: OnceLock<Library> = OnceLock::new();
static OPENING: Mutex<()> = Mutex::new(());

/// Opens the library once; a failure is not remembered, so a later call (or
/// a [`load`] with an explicit path) can still succeed.
fn get_or_open(
	open: impl FnOnce() -> Result<Library, LoadError>,
) -> Result<&'static Library, LoadError> {
	if let Some(library) = LIBRARY.get() {
		return Ok(library);
	}
	let _opening = OPENING
		.lock()
		.unwrap_or_else(|poisoned| poisoned.into_inner());
	if let Some(library) = LIBRARY.get() {
		return Ok(library);
	}
	let library = open()?;
	Ok(LIBRARY.get_or_init(|| library))
}

/// The library this process uses, opened as the crate's features say (see
/// the [module documentation](self)) if nothing has opened one yet.
pub fn library() -> Result<&'static Library, LoadError> {
	get_or_open(open_default)
}

/// The library, if one has been opened.
pub fn loaded() -> Option<&'static Library> {
	LIBRARY.get()
}

/// Opens the Slang library at `path` for this process.
///
/// Must come before anything else uses Slang. Once a library is open, asking
/// for the same file again succeeds and asking for another one is an error.
/// With the `system` feature there is nothing to open and this is an error.
pub fn load(path: impl AsRef<Path>) -> Result<&'static Library, LoadError> {
	let path = path.as_ref();
	#[cfg(feature = "system")]
	{
		Err(LoadError::new(format!(
			"cannot open {}: shader-slang-sys was built with the `system` feature, which links Slang",
			path.display()
		)))
	}
	#[cfg(not(feature = "system"))]
	{
		let library = get_or_open(|| open(path))?;
		let same = library.path().is_some_and(|loaded| {
			loaded == path
				|| matches!(
					(loaded.canonicalize(), path.canonicalize()),
					(Ok(a), Ok(b)) if a == b
				)
		});
		if same {
			Ok(library)
		} else {
			Err(LoadError::new(format!(
				"cannot open {}: this process already uses the Slang library at {}",
				path.display(),
				library
					.path()
					.map_or_else(|| "(linked)".into(), |p| p.display().to_string())
			)))
		}
	}
}

/// The function table, for the wrappers in functions.rs.
#[cfg_attr(feature = "system", allow(dead_code))]
pub(crate) fn functions() -> &'static Functions {
	match library() {
		Ok(library) => &library.functions,
		Err(err) => panic!("the Slang library is not available: {err}"),
	}
}

#[cfg_attr(feature = "system", allow(dead_code))]
pub(crate) fn missing(name: &str) -> ! {
	let path = loaded()
		.and_then(Library::path)
		.map_or_else(String::new, |p| format!(" at {}", p.display()));
	panic!("the Slang library{path} does not export {name}")
}

#[cfg(feature = "system")]
fn open_default() -> Result<Library, LoadError> {
	Ok(Library {
		path: None,
		functions: Functions::linked(),
	})
}

#[cfg(all(not(feature = "system"), feature = "embed"))]
fn open_default() -> Result<Library, LoadError> {
	crate::embedded::open()
}

#[cfg(all(not(feature = "system"), not(feature = "embed")))]
fn open_default() -> Result<Library, LoadError> {
	let names: &[&str] = if cfg!(windows) {
		&["slang-compiler.dll", "slang.dll"]
	} else if cfg!(target_vendor = "apple") {
		&["libslang-compiler.dylib", "libslang.dylib"]
	} else {
		&["libslang-compiler.so", "libslang.so"]
	};
	let mut failures = Vec::new();
	for name in names {
		match open(Path::new(name)) {
			Ok(library) => return Ok(library),
			Err(err) => failures.push(err.to_string()),
		}
	}
	Err(LoadError::new(format!(
		"no Slang library found on the library search path (built without the `embed` feature): {}",
		failures.join("; ")
	)))
}

/// Opens the library at `path` and resolves its functions.
#[cfg(not(feature = "system"))]
pub(crate) fn open(path: &Path) -> Result<Library, LoadError> {
	let handle = unsafe { open_handle(path) }
		.map_err(|err| LoadError::new(format!("cannot open {}: {err}", path.display())))?;
	// SAFETY: a library exporting Slang's entry points is taken to be Slang,
	// of the version the bindings were generated from.
	let functions = unsafe { Functions::resolve(&handle) };
	if functions.slang_createGlobalSession2.is_none()
		&& functions.slang_createGlobalSession.is_none()
	{
		return Err(LoadError::new(format!(
			"{} is not a Slang library: it exports no slang_createGlobalSession",
			path.display()
		)));
	}
	Ok(Library {
		path: Some(path.to_path_buf()),
		functions,
		_handle: handle,
	})
}

#[cfg(all(not(feature = "system"), unix))]
unsafe fn open_handle(path: &Path) -> Result<libloading::Library, libloading::Error> {
	use libloading::os::unix::{Library, RTLD_LOCAL, RTLD_NOW};
	unsafe { Library::open(Some(path), RTLD_NOW | RTLD_LOCAL) }.map(Into::into)
}

#[cfg(all(not(feature = "system"), windows))]
unsafe fn open_handle(path: &Path) -> Result<libloading::Library, libloading::Error> {
	use libloading::os::windows::{LOAD_WITH_ALTERED_SEARCH_PATH, Library};
	// An absolute path also makes Windows look beside the DLL for the DLLs it
	// imports; a bare name keeps the standard search order.
	let flags = if path.is_absolute() {
		LOAD_WITH_ALTERED_SEARCH_PATH
	} else {
		0
	};
	unsafe { Library::load_with_flags(path, flags) }.map(Into::into)
}
