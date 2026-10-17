use std::sync::Arc;
use std::sync::Mutex;

use crate as slang;
use slang::Interface;

const SPIRV_MAGIC: u32 = 0x0723_0203;

fn global_session() -> slang::GlobalSession {
	match slang::GlobalSession::try_new() {
		Ok(session) => session,
		Err(err) => panic!("no global session: {err}"),
	}
}

/// A session for SPIR-V with the options the README example uses.
fn session(global_session: &slang::GlobalSession) -> slang::Session {
	let session_options = slang::CompilerOptions::default()
		.optimization(slang::OptimizationLevel::High)
		.matrix_layout_row(true);
	let targets = [slang::TargetDesc::default()
		.format(slang::CompileTarget::Spirv)
		.profile(global_session.find_profile("glsl_450"))];
	let session_desc = slang::SessionDesc::default()
		.targets(&targets)
		.search_paths(["shaders"])
		.options(&session_options);
	global_session.create_session(&session_desc).unwrap()
}

/// test.slang's `main`, compiled to SPIR-V, with everything Slang said.
fn compile_test_shader(global_session: &slang::GlobalSession) -> (Vec<u8>, slang::Diagnostics) {
	let session = session(global_session);
	let mut diagnostics = slang::Diagnostics::new();
	let module = session
		.load_module_with_diagnostics("test.slang", &mut diagnostics)
		.unwrap();
	let entry_point = module.find_entry_point_by_name("main").unwrap();
	let program = session
		.create_composite_component_type_with_diagnostics(
			&[module.into(), entry_point.into()],
			&mut diagnostics,
		)
		.unwrap();
	let linked = program.link_with_diagnostics(&mut diagnostics).unwrap();
	let code = linked
		.entry_point_code_with_diagnostics(0, 0, &mut diagnostics)
		.unwrap();
	(code.as_slice().to_vec(), diagnostics)
}

fn words(spirv: &[u8]) -> Vec<u32> {
	spirv
		.chunks_exact(4)
		.map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
		.collect()
}

/// Upstream's test, as it was, with the search path now a Rust string.
#[test]
fn compile() {
	let global_session = global_session();

	// All compiler options are available through this builder.
	let session_options = slang::CompilerOptions::default()
		.optimization(slang::OptimizationLevel::High)
		.matrix_layout_row(true);

	let target_desc = slang::TargetDesc::default()
		.format(slang::CompileTarget::Spirv)
		.profile(global_session.find_profile("glsl_450"));

	let targets = [target_desc];

	let session_desc = slang::SessionDesc::default()
		.targets(&targets)
		.search_paths(["shaders"])
		.options(&session_options);

	let session = global_session.create_session(&session_desc).unwrap();
	let module = session.load_module("test.slang").unwrap();
	let entry_point = module.find_entry_point_by_name("main").unwrap();

	let program = session
		.create_composite_component_type(&[module.into(), entry_point.into()])
		.unwrap();

	let linked_program = program.link().unwrap();

	// Entry point to the reflection API.
	let reflection = linked_program.layout(0).unwrap();
	assert_eq!(reflection.entry_point_count(), 1);
	assert_eq!(reflection.parameter_count(), 3);

	let shader_bytecode = linked_program.entry_point_code(0, 0).unwrap();
	assert_ne!(shader_bytecode.as_slice().len(), 0);
}

/// (a) test.slang compiles to SPIR-V, with nothing to report.
#[test]
fn compiles_to_spirv() {
	let (spirv, diagnostics) = compile_test_shader(&global_session());
	assert!(
		spirv.len() > 20 && spirv.len() % 4 == 0,
		"{} bytes",
		spirv.len()
	);
	assert_eq!(words(&spirv)[0], SPIRV_MAGIC);
	assert!(
		diagnostics.is_empty(),
		"unexpected diagnostics: {diagnostics}"
	);
}

/// (b) The SPIR-V optimiser module is present and used: refusing it through
/// a shared-library loader still compiles, to different code, and the only
/// sign is an error on the success path.
#[test]
fn optimiser_is_loaded() {
	let (normal, normal_diagnostics) = compile_test_shader(&global_session());
	assert!(
		normal_diagnostics.is_empty(),
		"a normal compile reported: {normal_diagnostics}"
	);

	let requests = Arc::new(Mutex::new(Vec::<String>::new()));
	let seen = requests.clone();
	let loader =
		slang::SharedLibraryLoader::new(move |path: &str, default: &slang::DefaultLoader<'_>| {
			seen.lock().unwrap().push(path.to_string());
			if path.contains("glslang") {
				Err(slang::Error::Code(shader_slang_sys::SLANG_E_NOT_FOUND))
			} else {
				default.load(path)
			}
		});
	let refusing = global_session();
	refusing.set_shared_library_loader(Some(&loader));
	assert!(refusing.shared_library_loader().is_some());

	let (refused, refused_diagnostics) = compile_test_shader(&refusing);
	let requests = requests.lock().unwrap().clone();
	assert!(
		requests.iter().any(|path| path.contains("glslang")),
		"the loader never saw a request for glslang: {requests:?}"
	);
	assert_eq!(words(&refused)[0], SPIRV_MAGIC);
	assert_ne!(normal, refused, "refusing the optimiser changed nothing");
	assert!(
		refused_diagnostics
			.as_str()
			.contains("failed to load downstream compiler"),
		"no downstream-compiler error among: {refused_diagnostics}"
	);

	refusing.set_shared_library_loader(None);
	assert!(refusing.shared_library_loader().is_none());
}

