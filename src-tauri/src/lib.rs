use tauri::{AppHandle, Manager, WebviewWindow};

mod platforms;
#[tauri::command]
fn set_wallpaper(app: AppHandle, window_label: String) {
    let w = app.get_webview_window(&window_label).unwrap();
    attach(&w);
}
#[tauri::command]
fn remove_wallpaper(app: AppHandle, window_label: String) {
    let w = app.get_webview_window(&window_label).unwrap();
    detach(&w);
}

pub fn attach(window: &WebviewWindow) {
    #[cfg(target_os = "windows")]
    {
        let hwnd = window.hwnd().unwrap();
        platforms::windows::attach(hwnd);
        return;
    }
}

pub fn detach(window: &WebviewWindow) {
    #[cfg(target_os = "windows")]
    {
        let hwnd = window.hwnd().unwrap();
        platforms::windows::detach(hwnd);
        return;
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![set_wallpaper, remove_wallpaper])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
