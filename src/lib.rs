//! Rust bindings for the Slang shader language compiler

// Interface IDs are written in their 8-4-4-4-12 UUID form.
#![allow(clippy::unusual_byte_groupings)]

mod enums;
pub mod reflection;
mod shared_library;

#[cfg(test)]
mod tests;

use std::ffi::{CStr, CString, c_char, c_void};
use std::marker::PhantomData;
use std::path::Path;
use std::ptr::{NonNull, null, null_mut};

pub(crate) use shader_slang_sys as sys;

pub use shared_library::{
	DefaultLoader, FindSymbol, LoadSharedLibrary, SharedLibrary, SharedLibraryLoader,
};
pub use sys::library::{self, Library, LoadError};

/// The Slang library embedded in this binary (the `embed` feature).
#[cfg(all(feature = "embed", not(feature = "system")))]
pub use sys::embedded;

pub use enums::*;
pub use sys::{
	SlangReflectionGenericArg as GenericArg, SlangResourceShape as ResourceShape,
	SlangUUID as UUID, slang_Modifier as Modifier,
};

macro_rules! vcall {
	($self:expr, $method:ident($($args:expr),*)) => {
		unsafe { ($self.vtable().$method)($self.as_raw(), $($args),*) }
	};
}

pub(crate) use vcall;

const fn uuid(uuid: u128) -> UUID {
	UUID {
		data1: (uuid >> 96) as u32,
		data2: ((uuid >> 80) & 0xffff) as u16,
		data3: ((uuid >> 64) & 0xffff) as u16,
		data4: (uuid as u64).to_be_bytes(),
	}
}

pub enum Error {
	Code(sys::SlangResult),
	Blob(Blob),
	/// The Slang library could not be opened (see [`library`]).
	Load(LoadError),
}

impl std::fmt::Debug for Error {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Error::Code(code) => write!(f, "{}", code),
			Error::Blob(blob) => write!(f, "{}", blob.as_str().unwrap_or_default()),
			Error::Load(err) => write!(f, "{}", err),
		}
	}
}

impl std::fmt::Display for Error {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		std::fmt::Debug::fmt(self, f)
	}
}

unsafe impl Send for Error {}
unsafe impl Sync for Error {}
impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// An enum value Slang returned that these bindings do not know.
///
/// The `TryFrom` conversions from the sys enums return it. The safe API
/// panics with it instead: the embedded release cannot return such a value,
/// so one means a different Slang library was opened (through
/// [`library::load`] or the `system` feature), a version mismatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UnknownEnumValue {
	/// The sys type's name, such as `SlangTypeKind`.
	pub enum_name: &'static str,
	pub value: i64,
}

impl std::fmt::Display for UnknownEnumValue {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"Slang returned {} value {}, which these bindings (Slang {}) do not know; \
			 the library in use is not the release the crate was built for",
			self.enum_name,
			self.value,
			sys::SLANG_BINDINGS_VERSION
		)
	}
}

impl std::error::Error for UnknownEnumValue {}

/// An enum value read from Slang, as its Rust enum. Panics on a value the
/// bindings do not know (see [`UnknownEnumValue`]).
#[track_caller]
pub(crate) fn from_slang<S, T: TryFrom<S, Error = UnknownEnumValue>>(value: S) -> T {
	T::try_from(value).unwrap_or_else(|err| panic!("{err}"))
}

pub(crate) fn succeeded(result: sys::SlangResult) -> bool {
	result >= 0
}

/// A string for Slang, or `SLANG_E_INVALID_ARG` when it holds a NUL.
fn c_string(s: &str) -> Result<CString> {
	CString::new(s).map_err(|_| Error::Code(sys::SLANG_E_INVALID_ARG))
}

/// A C string Slang returned, as `&str`; `""` for a null pointer or text
/// that is not UTF-8.
///
/// # Safety
/// `ptr` is null or a NUL-terminated string that lives for `'a`.
unsafe fn str_from_ptr<'a>(ptr: *const c_char) -> &'a str {
	if ptr.is_null() {
		""
	} else {
		unsafe { CStr::from_ptr(ptr) }.to_str().unwrap_or_default()
	}
}

/// Everything Slang said while doing something, warnings included.
///
/// Slang reports warnings, and some errors it recovers from, on calls that
/// succeed: a compile whose SPIR-V optimiser module cannot be loaded returns
/// unoptimised code with only an error diagnostic to say so. The plain
/// methods ([`Session::load_module`], [`ComponentType::link`], ...) keep
/// upstream's behaviour of reporting diagnostics on failure only; each has a
/// `..._with_diagnostics` sibling that appends what Slang said to a
/// `Diagnostics`, on success and on failure alike. A caller that treats
/// warnings as errors, or prints the whole story of a failed compile, passes
/// one `Diagnostics` through every step and reads it at the end:
///
/// ```no_run
/// # fn main() -> shader_slang::Result<()> {
/// # let global_session = shader_slang::GlobalSession::new().unwrap();
/// # let session = global_session.create_session(&Default::default()).unwrap();
/// let mut diagnostics = shader_slang::Diagnostics::new();
/// let module = session.load_module_with_diagnostics("shader.slang", &mut diagnostics)?;
/// let entry = module.find_and_check_entry_point_with_diagnostics(
///     "main",
///     shader_slang::Stage::Compute,
///     &mut diagnostics,
/// )?;
/// let program = session.create_composite_component_type_with_diagnostics(
///     &[module.into(), entry.into()],
///     &mut diagnostics,
/// )?;
/// let linked = program.link_with_diagnostics(&mut diagnostics)?;
/// let code = linked.entry_point_code_with_diagnostics(0, 0, &mut diagnostics)?;
/// assert!(diagnostics.is_empty(), "{diagnostics}");
/// # Ok(())
/// # }
/// ```
///
/// On failure the error still carries the failing step's own diagnostics
/// ([`Error::Blob`]), and the `Diagnostics` holds them after everything
/// earlier steps reported.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Diagnostics {
	text: String,
}

