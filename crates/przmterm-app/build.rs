//! On Windows, puts the product name and version into the executable's
//! version information (Explorer shows them, and code signing checks them),
//! with the icon.
//!
//! On macOS, embeds an Info.plist in the executable, so a binary run
//! outside PrzmTerm.app (`cargo run`) is still called PrzmTerm in the menu
//! bar and the Dock instead of `przmterm`. Inside the app the bundle's own
//! Info.plist (scripts/macos-app.sh) is the one used.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/icon.ico");
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => return embed_info_plist(),
        Ok("windows") => {}
        _ => return,
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/icon.ico")
        .set("ProductName", "PrzmTerm")
        .set("FileDescription", "PrzmTerm terminal")
        .set("LegalCopyright", "MIT OR Apache-2.0");
    if let Err(e) = res.compile() {
        println!("cargo:warning=no version information in the executable: {e}");
    }
}

fn embed_info_plist() {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let short = version.split(['-', '+']).next().unwrap_or("0.0.0");
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDisplayName</key>
    <string>PrzmTerm</string>
    <key>CFBundleIdentifier</key>
    <string>io.github.rodrigoprazim.przmterm</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>PrzmTerm</string>
    <key>CFBundleShortVersionString</key>
    <string>{short}</string>
    <key>CFBundleVersion</key>
    <string>{short}</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSLocalNetworkUsageDescription</key>
    <string>PrzmTerm connects to the hosts you open on your local network over SSH and Telnet.</string>
</dict>
</plist>
"#
    );
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let path = out.join("Info.plist");
    std::fs::write(&path, plist).expect("writing Info.plist");
    // Only the desktop app's binary (not tests or other targets).
    println!(
        "cargo:rustc-link-arg-bin=przmterm=-Wl,-sectcreate,__TEXT,__info_plist,{}",
        path.display()
    );
}
