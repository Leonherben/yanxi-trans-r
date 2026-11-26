#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if matches!(
        std::env::args().nth(1).as_deref(),
        Some("--version" | "-V")
    ) {
        #[cfg(windows)]
        unsafe {
            use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
        println!("yanxi {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    yanxi_trans_r::gui::run_gui()
}