impl Diagnostics {
	pub fn new() -> Self {
		Self::default()
	}

	/// All the text, verbatim, in the order it was reported.
	pub fn as_str(&self) -> &str {
		&self.text
	}

	pub fn into_string(self) -> String {
		self.text
	}

	/// Whether nothing but whitespace was reported.
	pub fn is_empty(&self) -> bool {
		self.text.trim().is_empty()
	}

	pub fn clear(&mut self) {
		self.text.clear();
	}

	/// Appends `text`, on a line of its own.
	pub fn push_str(&mut self, text: &str) {
		if text.is_empty() {
			return;
		}
		if !self.text.is_empty() && !self.text.ends_with('\n') {
			self.text.push('\n');
		}
		self.text.push_str(text);
	}

	/// Takes ownership of a diagnostics blob Slang returned, appends its
	/// text, and hands the blob back for an error.
	///
	/// # Safety
	/// `blob` is null or an owned `ISlangBlob` reference.
	unsafe fn take(&mut self, blob: *mut sys::ISlangBlob) -> Option<Blob> {
		let blob = unsafe { Blob::from_raw(blob) }?;
		self.push_str(&String::from_utf8_lossy(blob.as_slice()));
		Some(blob)
	}

	/// The result of one step: `value` if the step succeeded and produced
	/// one, else an error carrying its diagnostics.
	///
	/// # Safety
	/// As for [`take`](Self::take).
	unsafe fn finish<T>(
		&mut self,
		code: sys::SlangResult,
		value: Option<T>,
		blob: *mut sys::ISlangBlob,
	) -> Result<T> {
		let blob = unsafe { self.take(blob) };
		match value {
			Some(value) if succeeded(code) => Ok(value),
			_ => Err(match blob {
				Some(blob) => Error::Blob(blob),
				None if succeeded(code) => Error::Code(sys::SLANG_FAIL),
				None => Error::Code(code),
			}),
		}
	}
}

impl std::fmt::Display for Diagnostics {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(&self.text)
	}
}

impl std::fmt::Debug for Diagnostics {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		std::fmt::Debug::fmt(&self.text, f)
	}
}

#[derive(Clone, Copy)]
pub struct ProfileID(sys::SlangProfileID);

impl ProfileID {
	pub const UNKNOWN: ProfileID = ProfileID(sys::SlangProfileID_SlangProfileUnknown);

	pub fn is_unknown(&self) -> bool {
		self.0 == sys::SlangProfileID_SlangProfileUnknown
	}
}

#[derive(Clone, Copy)]
pub struct CapabilityID(sys::SlangCapabilityID);

impl CapabilityID {
	pub const UNKNOWN: CapabilityID = CapabilityID(sys::SlangCapabilityID_SlangCapabilityUnknown);

	pub fn is_unknown(&self) -> bool {
		self.0 == sys::SlangCapabilityID_SlangCapabilityUnknown
	}
}

/// A COM interface pointer that owns one reference.
///
/// # Safety
/// An implementor is `#[repr(transparent)]` over [`IUnknown`] (a single
/// non-null interface pointer), and `Vtable` is a prefix of the interface's
/// real vtable.
pub unsafe trait Interface: Sized + Clone {
	#[doc(hidden)]
	type Vtable;

	const IID: UUID;

	#[doc(hidden)]
	#[inline(always)]
	unsafe fn vtable(&self) -> &Self::Vtable {
		unsafe { &**self.as_raw::<*mut Self::Vtable>() }
	}

	#[doc(hidden)]
	#[inline(always)]
	unsafe fn as_raw<T>(&self) -> *mut T {
		unsafe { std::mem::transmute_copy(self) }
	}

	fn as_unknown(&self) -> &IUnknown {
		// SAFETY: It is always safe to treat an `Interface` as an `IUnknown`.
		unsafe { std::mem::transmute(self) }
	}

	/// The interface pointer, still owned by `self`.
	fn as_ptr(&self) -> *mut c_void {
		unsafe { self.as_raw() }
	}

	/// Wraps a pointer and takes over the one reference it carries (as an
	/// out-parameter of a Slang call does): dropping the wrapper releases it.
	/// `None` for a null pointer.
	///
	/// # Safety
	/// `ptr` is null or a pointer to this interface (`Self::IID`), and the
	/// caller owns the reference it gives up.
	unsafe fn from_raw<P>(ptr: *mut P) -> Option<Self> {
		const { assert!(std::mem::size_of::<Self>() == std::mem::size_of::<NonNull<c_void>>()) };
		let ptr = NonNull::new(ptr as *mut c_void)?;
		Some(unsafe { std::mem::transmute_copy::<NonNull<c_void>, Self>(&ptr) })
	}

	/// Wraps a pointer the caller does not own (as Slang returns from a
	/// getter), taking a reference of its own.
	///
	/// # Safety
	/// As for [`from_raw`](Self::from_raw), except that no reference is
	/// given up.
	unsafe fn from_raw_borrowed<P>(ptr: *mut P) -> Option<Self> {
		let this = unsafe { Self::from_raw(ptr) }?;
		vcall!(this.as_unknown(), ISlangUnknown_addRef());
		Some(this)
	}

	/// Gives up the wrapper's reference, returning the pointer; the caller
	/// releases it (or hands it to [`from_raw`](Self::from_raw)).
	fn into_raw(self) -> *mut c_void {
		let ptr = self.as_ptr();
		std::mem::forget(self);
		ptr
	}

