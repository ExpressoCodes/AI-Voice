mod app_state;
mod audio;
mod claude;
mod config;
mod error;
mod stt;
mod tts;
mod ui;

use gtk4::gio::prelude::{ApplicationExt, ApplicationExtManual};
use gtk4::prelude::GtkWindowExt;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let app = libadwaita::Application::builder()
        .application_id("com.example.voicechat")
        .build();

    app.connect_activate(|app| {
        let win = ui::window::build_window(app);
        win.present();
    });

    app.run();
}
