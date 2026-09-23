//! Embeds the Windows executable's icon.
//!
//! Everything else about the icon happens at runtime -- the window icon is
//! decoded from `assets/voxview.png` -- but the picture Explorer and the
//! taskbar show for `voxview.exe` has to be a resource inside the PE file,
//! which means a build script and a resource compiler.
//!
//! A missing resource compiler is a warning rather than an error: the binary
//! is perfectly usable without an icon, and failing the build over one would
//! make the project unbuildable on a machine with no Windows SDK.

fn main() {
    println!("cargo:rerun-if-changed=assets/voxview.ico");
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/voxview.ico");
        if let Err(e) = resource.compile() {
            println!("cargo:warning=could not embed the Windows icon: {e}");
        }
    }
}