	/// The same object as another interface, through `queryInterface`.
	fn cast<T: Interface>(&self) -> Option<T> {
		let mut out = null_mut();
		let code = vcall!(
			self.as_unknown(),
			ISlangUnknown_queryInterface(&T::IID, &mut out)
		);
		if succeeded(code) {
			unsafe { T::from_raw(out) }
		} else {
			None
		}
	}
}

#[repr(transparent)]
pub struct IUnknown(std::ptr::NonNull<std::ffi::c_void>);

unsafe impl Interface for IUnknown {
	type Vtable = sys::ISlangUnknown__bindgen_vtable;
	const IID: UUID = uuid(0x00000000_0000_0000_c000000000000046);
}

impl Clone for IUnknown {
	fn clone(&self) -> Self {
		vcall!(self, ISlangUnknown_addRef());
		Self(self.0)
	}
}

impl Drop for IUnknown {
	fn drop(&mut self) {
		vcall!(self, ISlangUnknown_release());
	}
}

#[repr(transparent)]
#[derive(Clone)]
pub struct Blob(IUnknown);

unsafe impl Interface for Blob {
	type Vtable = sys::IBlobVtable;
	const IID: UUID = uuid(0x8ba5fb08_5195_40e2_ac580d989c3a0102);
}

impl Blob {
	/// A blob holding a copy of `bytes` (through `slang_createBlob`), for
	/// the calls that take one, such as
	/// [`Session::load_module_from_ir_blob`].
	pub fn from_slice(bytes: &[u8]) -> Result<Blob> {
		sys::library::library().map_err(Error::Load)?;
		if bytes.is_empty() {
			return Err(Error::Code(sys::SLANG_E_INVALID_ARG));
		}
		let blob = unsafe { sys::slang_createBlob(bytes.as_ptr() as *const _, bytes.len()) };
		unsafe { Blob::from_raw(blob) }.ok_or(Error::Code(sys::SLANG_FAIL))
	}

	pub fn as_slice(&self) -> &[u8] {
		let ptr = vcall!(self, getBufferPointer());
		let size = vcall!(self, getBufferSize());
		if ptr.is_null() || size == 0 {
			return &[];
		}
		unsafe { std::slice::from_raw_parts(ptr as *const u8, size) }
	}

	pub fn as_str(&self) -> std::result::Result<&str, std::str::Utf8Error> {
		std::str::from_utf8(self.as_slice())
	}
}

#[repr(transparent)]
#[derive(Clone)]
pub struct GlobalSession(IUnknown);

unsafe impl Interface for GlobalSession {
	type Vtable = sys::IGlobalSessionVtable;
	const IID: UUID = uuid(0xc140b5fd_0c78_452e_ba7c1a1e70c7f71c);
}

impl GlobalSession {
	/// A global session with the core module, or `None` when the Slang
	/// library cannot be opened or the session cannot be created;
	/// [`try_new`](Self::try_new) says why.
	pub fn new() -> Option<GlobalSession> {
		Self::try_new().ok()
	}

	/// A global session with the core module.
	///
	/// The first call in a process opens the Slang library (see [`library`]).
	pub fn try_new() -> Result<GlobalSession> {
		sys::library::library().map_err(Error::Load)?;
		let mut global_session = null_mut();
		let code = unsafe {
			sys::slang_createGlobalSession(sys::SLANG_API_VERSION as _, &mut global_session)
		};
		Self::created(code, global_session)
	}

	pub fn new_without_core_module() -> Option<GlobalSession> {
		sys::library::library().ok()?;
		let mut global_session = null_mut();
		let code = unsafe {
			sys::slang_createGlobalSessionWithoutCoreModule(
				sys::SLANG_API_VERSION as _,
				&mut global_session,
			)
		};
		Self::created(code, global_session).ok()
	}

	fn created(
		code: sys::SlangResult,
		global_session: *mut sys::slang_IGlobalSession,
	) -> Result<GlobalSession> {
		// A failure that still hands back a session releases it here.
		let session = unsafe { GlobalSession::from_raw(global_session) };
		match session {
			Some(session) if succeeded(code) => Ok(session),
			_ if !succeeded(code) => Err(Error::Code(code)),
			_ => Err(Error::Code(sys::SLANG_FAIL)),
		}
	}

	pub fn create_session(&self, desc: &SessionDesc) -> Option<Session> {
		self.try_create_session(desc).ok()
	}

	/// As [`create_session`](Self::create_session), with the result code on
	/// failure.
	pub fn try_create_session(&self, desc: &SessionDesc) -> Result<Session> {
		let mut session = null_mut();
		let code = vcall!(self, createSession(&**desc, &mut session));
		let session = unsafe { Session::from_raw(session) };
		match session {
			Some(session) if succeeded(code) => Ok(session),
			_ if !succeeded(code) => Err(Error::Code(code)),
			_ => Err(Error::Code(sys::SLANG_FAIL)),
		}
	}

	pub fn find_profile(&self, name: &str) -> ProfileID {
		match CString::new(name) {
			Ok(name) => ProfileID(vcall!(self, findProfile(name.as_ptr()))),
			Err(_) => ProfileID::UNKNOWN,
		}
	}

	pub fn find_capability(&self, name: &str) -> CapabilityID {
		match CString::new(name) {
			Ok(name) => CapabilityID(vcall!(self, findCapability(name.as_ptr()))),
			Err(_) => CapabilityID::UNKNOWN,
		}
	}

	pub fn build_tag_string(&self) -> &str {
		let tag = vcall!(self, getBuildTagString());
		unsafe { str_from_ptr(tag) }
	}

