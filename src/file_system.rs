//! Where a session reads source files from.
//!
//! `import` and `#include` resolve to paths (each search path joined with
//! the name, as `shaders/text.slang`), and Slang reads each through the
//! session's `ISlangFileSystem`, by default the platform's. A [`FileSystem`]
//! made from Rust ([`FileSystem::new`]) answers those reads instead: from
//! memory, from somewhere else, or by deferring to the platform
//! ([`DefaultFileSystem::load`]). Install it with
//! [`SessionDesc::file_system`](crate::SessionDesc::file_system).
//!
//! The paths are what Slang built, so a module served from memory is asked
//! for under each search path in turn; match on the file name, or use one
//! search path. A file this file system cannot provide is a missing file
//! to Slang, which reports it in the diagnostics.

use std::ffi::{CStr, CString, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::null_mut;
use std::sync::atomic::AtomicU32;

use crate::shared_library::{Counted, IID_CASTABLE, IID_UNKNOWN, add_ref, release, same};
use crate::{Blob, Error, IUnknown, Interface, Result, UUID, succeeded, sys, uuid, vcall};

/// An `ISlangFileSystem`: the platform's, or one made from Rust with
/// [`new`](Self::new).
#[repr(transparent)]
#[derive(Clone)]
pub struct FileSystem(IUnknown);

unsafe impl Interface for FileSystem {
	type Vtable = sys::IFileSystemVtable;
	const IID: UUID = uuid(0x003a09fc_3a4d_4ba0_ad601fd863a915ab);
}

/// What a [`FileSystem`] made from Rust does with each read.
///
/// `path` is what Slang asks for, as it built it from the search paths and
/// the name. `default` reads it from the platform's file system, as Slang
/// would have without this file system. An `Err` is a file that cannot be
/// read (`Error::Code(sys::SLANG_E_NOT_FOUND)` is what a missing one is).
///
/// Called from whichever thread is compiling; a panic is caught and reported
/// to Slang as a failure.
pub trait LoadFile: Send + Sync + 'static {
	fn load_file(&self, path: &str, default: &DefaultFileSystem) -> Result<Vec<u8>>;
}

impl<F> LoadFile for F
where
	F: Fn(&str, &DefaultFileSystem) -> Result<Vec<u8>> + Send + Sync + 'static,
{
	fn load_file(&self, path: &str, default: &DefaultFileSystem) -> Result<Vec<u8>> {
		self(path, default)
	}
}

/// The platform's file system, as Slang reads it.
pub struct DefaultFileSystem {
	_private: (),
}

impl DefaultFileSystem {
	/// The bytes of the file at `path`, read as Slang's own file system
	/// would read them.
	pub fn load(&self, path: &str) -> Result<Vec<u8>> {
		std::fs::read(path).map_err(|err| {
			Error::Code(match err.kind() {
				std::io::ErrorKind::NotFound => sys::SLANG_E_NOT_FOUND,
				_ => sys::SLANG_FAIL,
			})
		})
	}
}

impl FileSystem {
	/// A file system whose reads `loader` answers.
	pub fn new(loader: impl LoadFile) -> FileSystem {
		let object = Box::new(FileSystemObject {
			vtable: &FILE_SYSTEM_VTABLE,
			refs: AtomicU32::new(1),
			inner: Box::new(loader),
		});
		let ptr = Box::into_raw(object);
		unsafe { FileSystem::from_raw(ptr) }.expect("Box::into_raw is not null")
	}

	/// Reads `path` through this file system, as Slang would.
	pub fn load(&self, path: &str) -> Result<Blob> {
		let path = CString::new(path).map_err(|_| Error::Code(sys::SLANG_E_INVALID_ARG))?;
		let mut blob = null_mut();
		let code = vcall!(self, loadFile(path.as_ptr(), &mut blob));
		let blob = unsafe { Blob::from_raw(blob) };
		match blob {
			Some(blob) if succeeded(code) => Ok(blob),
			_ if !succeeded(code) => Err(Error::Code(code)),
			_ => Err(Error::Code(sys::SLANG_E_NOT_FOUND)),
		}
	}
}

/// A blob that owns `bytes`, for [`Blob::from_vec`].
pub(crate) fn blob_from_vec(bytes: Vec<u8>) -> Blob {
	let object = Box::new(BlobObject {
		vtable: &BLOB_VTABLE,
		refs: AtomicU32::new(1),
		bytes,
	});
	let ptr = Box::into_raw(object);
	unsafe { Blob::from_raw(ptr) }.expect("Box::into_raw is not null")
}

// ---------------------------------------------------------------------------
// The COM objects, laid out as in shared_library.rs.

#[repr(C)]
struct FileSystemObject {
	vtable: &'static sys::IFileSystemVtable,
	refs: AtomicU32,
	inner: Box<dyn LoadFile>,
}

