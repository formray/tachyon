//! Embeds the icon and Windows version info (needs MinGW `windres`; optional).
fn main() {
    println!("cargo:rerun-if-changed=res/app.rc");
    println!("cargo:rerun-if-changed=res/tachyon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("app_res.o");
    let windres = std::env::var("WINDRES").unwrap_or_else(|_| "x86_64-w64-mingw32-windres".into());
    // Without windres (e.g. an MSVC-only setup) the exe simply has no embedded icon.
    match std::process::Command::new(&windres)
        .args(["--input-format=rc", "-O", "coff", "-I", "res", "-i", "res/app.rc", "-o"])
        .arg(&out)
        .status()
    {
        Ok(st) if st.success() => println!("cargo:rustc-link-arg-bins={}", out.display()),
        _ => println!("cargo:warning=windres not found or failed; building without icon/version info"),
    }
}