	/// Installs `loader` as the way this global session opens the shared
	/// libraries it loads at run time (downstream compilers such as the
	/// `slang-glslang` SPIR-V optimiser); `None` restores Slang's own.
	///
	/// A loader made with [`SharedLibraryLoader::new`] remembers the loader
	/// it replaced the first time it is installed, and offers it to its
	/// callback as the fallback.
	pub fn set_shared_library_loader(&self, loader: Option<&SharedLibraryLoader>) {
		if let Some(loader) = loader {
			let current = self.shared_library_loader();
			loader.remember_fallback(current);
		}
		let ptr = loader.map_or(null_mut(), |l| unsafe { l.as_raw() });
		vcall!(self, setSharedLibraryLoader(ptr));
	}

	/// The loader set with
	/// [`set_shared_library_loader`](Self::set_shared_library_loader), or
	/// `None` while Slang's own is in place.
	pub fn shared_library_loader(&self) -> Option<SharedLibraryLoader> {
		let loader = vcall!(self, getSharedLibraryLoader());
		unsafe { SharedLibraryLoader::from_raw_borrowed(loader) }
	}
}

#[repr(transparent)]
#[derive(Clone)]
pub struct Session(IUnknown);

unsafe impl Interface for Session {
	type Vtable = sys::ISessionVtable;
	const IID: UUID = uuid(0x67618701_d116_468f_ab3b474bedce0e3d);
}

impl Session {
	/// The global session this session was created from.
	pub fn global_session(&self) -> Option<GlobalSession> {
		let global = vcall!(self, getGlobalSession());
		unsafe { GlobalSession::from_raw_borrowed(global) }
	}

	pub fn load_module(&self, name: &str) -> Result<Module> {
		self.load_module_with_diagnostics(name, &mut Diagnostics::new())
	}

	/// [`load_module`](Self::load_module), with everything Slang reported
	/// appended to `diagnostics` (see [`Diagnostics`]).
	pub fn load_module_with_diagnostics(
		&self,
		name: &str,
		diagnostics: &mut Diagnostics,
	) -> Result<Module> {
		let name = c_string(name)?;
		let mut blob = null_mut();
		let module = vcall!(self, loadModule(name.as_ptr(), &mut blob));
		unsafe { loaded_module(module, blob, diagnostics) }
	}

	pub fn load_module_from_source_string(
		&self,
		module_name: &str,
		path: &str,
		source: &str,
	) -> Result<Module> {
		self.load_module_from_source_string_with_diagnostics(
			module_name,
			path,
			source,
			&mut Diagnostics::new(),
		)
	}

	/// [`load_module_from_source_string`](Self::load_module_from_source_string),
	/// with everything Slang reported appended to `diagnostics`.
	///
	/// Slang keys a session's modules by name and by path: loading a second
	/// module under a name or path already loaded fails (an assertion in
	/// debug builds of Slang), so a caller that reloads edited source in one
	/// session gives each load a path of its own.
	pub fn load_module_from_source_string_with_diagnostics(
		&self,
		module_name: &str,
		path: &str,
		source: &str,
		diagnostics: &mut Diagnostics,
	) -> Result<Module> {
		let module_name = c_string(module_name)?;
		let path = c_string(path)?;
		let source = c_string(source)?;
		let mut blob = null_mut();
		let module = vcall!(
			self,
			loadModuleFromSourceString(
				module_name.as_ptr(),
				path.as_ptr(),
				source.as_ptr(),
				&mut blob
			)
		);
		unsafe { loaded_module(module, blob, diagnostics) }
	}

	/// A module from source text in a blob (`loadModuleFromSource`).
	pub fn load_module_from_source(
		&self,
		module_name: &str,
		path: &str,
		source: &Blob,
	) -> Result<Module> {
		self.load_module_from_source_with_diagnostics(
			module_name,
			path,
			source,
			&mut Diagnostics::new(),
		)
	}

	pub fn load_module_from_source_with_diagnostics(
		&self,
		module_name: &str,
		path: &str,
		source: &Blob,
		diagnostics: &mut Diagnostics,
	) -> Result<Module> {
		let module_name = c_string(module_name)?;
		let path = c_string(path)?;
		let mut blob = null_mut();
		let module = vcall!(
			self,
			loadModuleFromSource(
				module_name.as_ptr(),
				path.as_ptr(),
				source.as_raw(),
				&mut blob
			)
		);
		unsafe { loaded_module(module, blob, diagnostics) }
	}

	pub fn load_module_from_ir_blob(
		&self,
		module_name: &str,
		path: &str,
		ir_blob: &Blob,
	) -> Result<Module> {
		self.load_module_from_ir_blob_with_diagnostics(
			module_name,
			path,
			ir_blob,
			&mut Diagnostics::new(),
		)
	}

	/// [`load_module_from_ir_blob`](Self::load_module_from_ir_blob), with
	/// everything Slang reported appended to `diagnostics`. The blob is what
	/// [`Module::serialize`] produced; [`Blob::from_slice`] turns stored
	/// bytes back into one.
	pub fn load_module_from_ir_blob_with_diagnostics(
		&self,
		module_name: &str,
		path: &str,
		ir_blob: &Blob,
		diagnostics: &mut Diagnostics,
	) -> Result<Module> {
		let module_name = c_string(module_name)?;
		let path = c_string(path)?;
		let mut blob = null_mut();
		let module = vcall!(
			self,
			loadModuleFromIRBlob(
				module_name.as_ptr(),
				path.as_ptr(),
				ir_blob.as_raw(),
				&mut blob
			)
		);
		unsafe { loaded_module(module, blob, diagnostics) }
	}

