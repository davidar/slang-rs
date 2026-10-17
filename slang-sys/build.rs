use std::env;

#[cfg(feature = "embed")]
#[path = "build/embed.rs"]
mod embed;

fn main() {
	println!("cargo:rerun-if-changed=build.rs");
	println!("cargo:rerun-if-env-changed=SLANG_DIR");
	println!("cargo:rerun-if-env-changed=SLANG_INCLUDE_DIR");
	println!("cargo:rerun-if-env-changed=SLANG_LIB_DIR");
	println!("cargo:rerun-if-env-changed=VULKAN_SDK");

	#[cfg(feature = "bindgen")]
	generate::bindings();

	// Without `system` the library is opened at run time and nothing is
	// linked here (see src/library.rs). The embedded library; `system`
	// takes precedence when both are set.
	if env::var_os("CARGO_FEATURE_SYSTEM").is_some() {
		link();
	} else {
		#[cfg(feature = "embed")]
		embed::run();
	}
}

/// The directory named by `SLANG_<KIND>_DIR`, else `SLANG_DIR/<kind>`, else
/// the Vulkan SDK's copy.
fn slang_dir(kind: &str, vulkan_sdk_subdir: &str) -> Option<String> {
	let upper = kind.to_ascii_uppercase();
	if let Ok(dir) = env::var(format!("SLANG_{upper}_DIR")) {
		Some(dir)
	} else if let Ok(dir) = env::var("SLANG_DIR") {
		Some(format!("{dir}/{kind}"))
	} else if let Ok(dir) = env::var("VULKAN_SDK") {
		Some(format!("{dir}/{vulkan_sdk_subdir}"))
	} else {
		None
	}
}

fn link() {
	let lib_dir = slang_dir("lib", "lib").unwrap_or_else(|| {
		panic!(
			"The `system` feature links Slang: the environment variable SLANG_LIB_DIR, SLANG_DIR, or VULKAN_SDK must be set"
		)
	});

	if !lib_dir.is_empty() {
		println!("cargo:rustc-link-search=native={lib_dir}");
	}

	println!("cargo:rustc-link-lib=dylib=slang");
}

/// Regenerates the bindings from a Slang install's `slang.h` (the `bindgen`
/// feature). The crate then compiles against the fresh output in `OUT_DIR`;
/// with `SHADER_SLANG_SYS_BINDINGS_OUT` set, the two files are also written
/// to that directory, which is how `generate-bindings.sh` refreshes the
/// checked-in copies in `src/`. With `SHADER_SLANG_ENUMS_OUT` set, the safe
/// crate's Rust enums are written to that file (its `src/enums.rs`).
#[cfg(feature = "bindgen")]
mod generate {
	use std::collections::HashMap;
	use std::env;
	use std::path::Path;

	pub fn bindings() {
		println!("cargo:rerun-if-env-changed=SHADER_SLANG_SYS_BINDINGS_OUT");
		println!("cargo:rerun-if-env-changed=SHADER_SLANG_ENUMS_OUT");

		let include_dir = super::slang_dir("include", "include/slang").unwrap_or_else(|| {
			panic!(
				"The bindgen feature needs slang.h: set SLANG_INCLUDE_DIR, SLANG_DIR, or VULKAN_SDK"
			)
		});
		let header = format!("{include_dir}/slang.h");
		println!("cargo:rerun-if-changed={header}");

		let out_dir = env::var("OUT_DIR").expect("Couldn't determine output directory.");
		let copy_to = env::var_os("SHADER_SLANG_SYS_BINDINGS_OUT");

		// Types and constants in one file, the C functions in another: the
		// functions are declared differently depending on how the library
		// is reached, so they are kept apart from the rest. One bindgen run
		// (the allowlist pulls in the types the functions use), split by
		// its top-level `extern` blocks.
		let bindings = builder(&header)
			.with_codegen_config(
				bindgen::CodegenConfig::FUNCTIONS
					| bindgen::CodegenConfig::TYPES
					| bindgen::CodegenConfig::VARS,
			)
			.generate()
			.expect("Couldn't generate bindings.")
			.to_string();
		let (mut types, functions) = split_functions(&bindings);
		types.push_str(&format!(
			"\n/// The Slang release whose `slang.h` these bindings were generated from.\n\
			 pub const SLANG_BINDINGS_VERSION: &str = {:?};\n",
			tag_version(&include_dir)
		));

		let preamble = preamble(&include_dir);
		let enums = safe_enums(&bindings);
		for (name, body) in [("bindings.rs", types), ("functions.rs", functions)] {
			let text = format!("{preamble}{body}");
			std::fs::write(Path::new(&out_dir).join(name), &text)
				.expect("Couldn't write bindings.");
			if let Some(dir) = &copy_to {
				std::fs::write(Path::new(dir).join(name), &text).expect("Couldn't write bindings.");
			}
		}

		// The safe crate's Rust enums. This crate never compiles them; the
		// copy in OUT_DIR is only there to look at.
		let text = format!("{preamble}{enums}");
		std::fs::write(Path::new(&out_dir).join("enums.rs"), &text).expect("Couldn't write enums.");
		if let Some(path) = env::var_os("SHADER_SLANG_ENUMS_OUT") {
			std::fs::write(path, &text).expect("Couldn't write enums.");
		}
	}

