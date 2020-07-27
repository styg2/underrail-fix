fn main() {
	build();
}

fn build() {
	cc::Build::new()
		.include("Detours/src/")
		.static_crt(true)
		.flag("/MT")
		.flag("/W4")
		.flag("/WX")
		.flag("/Gy")
		.flag("/Gm-")
		.flag("/Zl")
		.flag("/Od")
		.define("WIN32_LEAN_AND_MEAN", "1")
		.define("_WIN32_WINNT", "0x501")
		.file("Detours/src/detours.cpp")
		.file("Detours/src/modules.cpp")
		.file("Detours/src/disasm.cpp")
		.file("Detours/src/image.cpp")
		.file("Detours/src/creatwth.cpp")
		.compile("detours");
}