	/// Whether `blob` (a serialized module) is up to date with the source at
	/// `module_path`.
	pub fn is_binary_module_up_to_date(&self, module_path: &str, blob: &Blob) -> bool {
		match CString::new(module_path) {
			Ok(path) => vcall!(self, isBinaryModuleUpToDate(path.as_ptr(), blob.as_raw())),
			Err(_) => false,
		}
	}

	/// How many modules the session holds, imports included.
	pub fn loaded_module_count(&self) -> i64 {
		vcall!(self, getLoadedModuleCount())
	}

	pub fn loaded_module(&self, index: i64) -> Option<Module> {
		let module = vcall!(self, getLoadedModule(index as _));
		unsafe { Module::from_raw_borrowed(module) }
	}

	pub fn loaded_modules(&self) -> impl ExactSizeIterator<Item = Module> + '_ {
		(0..self.loaded_module_count().max(0))
			.filter_map(|i| self.loaded_module(i))
			.collect::<Vec<_>>()
			.into_iter()
	}

	pub fn create_composite_component_type(
		&self,
		components: &[ComponentType],
	) -> Result<ComponentType> {
		self.create_composite_component_type_with_diagnostics(components, &mut Diagnostics::new())
	}

	/// [`create_composite_component_type`](Self::create_composite_component_type),
	/// with everything Slang reported appended to `diagnostics`.
	pub fn create_composite_component_type_with_diagnostics(
		&self,
		components: &[ComponentType],
		diagnostics: &mut Diagnostics,
	) -> Result<ComponentType> {
		let mut composite = null_mut();
		let mut blob = null_mut();
		// `ComponentType` is a transparent pointer, so the slice is the
		// array of interface pointers Slang takes.
		let code = vcall!(
			self,
			createCompositeComponentType(
				components.as_ptr() as _,
				components.len() as _,
				&mut composite,
				&mut blob
			)
		);
		let composite = unsafe { ComponentType::from_raw(composite) };
		unsafe { diagnostics.finish(code, composite, blob) }
	}
}

/// A module `loadModule` and its siblings returned: the session keeps the
/// reference it hands back, so the wrapper takes one of its own.
///
/// # Safety
/// `module` is null or an `IModule` the session owns; `blob` as for
/// [`Diagnostics::take`].
unsafe fn loaded_module(
	module: *mut sys::slang_IModule,
	blob: *mut sys::ISlangBlob,
	diagnostics: &mut Diagnostics,
) -> Result<Module> {
	let module = unsafe { Module::from_raw_borrowed(module) };
	let code = if module.is_some() {
		sys::SLANG_OK as sys::SlangResult
	} else {
		sys::SLANG_FAIL
	};
	unsafe { diagnostics.finish(code, module, blob) }
}

#[repr(transparent)]
#[derive(Clone)]
pub struct Metadata(IUnknown);

unsafe impl Interface for Metadata {
	type Vtable = sys::IMetadataVtable;
	const IID: UUID = uuid(0x8044a8a3_ddc0_4b7f_af8e026e905d7332);
}

impl Metadata {
	pub fn is_parameter_location_used(
		&self,
		category: ParameterCategory,
		space_index: u64,
		register_index: u64,
	) -> Option<bool> {
		let mut used = false;
		let result = vcall!(
			self,
			isParameterLocationUsed(category.into(), space_index, register_index, &mut used)
		);
		succeeded(result).then_some(used)
	}
}

#[repr(transparent)]
#[derive(Clone)]
pub struct ComponentType(IUnknown);

unsafe impl Interface for ComponentType {
	type Vtable = sys::IComponentTypeVtable;
	const IID: UUID = uuid(0x5bc42be8_5c50_4929_9e5ed15e7c24015f);
}

impl ComponentType {
	pub fn layout(&self, target: i64) -> Result<&reflection::Shader> {
		self.layout_with_diagnostics(target, &mut Diagnostics::new())
	}

	/// [`layout`](Self::layout), with everything Slang reported appended to
	/// `diagnostics`.
	pub fn layout_with_diagnostics(
		&self,
		target: i64,
		diagnostics: &mut Diagnostics,
	) -> Result<&reflection::Shader> {
		let mut blob = null_mut();
		let ptr = vcall!(self, getLayout(target, &mut blob));
		let layout = (!ptr.is_null()).then(|| unsafe { &*(ptr as *const reflection::Shader) });
		unsafe { diagnostics.finish(sys::SLANG_OK as _, layout, blob) }
	}

	pub fn link(&self) -> Result<ComponentType> {
		self.link_with_diagnostics(&mut Diagnostics::new())
	}

	/// [`link`](Self::link), with everything Slang reported appended to
	/// `diagnostics`.
	pub fn link_with_diagnostics(&self, diagnostics: &mut Diagnostics) -> Result<ComponentType> {
		let mut linked = null_mut();
		let mut blob = null_mut();
		let code = vcall!(self, link(&mut linked, &mut blob));
		let linked = unsafe { ComponentType::from_raw(linked) };
		unsafe { diagnostics.finish(code, linked, blob) }
	}

	pub fn target_code(&self, target: i64) -> Result<Blob> {
		self.target_code_with_diagnostics(target, &mut Diagnostics::new())
	}

	/// [`target_code`](Self::target_code), with everything Slang reported
	/// appended to `diagnostics`.
	pub fn target_code_with_diagnostics(
		&self,
		target: i64,
		diagnostics: &mut Diagnostics,
	) -> Result<Blob> {
		let mut code = null_mut();
		let mut blob = null_mut();
		let result = vcall!(self, getTargetCode(target, &mut code, &mut blob));
		let code = unsafe { Blob::from_raw(code) };
		unsafe { diagnostics.finish(result, code, blob) }
	}

