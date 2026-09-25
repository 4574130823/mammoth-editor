//! Embeds the generated Mammoth icon into the Windows executable.

#[path = "src/logo.rs"]
mod logo;

fn main() {
    println!("cargo:rerun-if-changed=src/logo.rs");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("mammoth.ico");
    std::fs::write(&out, logo::ico(&[16, 20, 24, 32, 40, 48, 64, 256])).unwrap();
    let mut res = winresource::WindowsResource::new();
    res.set_icon(out.to_str().unwrap());
    res.set("ProductName", "Mammoth");
    res.set("FileDescription", "Mammoth text editor");
    if let Err(e) = res.compile() {
        println!("cargo:warning=could not embed the icon: {e}");
    }
}
