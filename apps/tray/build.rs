use std::{env, error::Error, path::Path};

fn main() -> Result<(), Box<dyn Error>> {
    let icon = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/favicon.ico");
    println!("cargo:rerun-if-changed={}", icon.display());

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let icon = icon.to_str().ok_or("GFlick icon path is not valid UTF-8")?;
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon(icon)
            .set("ProductName", "GFlick")
            .set("FileDescription", "GFlick tray")
            .set("OriginalFilename", "gflick-tray.exe");
        resource.compile()?;
    }
    Ok(())
}