	/// The sys enums the safe crate exposes as Rust enums, and their names
	/// there. Each becomes an enum with a variant per distinct value; a later
	/// constant with the same value becomes an associated constant.
	const SAFE_ENUMS: &[(&str, &str)] = &[
		("SlangBindingType", "BindingType"),
		("SlangCompileTarget", "CompileTarget"),
		("SlangDebugInfoLevel", "DebugInfoLevel"),
		("SlangDeclKind", "DeclKind"),
		("SlangFloatingPointMode", "FloatingPointMode"),
		("SlangImageFormat", "ImageFormat"),
		("SlangLayoutRules", "LayoutRules"),
		("SlangLineDirectiveMode", "LineDirectiveMode"),
		("SlangMatrixLayoutMode", "MatrixLayoutMode"),
		("SlangModifierID", "ModifierID"),
		("SlangOptimizationLevel", "OptimizationLevel"),
		("SlangParameterCategory", "ParameterCategory"),
		("SlangReflectionGenericArgType", "GenericArgType"),
		("SlangResourceAccess", "ResourceAccess"),
		("SlangScalarType", "ScalarType"),
		("SlangSourceLanguage", "SourceLanguage"),
		("SlangStage", "Stage"),
		("SlangTypeKind", "TypeKind"),
		("slang_CompilerOptionName", "CompilerOptionName"),
	];

	/// One constant of a newtype enum: its doc attributes, name and value.
	struct EnumConst {
		docs: Vec<String>,
		name: String,
		value: i64,
	}

	/// Parses the `impl SlangX { pub const V: SlangX = SlangX(n); }` block
	/// bindgen writes for each newtype enum in [`SAFE_ENUMS`].
	fn parse_newtype_enums(bindings: &str) -> HashMap<String, Vec<EnumConst>> {
		let mut enums: HashMap<String, Vec<EnumConst>> = HashMap::new();
		let mut current: Option<String> = None;
		let mut docs = Vec::new();
		// An item inside an impl block, which bindgen may wrap over lines.
		let mut item = String::new();
		for line in bindings.lines() {
			let line = line.trim();
			if let Some(name) = &current {
				if item.is_empty() && line == "}" {
					current = None;
					continue;
				}
				if !item.is_empty() {
					item.push(' ');
				}
				item.push_str(line);
				if item.starts_with("#[") {
					if item.ends_with(']') {
						docs.push(std::mem::take(&mut item));
					}
				} else if item.ends_with(';') {
					let parsed = item.strip_prefix("pub const ").and_then(|rest| {
						let (variant, rest) = rest.split_once(':')?;
						let value = rest.split_once('(')?.1.strip_suffix(");")?;
						Some((variant.trim().to_string(), value.trim().parse().ok()?))
					});
					let (variant, value) =
						parsed.unwrap_or_else(|| panic!("Couldn't parse enum constant `{item}`"));
					enums.entry(name.clone()).or_default().push(EnumConst {
						docs: std::mem::take(&mut docs),
						name: variant,
						value,
					});
					item.clear();
				}
			} else if let Some(name) = line
				.strip_prefix("impl ")
				.and_then(|r| r.strip_suffix(" {"))
				&& SAFE_ENUMS.iter().any(|(sys, _)| *sys == name)
			{
				current = Some(name.to_string());
				docs.clear();
			}
		}
		enums
	}

