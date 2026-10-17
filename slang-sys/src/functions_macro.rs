// Declares Slang's C functions (the `spReflection*` family, `spComputeStringHash`
// and the `slang_*` entry points) from bindgen's `extern` blocks in
// functions.rs.
//
// With the `system` feature the library is linked at build time and the
// blocks are emitted as they are. Otherwise the library is opened at run time
// (see `library`): every function becomes a field of `Functions`, a table of
// pointers resolved once when the library is opened, and a wrapper of the same
// name and signature calls through it, so callers write `sys::spFoo(..)` either
// way.

#[cfg(feature = "system")]
macro_rules! slang_functions {
	($($block:tt)*) => {
		$($block)*

		/// Nothing to resolve: the functions are linked.
		pub(crate) struct Functions;

		impl Functions {
			pub(crate) const fn linked() -> Self {
				Functions
			}
		}
	};
}

#[cfg(not(feature = "system"))]
macro_rules! slang_functions {
	($(
		unsafe extern "C" {
			$(
				$(#[doc = $doc:literal])*
				$(#[link_name = $link:literal])?
				pub fn $name:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)?;
			)*
		}
	)*) => {
		/// Every C function of the library, resolved by name when it is opened.
		/// A symbol this build of Slang does not export is `None`, and calling
		/// it panics with its name.
		#[allow(non_snake_case)]
		pub(crate) struct Functions {
			$($(
				$name: Option<unsafe extern "C" fn($($ty),*) $(-> $ret)?>,
			)*)*
		}

		impl Functions {
			/// # Safety
			/// `library` must be a build of Slang whose exports match the
			/// signatures in functions.rs.
			pub(crate) unsafe fn resolve(library: &libloading::Library) -> Self {
				Functions {
					$($(
						$name: unsafe {
							$crate::library::resolve(library, [$($link,)? stringify!($name)][0])
						},
					)*)*
				}
			}
		}

		$($(
			$(#[doc = $doc])*
			///
			/// Resolved from the Slang library opened at run time; the first
			/// call opens it if nothing has (see [`library`](crate::library)).
			///
			/// # Safety
			/// As for the C function of the same name in `slang.h`.
			#[inline]
			pub unsafe fn $name($($arg: $ty),*) $(-> $ret)? {
				let f = $crate::library::functions()
					.$name
					.unwrap_or_else(|| $crate::library::missing(stringify!($name)));
				unsafe { f($($arg),*) }
			}
		)*)*
	};
}
