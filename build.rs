// On Windows, builds the app icon and the details Explorer shows (name,
// version) into TextEditor.exe. It's the same icon as the macOS app's.
fn main() {
    println!("cargo:rerun-if-changed=packaging/windows/AppIcon.ico");
    // The target's OS, not this build script's: it also runs when building
    // for Windows on a Mac.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("packaging/windows/AppIcon.ico")
        .set("ProductName", "TextEditor")
        .set("FileDescription", "TextEditor")
        .set("InternalName", "TextEditor")
        .set("OriginalFilename", "TextEditor.exe");
    res.compile().expect("could not build the Windows resources");
}
