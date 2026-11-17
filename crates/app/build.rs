//! Windows: embed the app icon (and version info) as resources in the .exe.
//!
//! GPUI's Windows backend loads icon resource ID 1 from the running module
//! for the window class, so this one icon drives the title bar, the
//! taskbar and Alt+Tab, while Explorer and shortcuts read the same resource
//! for the .exe itself. `winresource::set_icon` uses ID 1.
//!
//! The .ico (16 to 256 px) is generated from the macOS iconset in
//! assets/icon/AppIcon.iconset; regenerate it when the icon changes.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icon/AppIcon.ico");

    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon/AppIcon.ico")
            .set("ProductName", "Ora The Explorer")
            .set("FileDescription", "Ora The Explorer")
            .set("LegalCopyright", "Copyright (c) 2026 abu-abbas");
        // Fail loudly: a release without the icon is the bug this fixes.
        res.compile().expect("embed Windows icon/version resources");
    }
}
