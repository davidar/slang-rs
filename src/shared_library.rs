//! How a global session opens the shared libraries it loads at run time.
//!
//! Slang loads some of its parts on demand, by name, through an
//! `ISlangSharedLibraryLoader`: chiefly `slang-glslang`, which holds the
//! SPIR-V optimiser, validator and linker. A [`SharedLibraryLoader`] made
//! from Rust ([`SharedLibraryLoader::new`]) decides what each request gets:
//! a library of its own ([`SharedLibrary::new`] answers symbol lookups from
//! Rust, for a module linked into the binary), a library opened from
//! somewhere else, the default behaviour ([`DefaultLoader::load`]), or a
//! refusal. Install it with
//! [`GlobalSession::set_shared_library_loader`](crate::GlobalSession::set_shared_library_loader).
//!
//! A refused downstream compiler is not a failed compile: Slang still
//! returns code, without what that module would have done to it, and says
//! so only in the diagnostics (see [`Diagnostics`](crate::Diagnostics)).

use std::ffi::{CStr, CString, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::ptr::null_mut;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::{Error, IUnknown, Interface, Result, UUID, succeeded, sys, uuid, vcall};

const IID_UNKNOWN: UUID = uuid(0x00000000_0000_0000_c000000000000046);
const IID_CASTABLE: UUID = uuid(0x87ede0e1_4852_44b0_8bf2cb31874de239);

/// An `ISlangSharedLibraryLoader`: Slang's own, or one made from Rust with
/// [`new`](Self::new).
#[repr(transparent)]
#[derive(Clone)]
pub struct SharedLibraryLoader(IUnknown);

unsafe impl Interface for SharedLibraryLoader {
	type Vtable = sys::ISharedLibraryLoaderVtable;
	const IID: UUID = uuid(0x6264ab2b_a3e8_4a06_97f149bc2d2ab14d);
}

/// An `ISlangSharedLibrary`: a loaded library Slang looks symbols up in.
#[repr(transparent)]
#[derive(Clone)]
pub struct SharedLibrary(IUnknown);

unsafe impl Interface for SharedLibrary {
	type Vtable = sys::ISharedLibraryVtable;
	const IID: UUID = uuid(0x70dbc7c4_dc3b_4a07_ae7e752af6a81555);
}

/// What a [`SharedLibraryLoader`] made from Rust does with each request.
///
/// `path` is what Slang asks for: a library name without the platform's
/// prefix and extension (`slang-glslang-2026.14.1`), often with the
/// directory of the Slang library in front of it. `default` loads it the
/// way Slang would have without this loader. An `Err` refuses the request
/// (`Error::Code(sys::SLANG_E_NOT_FOUND)` is what a missing library is).
///
/// Called from whichever thread is compiling; a panic is caught and reported
/// to Slang as a failure.
pub trait LoadSharedLibrary: Send + Sync + 'static {
	fn load_shared_library(&self, path: &str, default: &DefaultLoader<'_>)
	-> Result<SharedLibrary>;
}

impl<F> LoadSharedLibrary for F
where
	F: Fn(&str, &DefaultLoader<'_>) -> Result<SharedLibrary> + Send + Sync + 'static,
{
	fn load_shared_library(
		&self,
		path: &str,
		default: &DefaultLoader<'_>,
	) -> Result<SharedLibrary> {
		self(path, default)
	}
}

/// The symbols of a [`SharedLibrary`] made from Rust: the address for a
/// name, or null when there is none.
pub trait FindSymbol: Send + Sync + 'static {
	fn find_symbol(&self, name: &CStr) -> *mut c_void;
}

impl<F> FindSymbol for F
where
	F: Fn(&CStr) -> *mut c_void + Send + Sync + 'static,
{
	fn find_symbol(&self, name: &CStr) -> *mut c_void {
		self(name)
	}
}

/// The loader in place before a Rust one was installed, offered to its
/// callback.
pub struct DefaultLoader<'a> {
	previous: Option<&'a SharedLibraryLoader>,
}

impl DefaultLoader<'_> {
	/// Loads `path` as Slang would have: through the loader installed before
	/// this one, if it was not Slang's own, else as Slang's own does
	/// ([`SharedLibrary::open`]).
	pub fn load(&self, path: &str) -> Result<SharedLibrary> {
		match self.previous {
			Some(previous) => previous.load(path),
			None => SharedLibrary::open(path),
		}
	}
}

impl SharedLibraryLoader {
	/// A loader whose requests `loader` answers.
	pub fn new(loader: impl LoadSharedLibrary) -> SharedLibraryLoader {
		let object = Box::new(LoaderObject {
			vtable: &LOADER_VTABLE,
			refs: AtomicU32::new(1),
			previous: OnceLock::new(),
			inner: Box::new(loader),
		});
		let ptr = Box::into_raw(object);
		unsafe { SharedLibraryLoader::from_raw(ptr) }.expect("Box::into_raw is not null")
	}

