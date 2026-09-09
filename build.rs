fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/icon/speckle.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon/speckle.ico");
        res.set("ProductName", "Speckle");
        res.set("FileDescription", "Speckle — local photo and video library");
        res.set("LegalCopyright", "MIT");
        // The icon is cosmetic: a missing resource compiler should warn, not
        // break the build for anyone without the Windows SDK.
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed the icon: {e}");
        }
    }
}
