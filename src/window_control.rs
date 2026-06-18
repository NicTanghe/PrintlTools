const WINDOW_TITLE: &str = "PrintLTools";

#[cfg(windows)]
pub fn hide() -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};

    let window = find_window()?;
    unsafe {
        let _ = ShowWindow(window, SW_HIDE);
    }
    Ok(())
}

#[cfg(windows)]
pub fn restore() -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::{
        SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
    };

    let window = find_window()?;
    unsafe {
        let _ = ShowWindow(window, SW_SHOW);
        let _ = ShowWindow(window, SW_RESTORE);
        let _ = SetForegroundWindow(window);
    }
    Ok(())
}

#[cfg(windows)]
fn find_window() -> Result<windows::Win32::Foundation::HWND, String> {
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
    use windows::core::{HSTRING, PCWSTR};

    let title = HSTRING::from(WINDOW_TITLE);
    unsafe { FindWindowW(PCWSTR::null(), &title) }
        .map_err(|error| format!("Could not find the {WINDOW_TITLE} window: {error}"))
}

#[cfg(not(windows))]
pub fn hide() -> Result<(), String> {
    Err("Minimize to tray is only implemented on Windows.".to_string())
}

#[cfg(not(windows))]
pub fn restore() -> Result<(), String> {
    Err("Tray window restore is only implemented on Windows.".to_string())
}