/// A loader made from Rust can also hand back a library of its own.
#[test]
fn shared_library_objects() {
	let marker = Box::leak(Box::new(7u32)) as *mut u32 as usize;
	let library = slang::SharedLibrary::new(move |name: &std::ffi::CStr| {
		if name.to_bytes() == b"marker" {
			marker as *mut std::ffi::c_void
		} else {
			std::ptr::null_mut()
		}
	});
	assert_eq!(library.find_symbol("marker") as usize, marker);
	assert!(library.find_symbol("other").is_null());
	let clone = library.clone();
	drop(library);
	assert_eq!(clone.find_symbol("marker") as usize, marker);

	let loader =
		slang::SharedLibraryLoader::new(move |path: &str, _: &slang::DefaultLoader<'_>| {
			if path == "mine" {
				Ok(slang::SharedLibrary::new(move |name: &std::ffi::CStr| {
					if name.to_bytes() == b"marker" {
						marker as *mut std::ffi::c_void
					} else {
						std::ptr::null_mut()
					}
				}))
			} else {
				Err(slang::Error::Code(shader_slang_sys::SLANG_E_NOT_FOUND))
			}
		});
	assert_eq!(
		loader.load("mine").unwrap().find_symbol("marker") as usize,
		marker
	);
	assert!(loader.load("theirs").is_err());

	// Raw round trip: the wrapper gives up its reference and takes it back.
	let raw = loader.clone().into_raw();
	let back = unsafe { slang::SharedLibraryLoader::from_raw(raw) }.unwrap();
	assert_eq!(back.as_ptr(), loader.as_ptr());
	assert!(back.cast::<slang::IUnknown>().is_some());
	assert!(back.cast::<slang::Blob>().is_none());
}

/// A session for reflect.slang with the consumer's options: glsl_450, the
/// shader-clock capability, entry points named as in the source.
fn reflect_session(global_session: &slang::GlobalSession) -> slang::Session {
	let session_options = slang::CompilerOptions::default().vulkan_use_entry_point_name(true);
	let target_options = slang::CompilerOptions::default()
		.capability(global_session.find_capability("spvShaderClockKHR"));
	let targets = [slang::TargetDesc::default()
		.format(slang::CompileTarget::Spirv)
		.profile(global_session.find_profile("glsl_450"))
		.options(&target_options)];
	let session_desc = slang::SessionDesc::default()
		.targets(&targets)
		.search_paths(vec![std::path::PathBuf::from("shaders")])
		.options(&session_options);
	global_session.create_session(&session_desc).unwrap()
}

fn linked_probe(session: &slang::Session) -> (slang::ComponentType, slang::Diagnostics) {
	let mut diagnostics = slang::Diagnostics::new();
	let module = session
		.load_module_with_diagnostics("reflect", &mut diagnostics)
		.unwrap();
	// `probe` has no [shader] attribute: looked up by name and stage, as
	// slangc's `-entry probe -stage compute` does.
	let entry = module
		.find_and_check_entry_point_with_diagnostics(
			"probe",
			slang::Stage::Compute,
			&mut diagnostics,
		)
		.unwrap();
	let program = session
		.create_composite_component_type_with_diagnostics(
			&[module.into(), entry.into()],
			&mut diagnostics,
		)
		.unwrap();
	(
		program.link_with_diagnostics(&mut diagnostics).unwrap(),
		diagnostics,
	)
}