	pub fn entry_point_code(&self, index: i64, target: i64) -> Result<Blob> {
		self.entry_point_code_with_diagnostics(index, target, &mut Diagnostics::new())
	}

	/// [`entry_point_code`](Self::entry_point_code), with everything Slang
	/// reported appended to `diagnostics`. Code generation is where a missing
	/// downstream compiler shows: the code comes back, unoptimised, and only
	/// the diagnostics say why.
	pub fn entry_point_code_with_diagnostics(
		&self,
		index: i64,
		target: i64,
		diagnostics: &mut Diagnostics,
	) -> Result<Blob> {
		let mut code = null_mut();
		let mut blob = null_mut();
		let result = vcall!(self, getEntryPointCode(index, target, &mut code, &mut blob));
		let code = unsafe { Blob::from_raw(code) };
		unsafe { diagnostics.finish(result, code, blob) }
	}

	pub fn target_metadata(&self, target_index: i64) -> Result<Metadata> {
		let mut metadata = null_mut();
		let mut blob = null_mut();
		let code = vcall!(
			self,
			getTargetMetadata(target_index, &mut metadata, &mut blob)
		);
		let metadata = unsafe { Metadata::from_raw(metadata) };
		unsafe { Diagnostics::new().finish(code, metadata, blob) }
	}

	pub fn entry_point_metadata(
		&self,
		entry_point_index: i64,
		target_index: i64,
	) -> Result<Metadata> {
		let mut metadata = null_mut();
		let mut blob = null_mut();
		let code = vcall!(
			self,
			getEntryPointMetadata(entry_point_index, target_index, &mut metadata, &mut blob)
		);
		let metadata = unsafe { Metadata::from_raw(metadata) };
		unsafe { Diagnostics::new().finish(code, metadata, blob) }
	}
}

#[repr(transparent)]
#[derive(Clone)]
pub struct EntryPoint(IUnknown);

unsafe impl Interface for EntryPoint {
	type Vtable = sys::IEntryPointVtable;
	const IID: UUID = uuid(0x8f241361_f5bd_4ca0_a3ac02f7fa2402b8);
}

impl From<EntryPoint> for ComponentType {
	fn from(value: EntryPoint) -> Self {
		unsafe { std::mem::transmute(value) }
	}
}

impl EntryPoint {
	pub fn function_reflection(&self) -> &reflection::Function {
		let ptr = vcall!(self, getFunctionReflection());
		unsafe { &*(ptr as *const _) }
	}
}

#[repr(transparent)]
#[derive(Clone)]
pub struct TypeConformance(IUnknown);

unsafe impl Interface for TypeConformance {
	type Vtable = sys::ITypeConformanceVtable;
	const IID: UUID = uuid(0x73eb3147_e544_41b5_b8f0a244df21940b);
}

impl From<TypeConformance> for ComponentType {
	fn from(value: TypeConformance) -> Self {
		unsafe { std::mem::transmute(value) }
	}
}

#[repr(transparent)]
#[derive(Clone)]
pub struct Module(IUnknown);

unsafe impl Interface for Module {
	type Vtable = sys::IModuleVtable;
	const IID: UUID = uuid(0x0c720e64_8722_4d31_8990638a98b1c279);
}

impl From<Module> for ComponentType {
	fn from(value: Module) -> Self {
		unsafe { std::mem::transmute(value) }
	}
}

impl Module {
	/// An entry point marked in the source with a `[shader("...")]`
	/// attribute.
	pub fn find_entry_point_by_name(&self, name: &str) -> Option<EntryPoint> {
		let name = CString::new(name).ok()?;
		let mut entry_point = null_mut();
		let code = vcall!(self, findEntryPointByName(name.as_ptr(), &mut entry_point));
		let entry_point = unsafe { EntryPoint::from_raw(entry_point) };
		entry_point.filter(|_| succeeded(code))
	}

	/// An entry point by name and stage, whether or not the source marks it
	/// with a `[shader("...")]` attribute: what slangc's
	/// `-entry <name> -stage <stage>` does. The function is checked as an
	/// entry point of that stage.
	pub fn find_and_check_entry_point(&self, name: &str, stage: Stage) -> Result<EntryPoint> {
		self.find_and_check_entry_point_with_diagnostics(name, stage, &mut Diagnostics::new())
	}

	/// [`find_and_check_entry_point`](Self::find_and_check_entry_point), with
	/// everything Slang reported appended to `diagnostics`.
	pub fn find_and_check_entry_point_with_diagnostics(
		&self,
		name: &str,
		stage: Stage,
		diagnostics: &mut Diagnostics,
	) -> Result<EntryPoint> {
		let name = c_string(name)?;
		let mut entry_point = null_mut();
		let mut blob = null_mut();
		let code = vcall!(
			self,
			findAndCheckEntryPoint(name.as_ptr(), stage.into(), &mut entry_point, &mut blob)
		);
		let entry_point = unsafe { EntryPoint::from_raw(entry_point) };
		unsafe { diagnostics.finish(code, entry_point, blob) }
	}

	pub fn entry_point_count(&self) -> u32 {
		vcall!(self, getDefinedEntryPointCount()) as _
	}

	pub fn entry_point_by_index(&self, index: u32) -> Option<EntryPoint> {
		let mut entry_point = null_mut();
		let code = vcall!(self, getDefinedEntryPoint(index as _, &mut entry_point));
		let entry_point = unsafe { EntryPoint::from_raw(entry_point) };
		entry_point.filter(|_| succeeded(code))
	}

	pub fn entry_points(&self) -> impl ExactSizeIterator<Item = EntryPoint> {
		(0..self.entry_point_count()).map(|i| self.entry_point_by_index(i).unwrap())
	}