	/// Asks this loader for a library, as Slang would.
	pub fn load(&self, path: &str) -> Result<SharedLibrary> {
		let path = CString::new(path).map_err(|_| Error::Code(sys::SLANG_E_INVALID_ARG))?;
		let mut library = null_mut();
		let code = vcall!(self, loadSharedLibrary(path.as_ptr(), &mut library));
		let library = unsafe { SharedLibrary::from_raw(library) };
		match library {
			Some(library) if succeeded(code) => Ok(library),
			_ if !succeeded(code) => Err(Error::Code(code)),
			_ => Err(Error::Code(sys::SLANG_E_NOT_FOUND)),
		}
	}

	/// The Rust object behind this loader, if it is one made by [`new`](Self::new).
	fn object(&self) -> Option<&LoaderObject> {
		let ptr = self.as_ptr() as *const LoaderObject;
		// Every COM object starts with its vtable pointer; only ours point at
		// LOADER_VTABLE.
		let vtable = unsafe { *(ptr as *const *const sys::ISharedLibraryLoaderVtable) };
		std::ptr::eq(vtable, &LOADER_VTABLE).then(|| unsafe { &*ptr })
	}

	/// Records the loader this one replaces, the first time it is installed.
	pub(crate) fn remember_fallback(&self, current: Option<SharedLibraryLoader>) {
		let Some(object) = self.object() else { return };
		let current = current.filter(|c| c.as_ptr() != self.as_ptr());
		let _ = object.previous.set(current);
	}
}

impl SharedLibrary {
	/// A library whose symbols `symbols` provides.
	pub fn new(symbols: impl FindSymbol) -> SharedLibrary {
		let object = Box::new(LibraryObject {
			vtable: &LIBRARY_VTABLE,
			refs: AtomicU32::new(1),
			inner: Box::new(symbols),
		});
		let ptr = Box::into_raw(object);
		unsafe { SharedLibrary::from_raw(ptr) }.expect("Box::into_raw is not null")
	}

	/// Opens a library file as Slang's own loader does: a name without an
	/// extension gets the platform's prefix and extension
	/// (`dir/slang-glslang` is `dir/libslang-glslang.so` on Linux,
	/// `dir/slang-glslang.dll` on Windows), and a bare name is looked for
	/// beside the Slang library before the platform's search path, which is
	/// where Slang's own requests find it. The library stays open while
	/// Slang holds it.
	pub fn open(path: &str) -> Result<SharedLibrary> {
		let file = platform_file_name(path);
		let beside = if path.contains(['/', '\\']) {
			None
		} else {
			sys::library::loaded()
				.and_then(|library| library.path())
				.and_then(|path| path.parent())
				.filter(|dir| !dir.as_os_str().is_empty())
				.map(|dir| dir.join(&file))
		};
		for candidate in beside.into_iter().chain([PathBuf::from(&file)]) {
			if let Ok(library) = unsafe { libloading::Library::new(&candidate) } {
				return Ok(SharedLibrary::new(OpenedLibrary(library)));
			}
		}
		Err(Error::Code(sys::SLANG_E_NOT_FOUND))
	}

	/// The address of `name` in the library, or null.
	pub fn find_symbol(&self, name: &str) -> *mut c_void {
		match CString::new(name) {
			Ok(name) => vcall!(self, findSymbolAddressByName(name.as_ptr())),
			Err(_) => null_mut(),
		}
	}
}

fn platform_file_name(path: &str) -> String {
	let (dir, name) = match path.rfind(['/', '\\']) {
		Some(i) => path.split_at(i + 1),
		None => ("", path),
	};
	let has_extension = name.ends_with(".so")
		|| name.contains(".so.")
		|| name.ends_with(".dylib")
		|| name.ends_with(".dll");
	if has_extension {
		path.to_string()
	} else if cfg!(windows) {
		format!("{dir}{name}.dll")
	} else if cfg!(target_vendor = "apple") {
		format!("{dir}lib{name}.dylib")
	} else {
		format!("{dir}lib{name}.so")
	}
}

struct OpenedLibrary(libloading::Library);

impl FindSymbol for OpenedLibrary {
	fn find_symbol(&self, name: &CStr) -> *mut c_void {
		unsafe { self.0.get::<*mut c_void>(name.to_bytes_with_nul()) }
			.map_or(null_mut(), |symbol| *symbol)
	}
}

// ---------------------------------------------------------------------------
// The COM objects behind `new`. Each is a heap object whose first field is
// its vtable pointer, reference-counted, freed on the last release.

#[repr(C)]
struct LoaderObject {
	vtable: &'static sys::ISharedLibraryLoaderVtable,
	refs: AtomicU32,
	previous: OnceLock<Option<SharedLibraryLoader>>,
	inner: Box<dyn LoadSharedLibrary>,
}

#[repr(C)]
struct LibraryObject {
	vtable: &'static sys::ISharedLibraryVtable,
	refs: AtomicU32,
	inner: Box<dyn FindSymbol>,
}

static LOADER_VTABLE: sys::ISharedLibraryLoaderVtable = sys::ISharedLibraryLoaderVtable {
	_base: sys::ISlangUnknown__bindgen_vtable {
		ISlangUnknown_queryInterface: loader_query_interface,
		ISlangUnknown_addRef: add_ref::<LoaderObject>,
		ISlangUnknown_release: release::<LoaderObject>,
	},
	loadSharedLibrary: loader_load,
};

