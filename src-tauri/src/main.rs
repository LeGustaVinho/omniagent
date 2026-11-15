#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[tauri::command]
fn runtime_status() -> &'static str {
    "Desktop runtime is ready."
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![runtime_status])
        .run(tauri::generate_context!())
        .expect("failed to start OmniAgent");
}