	/// The safe crate's `src/enums.rs`: a Rust enum for each entry of
	/// [`SAFE_ENUMS`], with `TryFrom` from the sys newtype and `From` back to it.
	fn safe_enums(bindings: &str) -> String {
		use std::fmt::Write;

		let enums = parse_newtype_enums(bindings);
		let mut out = String::new();
		let mut list = String::new();
		let o = &mut out;
		macro_rules! w {
			($($arg:tt)*) => { writeln!(o, $($arg)*).expect("String write") };
		}

		w!("//! Rust enums for the Slang enums the safe API reads and writes.");
		w!("//!");
		w!("//! Each has exactly the values of the Slang release the bindings were");
		w!("//! generated from. The sys crate's newtypes can hold any value, and");
		w!("//! `TryFrom` turns one these enums lack into an [`UnknownEnumValue`].");
		w!("//! The safe API panics with it instead: with the embedded release such a");
		w!("//! value cannot occur, so one means a different Slang library was");
		w!("//! opened (through `library::load` or the `system` feature), a version");
		w!("//! mismatch, which is treated like a function the library lacks.");
		w!();
		w!("use crate::{{UnknownEnumValue, sys}};");

		for (sys_name, name) in SAFE_ENUMS {
			let consts = enums
				.get(*sys_name)
				.filter(|c| !c.is_empty())
				.unwrap_or_else(|| panic!("{sys_name} is not a newtype enum in the bindings"));

			// The first constant with a value is the variant; later ones
			// with the same value are aliases of it.
			let mut variants: Vec<&EnumConst> = Vec::new();
			let mut alias_consts = Vec::new();
			for c in consts {
				match variants.iter().find(|v| v.value == c.value) {
					Some(v) => alias_consts.push((&c.name, &v.name)),
					None => variants.push(c),
				}
			}

			let s = format!("sys::{sys_name}");
			w!();
			w!("/// [`{s}`] as a Rust enum.");
			w!("#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]");
			w!("#[cfg_attr(feature = \"serde\", derive(serde::Serialize, serde::Deserialize))]");
			w!("pub enum {name} {{");
			for v in &variants {
				for d in &v.docs {
					w!("\t{d}");
				}
				w!("\t{},", v.name);
			}
			w!("}}");

			if !alias_consts.is_empty() {
				w!();
				w!("#[allow(non_upper_case_globals)]");
				w!("impl {name} {{");
				for (alias, target) in &alias_consts {
					w!("\tpub const {alias}: Self = Self::{target};");
				}
				w!("}}");
			}

			w!();
			w!("/// A value these bindings lack is an [`UnknownEnumValue`]; the safe API");
			w!("/// panics with it instead, as for a version mismatch.");
			w!("impl TryFrom<{s}> for {name} {{");
			w!("\ttype Error = UnknownEnumValue;");
			w!();
			w!("\tfn try_from(value: {s}) -> Result<Self, Self::Error> {{");
			w!("\t\tOk(match value {{");
			for v in &variants {
				w!("\t\t\t{s}::{0} => Self::{0},", v.name);
			}
			w!("\t\t\t{s}(raw) => {{");
			w!("\t\t\t\treturn Err(UnknownEnumValue {{");
			w!("\t\t\t\t\tenum_name: \"{sys_name}\",");
			w!("\t\t\t\t\tvalue: raw.into(),");
			w!("\t\t\t\t}});");
			w!("\t\t\t}}");
			w!("\t\t}})");
			w!("\t}}");
			w!("}}");

			w!();
			w!("impl From<{name}> for {s} {{");
			w!("\tfn from(value: {name}) -> Self {{");
			w!("\t\tmatch value {{");
			for v in &variants {
				w!("\t\t\t{name}::{0} => Self::{0},", v.name);
			}
			w!("\t\t}}");
			w!("\t}}");
			w!("}}");

			w!();
			w!("#[cfg(test)]");
			w!("impl {name} {{");
			w!("\t/// Every variant with its sys constant.");
			w!("\tpub(crate) const VARIANTS: &[(Self, {s})] = &[");
			for v in &variants {
				w!("\t\t(Self::{0}, {s}::{0}),", v.name);
			}
			w!("\t];");
			w!("}}");

			writeln!(list, "\t\t$m!({name}, {s});").expect("String write");
		}

		w!();
		w!("/// Calls `$m!(Enum, sys::SlangEnum)` for every enum above.");
		w!("#[cfg(test)]");
		w!("macro_rules! for_each_enum {{");
		w!("\t($m:ident) => {{");
		o.push_str(&list);
		w!("\t}};");
		w!("}}");
		w!();
		w!("#[cfg(test)]");
		w!("pub(crate) use for_each_enum;");
		out
	}

