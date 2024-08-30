pub fn attach(window: &WebviewWindow) {
    if let Ok(hwnd) = window.gtk_window() {
        let gtk_window_id = hwnd.as_ref().unwrap().as_ptr() as u64;
        window.set_fullscreen(true).unwrap();

        let output = Command::new("sh")
            .arg("-c")
            .arg(format!("wmctrl -r :ACTIVE: -b add,below,sticky,skip_taskbar,skip_pager; xdotool windowunmap {gtk_window_id}; xdotool windowmap {gtk_window_id}"))
            .output()
            .expect("Failed to execute command");

        if !output.status.success() {
            eprintln!(
                "Failed to set wallpaper: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

pub fn detach(window: &WebviewWindow) {
    if let Ok(hwnd) = window.gtk_window() {
        let gtk_window_id = hwnd.as_ref().unwrap().as_ptr() as u64;
        window.set_fullscreen(true).unwrap();

        let output = Command::new("sh")
            .arg("-c")
            .arg(format!("wmctrl -r :ACTIVE: -b add,below,sticky,skip_taskbar,skip_pager; xdotool windowunmap {gtk_window_id}; xdotool windowmap {gtk_window_id}"))
            .output()
            .expect("Failed to execute command");

        if !output.status.success() {
            eprintln!(
                "Failed to set wallpaper: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
