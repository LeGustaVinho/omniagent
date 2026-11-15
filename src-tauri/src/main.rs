#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod fixture;

#[tauri::command]
fn runtime_status() -> &'static str {
    "Desktop runtime is ready."
}

fn main() {
    if let Some(code) = fixture::run_from_args() {
        std::process::exit(code);
    }

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![runtime_status])
        .run(tauri::generate_context!())
        .expect("failed to start OmniAgent");
}
