//! Embeds the app icon in the Windows executable, so Explorer and the taskbar show it.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "Projects");
        res.set("FileDescription", "Projects");
        res.compile().expect("failed to embed the Windows icon resource");
    }
}
