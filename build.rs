//! On Windows, the logo is the executables' icon: Explorer and the taskbar
//! show it, and GPUI gives its windows icon resource 1, which `set_icon`
//! fills. Other platforms take the icon from their package (`packaging/`).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // The text `rust_i18n::i18n!` reads while compiling, which the macro
    // does not tell Cargo about.
    println!("cargo:rerun-if-changed=locales");
    println!("cargo:rerun-if-changed=assets/logo/shellrs.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon("assets/logo/shellrs.ico")
            .set("ProductName", "ShellRS")
            .set("FileDescription", "ShellRS");
        if let Err(error) = resource.compile() {
            panic!("cannot embed the icon: {error}");
        }
    }
}