	/// The module's IR, serialized: what
	/// [`Session::load_module_from_ir_blob`] loads back, in this or a later
	/// session, without parsing or checking the source again.
	pub fn serialize(&self) -> Result<Blob> {
		let mut blob = null_mut();
		let code = vcall!(self, serialize(&mut blob));
		let blob = unsafe { Blob::from_raw(blob) };
		match blob {
			Some(blob) if succeeded(code) => Ok(blob),
			_ if !succeeded(code) => Err(Error::Code(code)),
			_ => Err(Error::Code(sys::SLANG_FAIL)),
		}
	}

	/// Writes [`serialize`](Self::serialize)'s output to a file.
	pub fn write_to_file(&self, path: &str) -> Result<()> {
		let path = c_string(path)?;
		let code = vcall!(self, writeToFile(path.as_ptr()));
		if succeeded(code) {
			Ok(())
		} else {
			Err(Error::Code(code))
		}
	}

	pub fn name(&self) -> &str {
		let name = vcall!(self, getName());
		unsafe { str_from_ptr(name) }
	}

	pub fn file_path(&self) -> &str {
		let path = vcall!(self, getFilePath());
		unsafe { str_from_ptr(path) }
	}

	pub fn unique_identity(&self) -> &str {
		let identity = vcall!(self, getUniqueIdentity());
		unsafe { str_from_ptr(identity) }
	}

	pub fn dependency_file_count(&self) -> i32 {
		vcall!(self, getDependencyFileCount())
	}

	pub fn dependency_file_path(&self, index: i32) -> &str {
		let path = vcall!(self, getDependencyFilePath(index));
		unsafe { str_from_ptr(path) }
	}

	pub fn dependency_file_paths(&self) -> impl ExactSizeIterator<Item = &str> {
		(0..self.dependency_file_count()).map(|i| self.dependency_file_path(i))
	}

	pub fn module_reflection(&self) -> &reflection::Decl {
		let ptr = vcall!(self, getModuleReflection());
		unsafe { &*(ptr as *const _) }
	}
}

#[repr(transparent)]
pub struct TargetDesc<'a> {
	inner: sys::slang_TargetDesc,
	_phantom: PhantomData<&'a ()>,
}

impl std::ops::Deref for TargetDesc<'_> {
	type Target = sys::slang_TargetDesc;

	fn deref(&self) -> &Self::Target {
		&self.inner
	}
}

impl Default for TargetDesc<'_> {
	fn default() -> Self {
		Self {
			inner: sys::slang_TargetDesc {
				structureSize: std::mem::size_of::<sys::slang_TargetDesc>(),
				..unsafe { std::mem::zeroed() }
			},
			_phantom: PhantomData,
		}
	}
}

impl<'a> TargetDesc<'a> {
	pub fn format(mut self, format: CompileTarget) -> Self {
		self.inner.format = format.into();
		self
	}

	pub fn profile(mut self, profile: ProfileID) -> Self {
		self.inner.profile = profile.0;
		self
	}

	pub fn options(mut self, options: &'a CompilerOptions) -> Self {
		self.inner.compilerOptionEntries = options.options.as_ptr() as _;
		self.inner.compilerOptionEntryCount = options.options.len() as _;
		self
	}
}

pub struct SessionDesc<'a> {
	inner: sys::slang_SessionDesc,
	// The search paths, owned here so the pointers in `inner` stay valid for
	// as long as the descriptor does.
	search_paths: Vec<CString>,
	search_path_ptrs: Vec<*const c_char>,
	_phantom: PhantomData<&'a ()>,
}

impl std::ops::Deref for SessionDesc<'_> {
	type Target = sys::slang_SessionDesc;

	fn deref(&self) -> &Self::Target {
		&self.inner
	}
}

impl Default for SessionDesc<'_> {
	fn default() -> Self {
		Self {
			inner: sys::slang_SessionDesc {
				structureSize: std::mem::size_of::<sys::slang_SessionDesc>(),
				..unsafe { std::mem::zeroed() }
			},
			search_paths: Vec::new(),
			search_path_ptrs: Vec::new(),
			_phantom: PhantomData,
		}
	}
}

impl<'a> SessionDesc<'a> {
	pub fn targets(mut self, targets: &'a [TargetDesc]) -> Self {
		self.inner.targets = targets.as_ptr() as _;
		self.inner.targetCount = targets.len() as _;
		self
	}

	/// The directories `import` and `#include` search, in order (slangc's
	/// `-I`). The descriptor keeps its own copies of the strings.
	///
	/// A path that holds a NUL cannot reach Slang and is skipped. On Unix a
	/// path's bytes are passed as they are; elsewhere it is passed as UTF-8.
	pub fn search_paths<P: AsRef<Path>>(mut self, paths: impl IntoIterator<Item = P>) -> Self {
		self.search_paths = paths
			.into_iter()
			.filter_map(|path| path_c_string(path.as_ref()))
			.collect();
		self.search_path_ptrs = self.search_paths.iter().map(|p| p.as_ptr()).collect();
		self.inner.searchPaths = self.search_path_ptrs.as_ptr();
		self.inner.searchPathCount = self.search_path_ptrs.len() as _;
		self
	}

	pub fn options(mut self, options: &'a CompilerOptions) -> Self {
		self.inner.compilerOptionEntries = options.options.as_ptr() as _;
		self.inner.compilerOptionEntryCount = options.options.len() as _;
		self
	}
}

#[cfg(unix)]
fn path_c_string(path: &Path) -> Option<CString> {
	use std::os::unix::ffi::OsStrExt;
	CString::new(path.as_os_str().as_bytes()).ok()
}

#[cfg(not(unix))]
fn path_c_string(path: &Path) -> Option<CString> {
	CString::new(path.to_string_lossy().into_owned()).ok()
}