#[repr(C)]
struct BlobObject {
	vtable: &'static sys::IBlobVtable,
	refs: AtomicU32,
	bytes: Vec<u8>,
}

impl Counted for FileSystemObject {
	fn refs(&self) -> &AtomicU32 {
		&self.refs
	}
}

impl Counted for BlobObject {
	fn refs(&self) -> &AtomicU32 {
		&self.refs
	}
}

static FILE_SYSTEM_VTABLE: sys::IFileSystemVtable = sys::IFileSystemVtable {
	_base: sys::ICastableVtable {
		_base: sys::ISlangUnknown__bindgen_vtable {
			ISlangUnknown_queryInterface: file_system_query_interface,
			ISlangUnknown_addRef: add_ref::<FileSystemObject>,
			ISlangUnknown_release: release::<FileSystemObject>,
		},
		castAs: file_system_cast_as,
	},
	loadFile: file_system_load_file,
};

static BLOB_VTABLE: sys::IBlobVtable = sys::IBlobVtable {
	_base: sys::ISlangUnknown__bindgen_vtable {
		ISlangUnknown_queryInterface: blob_query_interface,
		ISlangUnknown_addRef: add_ref::<BlobObject>,
		ISlangUnknown_release: release::<BlobObject>,
	},
	getBufferPointer: blob_buffer_pointer,
	getBufferSize: blob_buffer_size,
};

unsafe extern "C" fn file_system_query_interface(
	this: *mut sys::ISlangUnknown,
	guid: *const sys::SlangUUID,
	out: *mut *mut c_void,
) -> sys::SlangResult {
	if out.is_null() {
		return sys::SLANG_E_INVALID_ARG;
	}
	let cast = unsafe { file_system_cast_as(this as *mut c_void, guid) };
	unsafe { *out = cast };
	if cast.is_null() {
		sys::SLANG_E_NO_INTERFACE
	} else {
		unsafe { add_ref::<FileSystemObject>(this) };
		sys::SLANG_OK as _
	}
}

/// `ISlangCastable::castAs`: the object itself for the interfaces it has,
/// no reference taken. Slang asks for the richer `ISlangFileSystemExt` and
/// `ISlangMutableFileSystem` too, and wraps a plain file system itself.
unsafe extern "C" fn file_system_cast_as(
	this: *mut c_void,
	guid: *const sys::SlangUUID,
) -> *mut c_void {
	if guid.is_null() {
		return null_mut();
	}
	let guid = unsafe { &*guid };
	if same(guid, &IID_UNKNOWN) || same(guid, &IID_CASTABLE) || same(guid, &FileSystem::IID) {
		this
	} else {
		null_mut()
	}
}

unsafe extern "C" fn file_system_load_file(
	this: *mut c_void,
	path: *const c_char,
	out: *mut *mut sys::ISlangBlob,
) -> sys::SlangResult {
	if out.is_null() {
		return sys::SLANG_E_INVALID_ARG;
	}
	unsafe { *out = null_mut() };
	let object = unsafe { &*(this as *const FileSystemObject) };
	let path = if path.is_null() {
		String::new()
	} else {
		unsafe { CStr::from_ptr(path) }
			.to_string_lossy()
			.into_owned()
	};
	let default = DefaultFileSystem { _private: () };
	let result = catch_unwind(AssertUnwindSafe(|| object.inner.load_file(&path, &default)));
	match result {
		Ok(Ok(bytes)) => {
			unsafe { *out = blob_from_vec(bytes).into_raw() as *mut sys::ISlangBlob };
			sys::SLANG_OK as _
		}
		Ok(Err(Error::Code(code))) if !succeeded(code) => code,
		Ok(Err(_)) => sys::SLANG_E_NOT_FOUND,
		Err(_) => sys::SLANG_FAIL,
	}
}

unsafe extern "C" fn blob_query_interface(
	this: *mut sys::ISlangUnknown,
	guid: *const sys::SlangUUID,
	out: *mut *mut c_void,
) -> sys::SlangResult {
	if out.is_null() || guid.is_null() {
		return sys::SLANG_E_INVALID_ARG;
	}
	let guid = unsafe { &*guid };
	if same(guid, &IID_UNKNOWN) || same(guid, &Blob::IID) {
		unsafe { add_ref::<BlobObject>(this) };
		unsafe { *out = this as *mut c_void };
		sys::SLANG_OK as _
	} else {
		unsafe { *out = null_mut() };
		sys::SLANG_E_NO_INTERFACE
	}
}

unsafe extern "C" fn blob_buffer_pointer(this: *mut c_void) -> *const c_void {
	let object = unsafe { &*(this as *const BlobObject) };
	object.bytes.as_ptr() as *const c_void
}

unsafe extern "C" fn blob_buffer_size(this: *mut c_void) -> usize {
	let object = unsafe { &*(this as *const BlobObject) };
	object.bytes.len()
}
