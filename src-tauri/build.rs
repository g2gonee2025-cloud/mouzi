fn main() {
    let mut attributes = tauri_build::Attributes::new();
    if std::env::var("TARGET").is_ok_and(|target| target.ends_with("-windows-msvc")) {
        // The native picker links Common Controls 6 into the library test binary too.
        // Use the linker for the manifest so both that binary and the app receive it.
        attributes = attributes.windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTUAC:NO");
        println!("cargo:rustc-link-arg=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'");
    }
    tauri_build::try_build(attributes).expect("Tauri build setup failed");
}
