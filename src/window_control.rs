const WINDOW_TITLE: &str = "PrintLTools";
const WINDOW_MARGIN: i32 = 12;

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
    let window = find_window()?;
    unsafe { restore_window(window)? };
    Ok(())
}

#[cfg(windows)]
pub fn is_visible() -> Result<bool, String> {
    use windows::Win32::UI::WindowsAndMessaging::IsWindowVisible;
    let window = find_window()?;
    Ok(unsafe { IsWindowVisible(window) }.as_bool())
}

#[cfg(windows)]
unsafe fn restore_window(window: windows::Win32::Foundation::HWND) -> Result<(), String> {
    use windows::Win32::Graphics::Gdi::{RDW_INTERNALPAINT, RedrawWindow};
    use windows::Win32::UI::WindowsAndMessaging::{
        SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
    };

    unsafe {
        let _ = ShowWindow(window, SW_SHOW);
        let _ = ShowWindow(window, SW_RESTORE);
        position_window_bottom_right(window)?;
        let _ = SetForegroundWindow(window);
        let _ = RedrawWindow(Some(window), None, None, RDW_INTERNALPAINT);
    }
    Ok(())
}

#[cfg(windows)]
pub fn position_bottom_right() -> Result<(), String> {
    let window = find_window()?;
    unsafe { position_window_bottom_right(window) }
}

#[cfg(windows)]
unsafe fn position_window_bottom_right(
    window: windows::Win32::Foundation::HWND,
) -> Result<(), String> {
    use std::mem::size_of;

    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowRect, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos,
    };

    let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };
    let mut monitor_info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut monitor_info) }.as_bool() {
        return Err("Could not read the monitor work area.".to_string());
    }

    let mut window_rect = RECT::default();
    unsafe { GetWindowRect(window, &mut window_rect) }
        .map_err(|error| format!("Could not read the window bounds: {error}"))?;

    let (x, y) = bottom_right_position(monitor_info.rcWork, window_rect, WINDOW_MARGIN);
    unsafe {
        SetWindowPos(
            window,
            None,
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    }
    .map_err(|error| format!("Could not position the window: {error}"))
}

#[cfg(windows)]
fn bottom_right_position(
    work_area: windows::Win32::Foundation::RECT,
    window: windows::Win32::Foundation::RECT,
    margin: i32,
) -> (i32, i32) {
    let width = window.right.saturating_sub(window.left);
    let height = window.bottom.saturating_sub(window.top);
    let x = work_area
        .right
        .saturating_sub(width)
        .saturating_sub(margin)
        .max(work_area.left);
    let y = work_area
        .bottom
        .saturating_sub(height)
        .saturating_sub(margin)
        .max(work_area.top);
    (x, y)
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

#[cfg(not(windows))]
pub fn is_visible() -> Result<bool, String> {
    Err("Tray window visibility is only implemented on Windows.".to_string())
}

#[cfg(not(windows))]
pub fn position_bottom_right() -> Result<(), String> {
    Err("Automatic window positioning is only implemented on Windows.".to_string())
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    #[test]
    fn bottom_right_position_uses_work_area_and_margin() {
        use windows::Win32::Foundation::RECT;

        let work_area = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        let window = RECT {
            left: 100,
            top: 100,
            right: 620,
            bottom: 780,
        };

        assert_eq!(
            super::bottom_right_position(work_area, window, 12),
            (1388, 348)
        );
    }

    #[cfg(windows)]
    #[test]
    fn bottom_right_position_stays_inside_small_work_area() {
        use windows::Win32::Foundation::RECT;

        let work_area = RECT {
            left: -1000,
            top: 0,
            right: 0,
            bottom: 600,
        };
        let window = RECT {
            left: 0,
            top: 0,
            right: 1200,
            bottom: 700,
        };

        assert_eq!(
            super::bottom_right_position(work_area, window, 12),
            (-1000, 0)
        );
    }
}
