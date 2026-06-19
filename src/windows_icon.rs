#[cfg(windows)]
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, WPARAM};
#[cfg(windows)]
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    HICON, ICON_BIG, ICON_SMALL, LoadIconW, SendMessageW, WM_SETICON,
};
#[cfg(windows)]
use windows::core::PCWSTR;

#[cfg(windows)]
const APP_ICON_RESOURCE_ID: u16 = 1;
#[cfg(windows)]
const TRAY_ICON_RESOURCE_ID: u16 = 2;

#[cfg(windows)]
pub fn load() -> Result<HICON, String> {
    load_resource(APP_ICON_RESOURCE_ID)
}

#[cfg(windows)]
pub fn load_tray() -> Result<HICON, String> {
    load_resource(TRAY_ICON_RESOURCE_ID)
}

#[cfg(windows)]
fn load_resource(resource_id: u16) -> Result<HICON, String> {
    let module = unsafe { GetModuleHandleW(None) }
        .map_err(|error| format!("GetModuleHandleW failed: {error}"))?;
    let resource_name = PCWSTR(resource_id as usize as *const u16);

    unsafe { LoadIconW(Some(HINSTANCE(module.0)), resource_name) }
        .map_err(|error| format!("LoadIconW failed: {error}"))
}

#[cfg(windows)]
pub fn apply_to_window(window: HWND) -> Result<(), String> {
    let icon = load()?;
    let icon_parameter = LPARAM(icon.0 as isize);

    unsafe {
        SendMessageW(
            window,
            WM_SETICON,
            Some(WPARAM(ICON_BIG as usize)),
            Some(icon_parameter),
        );
        SendMessageW(
            window,
            WM_SETICON,
            Some(WPARAM(ICON_SMALL as usize)),
            Some(icon_parameter),
        );
    }

    Ok(())
}
