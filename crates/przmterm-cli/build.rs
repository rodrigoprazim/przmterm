//! On Windows, puts the product name and version into the executable's
//! version information (Explorer shows them, and code signing checks them),
//! with the icon.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/icon.ico")
        .set("ProductName", "PrzmTerm")
        .set("FileDescription", "PrzmTerm command line (przm)")
        .set("LegalCopyright", "MIT OR Apache-2.0");
    if let Err(e) = res.compile() {
        println!("cargo:warning=no version information in the executable: {e}");
    }
}