macro_rules! option {
	($name:ident, $func:ident($p_name:ident: $p_type:ident => $sys:ty)) => {
		#[inline(always)]
		pub fn $func(self, $p_name: $p_type) -> Self {
			self.push_ints(CompilerOptionName::$name, <$sys>::from($p_name).0 as _, 0)
		}
	};

	($name:ident, $func:ident($p_name:ident: $p_type:ident)) => {
		#[inline(always)]
		pub fn $func(self, $p_name: $p_type) -> Self {
			self.push_ints(CompilerOptionName::$name, $p_name as _, 0)
		}
	};

	($name:ident, $func:ident($p_name:ident: &str)) => {
		#[inline(always)]
		pub fn $func(self, $p_name: &str) -> Self {
			self.push_str1(CompilerOptionName::$name, $p_name)
		}
	};

	($name:ident, $func:ident($p_name1:ident: &str, $p_name2:ident: &str)) => {
		#[inline(always)]
		pub fn $func(self, $p_name1: &str, $p_name2: &str) -> Self {
			self.push_str2(CompilerOptionName::$name, $p_name1, $p_name2)
		}
	};
}

#[derive(Default)]
pub struct CompilerOptions {
	strings: Vec<CString>,
	options: Vec<sys::slang_CompilerOptionEntry>,
}

impl CompilerOptions {
	fn push_ints(mut self, name: CompilerOptionName, i0: i32, i1: i32) -> Self {
		self.options.push(sys::slang_CompilerOptionEntry {
			name: name.into(),
			value: sys::slang_CompilerOptionValue {
				kind: sys::slang_CompilerOptionValueKind::Int,
				intValue0: i0,
				intValue1: i1,
				stringValue0: null(),
				stringValue1: null(),
			},
		});

		self
	}

	fn push_strings(mut self, name: CompilerOptionName, s0: *const i8, s1: *const i8) -> Self {
		self.options.push(sys::slang_CompilerOptionEntry {
			name: name.into(),
			value: sys::slang_CompilerOptionValue {
				kind: sys::slang_CompilerOptionValueKind::String,
				intValue0: 0,
				intValue1: 0,
				stringValue0: s0,
				stringValue1: s1,
			},
		});

		self
	}

	fn push_str1(mut self, name: CompilerOptionName, s0: &str) -> Self {
		let s0 = CString::new(s0).unwrap();
		let s0_ptr = s0.as_ptr();
		self.strings.push(s0);

		self.push_strings(name, s0_ptr, null())
	}

	fn push_str2(mut self, name: CompilerOptionName, s0: &str, s1: &str) -> Self {
		let s0 = CString::new(s0).unwrap();
		let s0_ptr = s0.as_ptr();
		self.strings.push(s0);

		let s1 = CString::new(s1).unwrap();
		let s1_ptr = s1.as_ptr();
		self.strings.push(s1);

		self.push_strings(name, s0_ptr, s1_ptr)
	}
}

impl CompilerOptions {
	option!(MacroDefine, macro_define(key: &str, value: &str));
	option!(Include, include(path: &str));
	option!(Language, language(language: SourceLanguage => sys::SlangSourceLanguage));
	option!(MatrixLayoutColumn, matrix_layout_column(enable: bool));
	option!(MatrixLayoutRow, matrix_layout_row(enable: bool));

	#[inline(always)]
	pub fn profile(self, profile: ProfileID) -> Self {
		self.push_ints(CompilerOptionName::Profile, profile.0 as _, 0)
	}

	option!(Stage, stage(stage: Stage => sys::SlangStage));
	option!(Target, target(target: CompileTarget => sys::SlangCompileTarget));
	option!(WarningsAsErrors, warnings_as_errors(warning_codes: &str));
	option!(DisableWarnings, disable_warnings(warning_codes: &str));
	option!(EnableWarning, enable_warning(warning_code: &str));
	option!(DisableWarning, disable_warning(warning_code: &str));
	option!(ReportDownstreamTime, report_downstream_time(enable: bool));
	option!(ReportPerfBenchmark, report_perf_benchmark(enable: bool));
	option!(SkipSPIRVValidation, skip_spirv_validation(enable: bool));

	// Target
	#[inline(always)]
	pub fn capability(self, capability: CapabilityID) -> Self {
		self.push_ints(CompilerOptionName::Capability, capability.0 as _, 0)
	}

	option!(DefaultImageFormatUnknown, default_image_format_unknown(enable: bool));
	option!(DisableDynamicDispatch, disable_dynamic_dispatch(enable: bool));
	option!(DisableSpecialization, disable_specialization(enable: bool));
	option!(FloatingPointMode, floating_point_mode(mode: FloatingPointMode => sys::SlangFloatingPointMode));
	option!(DebugInformation, debug_information(level: DebugInfoLevel => sys::SlangDebugInfoLevel));
	option!(LineDirectiveMode, line_directive_mode(mode: LineDirectiveMode => sys::SlangLineDirectiveMode));
	option!(Optimization, optimization(level: OptimizationLevel => sys::SlangOptimizationLevel));
	option!(Obfuscate, obfuscate(enable: bool));
	option!(VulkanUseEntryPointName, vulkan_use_entry_point_name(enable: bool));
	option!(GLSLForceScalarLayout, glsl_force_scalar_layout(enable: bool));
	option!(EmitSpirvDirectly, emit_spirv_directly(enable: bool));

	// Debugging
	option!(NoCodeGen, no_code_gen(enable: bool));

	// Experimental
	option!(NoMangle, no_mangle(enable: bool));
	option!(ValidateUniformity, validate_uniformity(enable: bool));
}
