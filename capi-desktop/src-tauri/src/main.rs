// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--mcp" || arg == "mcp") {
        capi_desktop_lib::run_stdio_mcp();
        return;
    }

    std::panic::set_hook(Box::new(|info| {
        let msg = format!("Capi v0.1.4 — Error inesperado\n\n{info:?}");
        let _ = writeln!(std::io::stderr(), "{msg}");
        rfd::MessageDialog::new()
            .set_title("Capi - Error inesperado")
            .set_description(&msg)
            .set_level(rfd::MessageLevel::Error)
            .show();
    }));
    capi_desktop_lib::run()
}
