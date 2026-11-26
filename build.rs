use std::env;
use std::io;

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if env::var_os("CARGO_CFG_WINDOWS").is_some() {
        winresource::WindowsResource::new()
            .set_icon("assets/icon.ico")
            .compile()?;
    }
    Ok(())
}