/// (c) Reflection as the consumer reads it: bindings (set and index), struct
/// field offsets and the hashed-string table.
#[test]
fn reflection() {
	let global_session = global_session();
	let session = reflect_session(&global_session);
	let (linked, mut diagnostics) = linked_probe(&session);
	let layout = linked.layout_with_diagnostics(0, &mut diagnostics).unwrap();
	let code = linked
		.entry_point_code_with_diagnostics(0, 0, &mut diagnostics)
		.unwrap();
	assert_eq!(words(code.as_slice())[0], SPIRV_MAGIC);
	assert!(
		diagnostics.is_empty(),
		"unexpected diagnostics: {diagnostics}"
	);

	let parameter = |name: &str| {
		layout
			.parameters()
			.find(|p| p.variable().and_then(|v| v.name()) == Some(name))
			.unwrap_or_else(|| panic!("no parameter {name}"))
	};

	// Bindings: (set, index) as `[[vk::binding(index, set)]]` gave them.
	for (name, set, index) in [
		("params", 0, 0),
		("output", 0, 1),
		("layers", 1, 0),
		("linear_sampler", 1, 1),
	] {
		let p = parameter(name);
		assert_eq!(
			(p.binding_space(), p.binding_index()),
			(set, index),
			"{name}"
		);
	}
	assert_eq!(layout.parameter_count(), 4);

	// The texture's shape is a base shape with a flag.
	let shape = parameter("layers")
		.type_layout()
		.unwrap()
		.resource_shape()
		.unwrap();
	assert_eq!(shape, slang::ResourceShape::SlangTexture2dArray);
	assert_eq!(shape.base_shape(), slang::ResourceShape::SlangTexture2d);
	assert!(shape.is_array() && !shape.is_multisample());

	// Struct field offsets (std140 in a constant buffer).
	let element = parameter("params")
		.type_layout()
		.unwrap()
		.element_type_layout()
		.unwrap();
	let offsets: Vec<(String, usize)> = element
		.fields()
		.map(|field| {
			(
				field.variable().unwrap().name().unwrap().to_string(),
				field.offset(slang::ParameterCategory::Uniform),
			)
		})
		.collect();
	assert_eq!(
		offsets,
		[
			("color".to_string(), 0),
			("count".to_string(), 16),
			("offset".to_string(), 24),
			("flags".to_string(), 32),
		]
	);
	assert_eq!(element.size(slang::ParameterCategory::Uniform), 48);

	// The hashed-string table: every getStringHash literal, under the hash
	// the shader computes.
	assert_eq!(layout.hashed_string_count(), 2);
	let mut strings: Vec<&str> = layout.hashed_strings().collect();
	strings.sort();
	assert_eq!(strings, ["hello", "world"]);
	assert_ne!(
		slang::reflection::compute_string_hash("hello"),
		slang::reflection::compute_string_hash("world")
	);
}

/// Modules serialise to IR blobs and load back, giving the same code; the
/// session lists what it loaded.
#[test]
fn ir_blob_round_trip() {
	let global_session = global_session();
	let session = reflect_session(&global_session);
	let (linked, _) = linked_probe(&session);
	let expected = linked.entry_point_code(0, 0).unwrap().as_slice().to_vec();
	let module = session.load_module("reflect").unwrap();
	let ir = module.serialize().unwrap();
	assert!(!ir.as_slice().is_empty());
	assert!(session.loaded_module_count() >= 1);
	assert!(session.loaded_modules().any(|m| m.name() == "reflect"));
	assert!(session.global_session().is_some());

	// Into a fresh session, through bytes as a cache would hold them.
	let stored = ir.as_slice().to_vec();
	let fresh = reflect_session(&global_session);
	let blob = slang::Blob::from_slice(&stored).unwrap();
	let mut diagnostics = slang::Diagnostics::new();
	let module = fresh
		.load_module_from_ir_blob_with_diagnostics(
			"reflect",
			"reflect.slang",
			&blob,
			&mut diagnostics,
		)
		.unwrap();
	let entry = module
		.find_and_check_entry_point("probe", slang::Stage::Compute)
		.unwrap();
	let program = fresh
		.create_composite_component_type(&[module.into(), entry.into()])
		.unwrap();
	let code = program.link().unwrap().entry_point_code(0, 0).unwrap();
	assert_eq!(code.as_slice(), expected.as_slice());
	assert!(diagnostics.is_empty(), "{diagnostics}");
}