static LIBRARY_VTABLE: sys::ISharedLibraryVtable = sys::ISharedLibraryVtable {
	_base: sys::ICastableVtable {
		_base: sys::ISlangUnknown__bindgen_vtable {
			ISlangUnknown_queryInterface: library_query_interface,
			ISlangUnknown_addRef: add_ref::<LibraryObject>,
			ISlangUnknown_release: release::<LibraryObject>,
		},
		castAs: library_cast_as,
	},
	findSymbolAddressByName: library_find_symbol,
};

trait Counted {
	fn refs(&self) -> &AtomicU32;
}

impl Counted for LoaderObject {
	fn refs(&self) -> &AtomicU32 {
		&self.refs
	}
}

impl Counted for LibraryObject {
	fn refs(&self) -> &AtomicU32 {
		&self.refs
	}
}

unsafe extern "C" fn add_ref<T: Counted>(this: *mut sys::ISlangUnknown) -> u32 {
	let object = unsafe { &*(this as *const T) };
	object.refs().fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "C" fn release<T: Counted>(this: *mut sys::ISlangUnknown) -> u32 {
	let object = unsafe { &*(this as *const T) };
	let left = object.refs().fetch_sub(1, Ordering::Release) - 1;
	if left == 0 {
		std::sync::atomic::fence(Ordering::Acquire);
		drop(unsafe { Box::from_raw(this as *mut T) });
	}
	left
}

fn same(a: &UUID, b: &UUID) -> bool {
	a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

unsafe extern "C" fn loader_query_interface(
	this: *mut sys::ISlangUnknown,
	guid: *const sys::SlangUUID,
	out: *mut *mut c_void,
) -> sys::SlangResult {
	if out.is_null() || guid.is_null() {
		return sys::SLANG_E_INVALID_ARG;
	}
	let guid = unsafe { &*guid };
	if same(guid, &IID_UNKNOWN) || same(guid, &SharedLibraryLoader::IID) {
		unsafe { add_ref::<LoaderObject>(this) };
		unsafe { *out = this as *mut c_void };
		sys::SLANG_OK as _
	} else {
		unsafe { *out = null_mut() };
		sys::SLANG_E_NO_INTERFACE
	}
}

unsafe extern "C" fn loader_load(
	this: *mut c_void,
	path: *const c_char,
	out: *mut *mut sys::ISlangSharedLibrary,
) -> sys::SlangResult {
	if out.is_null() {
		return sys::SLANG_E_INVALID_ARG;
	}
	unsafe { *out = null_mut() };
	let object = unsafe { &*(this as *const LoaderObject) };
	let path = if path.is_null() {
		String::new()
	} else {
		unsafe { CStr::from_ptr(path) }
			.to_string_lossy()
			.into_owned()
	};
	let default = DefaultLoader {
		previous: object.previous.get().and_then(Option::as_ref),
	};
	let result = catch_unwind(AssertUnwindSafe(|| {
		object.inner.load_shared_library(&path, &default)
	}));
	match result {
		Ok(Ok(library)) => {
			unsafe { *out = library.into_raw() as *mut sys::ISlangSharedLibrary };
			sys::SLANG_OK as _
		}
		Ok(Err(Error::Code(code))) if !succeeded(code) => code,
		Ok(Err(_)) => sys::SLANG_E_NOT_FOUND,
		Err(_) => sys::SLANG_FAIL,
	}
}

unsafe extern "C" fn library_query_interface(
	this: *mut sys::ISlangUnknown,
	guid: *const sys::SlangUUID,
	out: *mut *mut c_void,
) -> sys::SlangResult {
	if out.is_null() {
		return sys::SLANG_E_INVALID_ARG;
	}
	let cast = unsafe { library_cast_as(this as *mut c_void, guid) };
	unsafe { *out = cast };
	if cast.is_null() {
		sys::SLANG_E_NO_INTERFACE
	} else {
		unsafe { add_ref::<LibraryObject>(this) };
		sys::SLANG_OK as _
	}
}

/// `ISlangCastable::castAs`: the object itself for the interfaces it has, no
/// reference taken.
unsafe extern "C" fn library_cast_as(
	this: *mut c_void,
	guid: *const sys::SlangUUID,
) -> *mut c_void {
	if guid.is_null() {
		return null_mut();
	}
	let guid = unsafe { &*guid };
	if same(guid, &IID_UNKNOWN) || same(guid, &IID_CASTABLE) || same(guid, &SharedLibrary::IID) {
		this
	} else {
		null_mut()
	}
}

unsafe extern "C" fn library_find_symbol(this: *mut c_void, name: *const c_char) -> *mut c_void {
	if name.is_null() {
		return null_mut();
	}
	let object = unsafe { &*(this as *const LibraryObject) };
	let name = unsafe { CStr::from_ptr(name) };
	catch_unwind(AssertUnwindSafe(|| object.inner.find_symbol(name))).unwrap_or(null_mut())
}
