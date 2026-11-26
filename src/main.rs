fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if matches!(
        std::env::args().nth(1).as_deref(),
        Some("--version" | "-V")
    ) {
        println!("yanxi {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    yanxi_trans_r::gui::run_gui()
}
