mod app;

use anyhow::Result;
use app::LcdGuiApp;
use eframe::egui::Vec2;

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let session_type = std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "desktop".to_string());
    tracing::info!("Starting X870E Extreme LCD Tool (session: {})", session_type);

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("X870E Extreme LCD Tool")
            .with_inner_size(Vec2::new(980.0, 680.0))
            .with_min_inner_size(Vec2::new(740.0, 520.0))
            .with_drag_and_drop(true),
        ..Default::default()
    };

    eframe::run_native(
        "X870E Extreme LCD Tool",
        options,
        Box::new(|cc| Ok(Box::new(LcdGuiApp::new(cc)))),
    )
    .map_err(|e| anyhow::anyhow!("GUI runtime error: {}", e))
}