/// Warnings reach the diagnostics on success, errors on failure.
#[test]
fn diagnostics_on_success_and_failure() {
	let global_session = global_session();
	let session = reflect_session(&global_session);

	// Without the shader-clock capability, the profile is upgraded with a
	// warning (the case the consumer's zero-warnings rule catches).
	let clock = "[shader(\"compute\")] [numthreads(1,1,1)]\n\
		void main(uint3 id : SV_DispatchThreadID, RWStructuredBuffer<uint> out) {\n\
		\tout[id.x] = uint(clockARB());\n\
		}\n";
	let plain_session = session_without_capability(&global_session);
	let mut diagnostics = slang::Diagnostics::new();
	let module = plain_session
		.load_module_from_source_string_with_diagnostics(
			"clock",
			"clock.slang",
			clock,
			&mut diagnostics,
		)
		.unwrap();
	let entry = module.find_entry_point_by_name("main").unwrap();
	let program = plain_session
		.create_composite_component_type_with_diagnostics(
			&[module.into(), entry.into()],
			&mut diagnostics,
		)
		.unwrap();
	let linked = program.link_with_diagnostics(&mut diagnostics).unwrap();
	linked
		.entry_point_code_with_diagnostics(0, 0, &mut diagnostics)
		.unwrap();
	assert!(
		diagnostics
			.as_str()
			.contains("warning[E41012]: profile implicitly upgraded"),
		"no profile warning in: {diagnostics:?}"
	);

	let mut diagnostics = slang::Diagnostics::new();
	let broken = session.load_module_from_source_string_with_diagnostics(
		"broken",
		"broken.slang",
		"void f() { this is not slang }",
		&mut diagnostics,
	);
	let Err(slang::Error::Blob(blob)) = broken else {
		panic!("a syntax error did not fail with diagnostics");
	};
	assert!(diagnostics.as_str().contains("error"));
	assert!(diagnostics.as_str().contains(blob.as_str().unwrap().trim()));

	// A NUL in a name is an error, not a panic.
	assert!(session.load_module("bad\0name").is_err());
}

fn session_without_capability(global_session: &slang::GlobalSession) -> slang::Session {
	let targets = [slang::TargetDesc::default()
		.format(slang::CompileTarget::Spirv)
		.profile(global_session.find_profile("glsl_450"))];
	let session_desc = slang::SessionDesc::default().targets(&targets);
	global_session.create_session(&session_desc).unwrap()
}

/// (d) The cache directory holds exactly the embedded files, byte for byte.
#[cfg(all(feature = "embed", not(feature = "system")))]
#[test]
fn embedded_cache_matches_payload() {
	use sha2::{Digest, Sha256};

	let _ = global_session();
	let dir = slang::embedded::dir().unwrap();
	let library = slang::library::loaded().unwrap();
	assert_eq!(
		library.path().unwrap(),
		dir.join(slang::embedded::files()[0].name)
	);
	assert!(dir.ends_with(slang::embedded::dir_name()));

	let mut on_disk: Vec<String> = std::fs::read_dir(dir)
		.unwrap()
		.map(|entry| entry.unwrap().file_name().into_string().unwrap())
		.collect();
	on_disk.sort();
	let mut embedded: Vec<String> = slang::embedded::files()
		.iter()
		.map(|f| f.name.to_string())
		.collect();
	embedded.sort();
	assert_eq!(on_disk, embedded);

	for file in slang::embedded::files() {
		let bytes = std::fs::read(dir.join(file.name)).unwrap();
		assert_eq!(bytes.len() as u64, file.size, "{}", file.name);
		let hash: String = Sha256::digest(&bytes)
			.iter()
			.map(|b| format!("{b:02x}"))
			.collect();
		assert_eq!(hash, file.sha256, "{}", file.name);
		assert_eq!(bytes, file.bytes().unwrap(), "{}", file.name);
	}
	assert!(slang::embedded::verify(dir).unwrap());
}

/// Run by scripts/test-no-env.sh only: checks from inside the process that
/// nothing outside the binary supplied Slang.
#[cfg(all(feature = "embed", not(feature = "system")))]
#[test]
fn environment_is_clean() {
	if std::env::var_os("SHADER_SLANG_TEST_NO_ENV").is_none() {
		return;
	}
	for var in [
		"SLANG_DIR",
		"SLANG_LIB_DIR",
		"SLANG_INCLUDE_DIR",
		"VULKAN_SDK",
		"LD_LIBRARY_PATH",
		"DYLD_LIBRARY_PATH",
		"XDG_CACHE_HOME",
	] {
		assert!(std::env::var_os(var).is_none(), "{var} is set");
	}
	let path = std::env::var_os("PATH").unwrap_or_default();
	for dir in std::env::split_paths(&path) {
		assert!(!dir.join("slangc").exists(), "slangc in {}", dir.display());
	}

	let _ = global_session();
	let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
	let library = slang::library::loaded().unwrap().path().unwrap();
	assert!(
		library.starts_with(home.join(".cache").join("shader-slang")),
		"library opened from {}",
		library.display()
	);
	// Every Slang library mapped into the process comes from that cache.
	let exe = std::env::current_exe().unwrap();
	let maps = std::fs::read_to_string("/proc/self/maps").unwrap_or_default();
	for mapped in maps.lines().filter_map(|l| l.split_whitespace().nth(5)) {
		let path = std::path::Path::new(mapped);
		if path == exe || !path.is_absolute() {
			continue;
		}
		let name = path.file_name().unwrap().to_string_lossy();
		if name.contains("slang") {
			assert!(
				path.starts_with(&home),
				"a Slang file from outside the cache is mapped: {mapped}"
			);
		}
	}
}
