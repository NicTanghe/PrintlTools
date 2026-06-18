#[derive(Debug, Clone)]
pub enum TrayEvent {
    OpenLauncher,
    OpenSettings,
    Exit,
    Error(String),
}

pub fn spawn_events() -> Result<std::sync::mpsc::Receiver<TrayEvent>, String> {
    let (sender, receiver) = std::sync::mpsc::channel();
    platform::spawn(sender)?;
    Ok(receiver)
}

pub fn shutdown() {
    platform::shutdown();
}

fn tray_notification_code(lparam: isize) -> u32 {
    (lparam as usize & 0xffff) as u32
}

#[cfg(windows)]
mod platform {
    use std::mem::size_of;
    use std::sync::{Mutex, OnceLock, mpsc::Sender};
    use std::thread;

    use windows::Win32::Foundation::{
        GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::{
        NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_SETVERSION,
        NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
        DispatchMessageW, GetCursorPos, GetMessageW, HMENU, IDI_APPLICATION, LoadIconW, MF_STRING,
        MSG, PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow, TPM_LEFTALIGN,
        TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP, WM_CLOSE, WM_COMMAND, WM_CREATE,
        WM_DESTROY, WM_LBUTTONUP, WM_RBUTTONUP, WM_USER, WNDCLASSW, WS_EX_TOOLWINDOW,
        WS_OVERLAPPED,
    };
    use windows::core::w;

    use super::{TrayEvent, tray_notification_code};

    const WM_TRAYICON: u32 = WM_USER + 1;
    const WM_TRAY_SHUTDOWN: u32 = WM_APP + 1;
    const TRAY_UID: u32 = 1;
    const MENU_OPEN: usize = 1001;
    const MENU_SETTINGS: usize = 1002;
    const MENU_EXIT: usize = 1003;

    static TRAY_SENDER: OnceLock<Mutex<Sender<TrayEvent>>> = OnceLock::new();
    static TRAY_HWND: OnceLock<Mutex<Option<isize>>> = OnceLock::new();

    pub fn spawn(sender: Sender<TrayEvent>) -> Result<(), String> {
        if TRAY_SENDER.set(Mutex::new(sender)).is_err() {
            return Ok(());
        }

        let _ = TRAY_HWND.set(Mutex::new(None));

        thread::Builder::new()
            .name("printltools-tray".to_string())
            .spawn(|| {
                if let Err(error) = unsafe { run_tray_loop() } {
                    send(TrayEvent::Error(error));
                }
            })
            .map(|_| ())
            .map_err(|error| format!("Failed to start tray thread: {error}"))
    }

    pub fn shutdown() {
        let Some(hwnd) = TRAY_HWND
            .get()
            .and_then(|hwnd| hwnd.lock().ok())
            .and_then(|hwnd| *hwnd)
        else {
            return;
        };
        let hwnd = HWND(hwnd as *mut core::ffi::c_void);

        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_TRAY_SHUTDOWN, WPARAM(0), LPARAM(0));
        }
    }

    unsafe fn run_tray_loop() -> Result<(), String> {
        let module = unsafe { GetModuleHandleW(None) }
            .map_err(|error| format!("GetModuleHandleW failed: {error}"))?;
        let hinstance = HINSTANCE(module.0);
        let class_name = w!("PrintLToolsTrayWindow");

        let window_class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: hinstance,
            lpszClassName: class_name,
            ..Default::default()
        };

        if unsafe { RegisterClassW(&window_class) } == 0 {
            let error = unsafe { GetLastError() };
            if error.0 != 1410 {
                return Err(format!("RegisterClassW failed: {}", error.0));
            }
        }

        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class_name,
                w!("PrintLTools Tray"),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(hinstance),
                None,
            )
        }
        .map_err(|error| format!("CreateWindowExW failed: {error}"))?;

        if let Some(store) = TRAY_HWND.get() {
            if let Ok(mut store) = store.lock() {
                *store = Some(hwnd.0 as isize);
            }
        }

        unsafe { add_icon(hwnd)? };

        let mut message = MSG::default();
        while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }

        unsafe { remove_icon(hwnd) };
        if let Some(store) = TRAY_HWND.get() {
            if let Ok(mut store) = store.lock() {
                *store = None;
            }
        }
        Ok(())
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_CREATE => LRESULT(0),
            WM_TRAYICON => {
                match tray_notification_code(lparam.0) {
                    NIN_SELECT | WM_LBUTTONUP => restore_and_send(TrayEvent::OpenLauncher),
                    WM_RBUTTONUP => unsafe { show_menu(hwnd) },
                    _ => {}
                }

                LRESULT(0)
            }
            WM_COMMAND => {
                match wparam.0 & 0xffff {
                    MENU_OPEN => restore_and_send(TrayEvent::OpenLauncher),
                    MENU_SETTINGS => restore_and_send(TrayEvent::OpenSettings),
                    MENU_EXIT => send(TrayEvent::Exit),
                    _ => {}
                }

                LRESULT(0)
            }
            WM_TRAY_SHUTDOWN => {
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                unsafe { remove_icon(hwnd) };
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        }
    }

    unsafe fn add_icon(hwnd: HWND) -> Result<(), String> {
        let mut data = notify_icon_data(hwnd);
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = WM_TRAYICON;
        data.hIcon = unsafe { LoadIconW(None, IDI_APPLICATION) }
            .map_err(|error| format!("LoadIconW failed: {error}"))?;
        write_wide_fixed(&mut data.szTip, "PrintLTools");

        if !unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
            return Err(format!(
                "Shell_NotifyIconW(NIM_ADD) failed: {}",
                unsafe { GetLastError() }.0
            ));
        }

        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) };

        Ok(())
    }

    unsafe fn remove_icon(hwnd: HWND) {
        let data = notify_icon_data(hwnd);
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
    }

    fn notify_icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_UID,
            ..Default::default()
        }
    }

    unsafe fn show_menu(hwnd: HWND) {
        let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
            return;
        };

        let _ = unsafe { AppendMenuW(menu, MF_STRING, MENU_OPEN, w!("Open launcher")) };
        let _ = unsafe { AppendMenuW(menu, MF_STRING, MENU_SETTINGS, w!("Settings")) };
        let _ = unsafe { AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("Exit")) };

        let mut cursor = POINT::default();
        if unsafe { GetCursorPos(&mut cursor) }.is_ok() {
            let _ = unsafe { SetForegroundWindow(hwnd) };
            let _ = unsafe {
                TrackPopupMenu(
                    menu,
                    TPM_LEFTALIGN | TPM_RIGHTBUTTON,
                    cursor.x,
                    cursor.y,
                    None,
                    hwnd,
                    None,
                )
            };
        }

        unsafe { destroy_menu(menu) };
    }

    unsafe fn destroy_menu(menu: HMENU) {
        let _ = unsafe { DestroyMenu(menu) };
    }

    fn send(event: TrayEvent) {
        if let Some(sender) = TRAY_SENDER.get() {
            if let Ok(sender) = sender.lock() {
                let _ = sender.send(event);
            }
        }
    }

    fn restore_and_send(event: TrayEvent) {
        if let Err(error) = crate::window_control::restore() {
            send(TrayEvent::Error(error));
            return;
        }

        send(event);
    }

    fn write_wide_fixed(buffer: &mut [u16], value: &str) {
        let mut encoded = value.encode_utf16();

        for slot in buffer.iter_mut() {
            *slot = encoded.next().unwrap_or(0);
            if *slot == 0 {
                break;
            }
        }

        if let Some(last) = buffer.last_mut() {
            *last = 0;
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use std::sync::mpsc::Sender;

    use super::TrayEvent;

    pub fn spawn(_sender: Sender<TrayEvent>) -> Result<(), String> {
        Err("Tray integration is only implemented on Windows.".to_string())
    }

    pub fn shutdown() {}
}

#[cfg(test)]
mod tests {
    #[test]
    fn tray_notification_uses_low_word_for_version_four_callbacks() {
        let encoded = ((super::platform_tray_uid_for_test() as isize) << 16) | 0x0400;
        assert_eq!(super::tray_notification_code(encoded), 0x0400);
    }
}

#[cfg(test)]
const fn platform_tray_uid_for_test() -> u32 {
    1
}
