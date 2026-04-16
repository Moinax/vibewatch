pub mod session_row;
pub mod window;

use libadwaita as adw;

use adw::prelude::*;

pub async fn run_panel() -> anyhow::Result<()> {
    let app = adw::Application::builder()
        .application_id("app.vibewatch.panel")
        .build();

    app.connect_activate(|app| {
        window::build_window(app);
    });

    app.run_with_args::<String>(&[]);
    Ok(())
}