	/// Splits bindgen's output into everything else and its top-level
	/// `unsafe extern "C" { ... }` blocks (each opens and closes at column 0).
	fn split_functions(bindings: &str) -> (String, String) {
		let mut types = String::new();
		let mut functions = String::new();
		let mut in_block = false;
		let mut in_block_seen = false;
		for line in bindings.lines() {
			if line.starts_with("unsafe extern \"C\" {") {
				if !in_block_seen {
					functions.push_str("slang_functions! {\n");
					in_block_seen = true;
				}
				in_block = true;
			}
			let out = if in_block { &mut functions } else { &mut types };
			out.push_str(line);
			out.push('\n');
			if in_block && line == "}" {
				in_block = false;
			}
		}
		functions.push_str("}\n");
		(types, functions)
	}

	fn tag_version(include_dir: &str) -> String {
		std::fs::read_to_string(format!("{include_dir}/slang-tag-version.h"))
			.ok()
			.and_then(|text| {
				text.lines()
					.find_map(|line| line.trim().strip_prefix("#define SLANG_TAG_VERSION"))
					.map(|v| v.trim().trim_matches('"').to_string())
			})
			.unwrap_or_else(|| "unknown".into())
	}

	fn preamble(include_dir: &str) -> String {
		let version = tag_version(include_dir);
		format!(
			"// Generated by `slang-sys/generate-bindings.sh` with bindgen from `slang.h`\n\
			 // (Slang tag {version}). Do not edit by hand.\n\n"
		)
	}

	fn builder(header: &str) -> bindgen::Builder {
		bindgen::builder()
			.header(header)
			.clang_arg("-xc++")
			.clang_arg("-std=c++17")
			.allowlist_function("spReflection.*")
			.allowlist_function("spComputeStringHash")
			.allowlist_function("slang_.*")
			.allowlist_type("slang.*")
			.allowlist_var("SLANG_.*")
			.parse_callbacks(Box::new(ParseCallback {}))
			// Newtypes over the integer, not Rust enums: Slang may return a
			// value these bindings lack, and any bit pattern is a valid
			// newtype. The safe crate maps them onto Rust enums (enums.rs).
			.default_enum_style(bindgen::EnumVariation::NewType {
				is_bitfield: false,
				is_global: false,
			})
			.constified_enum("SlangProfileID")
			.constified_enum("SlangCapabilityID")
			// A base shape combined with flags (array, multisample, shadow,
			// feedback, combined), so any bit pattern is a valid value.
			.bitfield_enum("SlangResourceShape")
			.vtable_generation(true)
			.layout_tests(false)
			.derive_copy(true)
	}

	#[derive(Debug)]
	struct ParseCallback {}

	impl bindgen::callbacks::ParseCallbacks for ParseCallback {
		fn enum_variant_name(
			&self,
			enum_name: Option<&str>,
			original_variant_name: &str,
			_variant_value: bindgen::callbacks::EnumVariantValue,
		) -> Option<String> {
			let enum_name = enum_name?;

			// Map enum names to the part of their variant names that needs to be trimmed.
			// When an enum name is not in this map the code below will try to trim the enum name itself.
			let mut map = std::collections::HashMap::new();
			map.insert("SlangMatrixLayoutMode", "SlangMatrixLayout");
			map.insert("SlangCompileTarget", "Slang");

			let trim = map.get(enum_name).unwrap_or(&enum_name);
			let new_variant_name = pascal_case_from_snake_case(original_variant_name);
			let new_variant_name = new_variant_name.trim_start_matches(trim);
			Some(new_variant_name.to_string())
		}

		// The checked-in bindings serve builds with and without the `serde`
		// feature, so the derive is written conditionally.
		fn add_attributes(&self, info: &bindgen::callbacks::AttributeInfo<'_>) -> Vec<String> {
			let is_enum = info.kind == bindgen::callbacks::TypeKind::Enum
				|| info.name == "SlangResourceShape";
			if info.name.starts_with("Slang") && is_enum {
				return vec![
					"#[cfg_attr(feature = \"serde\", derive(serde::Serialize, serde::Deserialize))]"
						.into(),
				];
			}
			vec![]
		}
	}

	/// Converts `snake_case` or `SNAKE_CASE` to `PascalCase`.
	/// If the input is already in `PascalCase` it will be returned as is.
	fn pascal_case_from_snake_case(snake_case: &str) -> String {
		let mut result = String::new();

		let should_lower = snake_case
			.chars()
			.filter(|c| c.is_alphabetic())
			.all(|c| c.is_uppercase());

		for part in snake_case.split('_') {
			for (i, c) in part.chars().enumerate() {
				if i == 0 {
					result.push(c.to_ascii_uppercase());
				} else if should_lower {
					result.push(c.to_ascii_lowercase());
				} else {
					result.push(c);
				}
			}
		}

		result
	}
}
