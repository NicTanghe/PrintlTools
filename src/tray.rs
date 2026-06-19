#[derive(Debug, Clone)]
pub enum TrayEvent {
    HideLauncher,
    OpenLauncher,
    OpenSettings,
    Error(String),
}

pub fn spawn_events() -> Result<std::sync::mpsc::Receiver<TrayEvent>, String> {
    let (sender, receiver) = std::sync::mpsc::channel();
    platform::spawn(sender)?;
    Ok(receiver)
}

fn tray_notification_code(lparam: isize) -> u32 {
    (lparam as usize & 0xffff) as u32
}

const DUPLICATE_LEFT_CLICK_WINDOW_MS: u32 = 250;

#[derive(Debug, Default)]
struct LeftClickFilter {
    last_handled: Option<(u32, u32)>,
}

impl LeftClickFilter {
    fn should_handle(&mut self, code: u32, message_time: u32) -> bool {
        if let Some((last_code, last_time)) = self.last_handled
            && code != last_code
            && message_time.wrapping_sub(last_time) <= DUPLICATE_LEFT_CLICK_WINDOW_MS
        {
            return false;
        }

        self.last_handled = Some((code, message_time));
        true
    }
}

#[cfg(windows)]
mod platform {
    use std::cell::RefCell;
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
        DispatchMessageW, GetCursorPos, GetMessageTime, GetMessageW, HMENU, MF_STRING, MSG,
        PostQuitMessage, RegisterClassW, SetForegroundWindow, TPM_LEFTALIGN, TPM_RIGHTBUTTON,
        TrackPopupMenu, TranslateMessage, WM_COMMAND, WM_CREATE, WM_DESTROY, WM_LBUTTONUP,
        WM_RBUTTONUP, WM_USER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_OVERLAPPED,
    };
    use windows::core::w;

    use super::{LeftClickFilter, TrayEvent, tray_notification_code};

    const WM_TRAYICON: u32 = WM_USER + 1;
    const TRAY_UID: u32 = 1;
    const MENU_OPEN: usize = 1001;
    const MENU_SETTINGS: usize = 1002;
    const MENU_EXIT: usize = 1003;

    static TRAY_SENDER: OnceLock<Mutex<Sender<TrayEvent>>> = OnceLock::new();

    thread_local! {
        static LEFT_CLICK_FILTER: RefCell<LeftClickFilter> =
            RefCell::new(LeftClickFilter::default());
    }

    pub fn spawn(sender: Sender<TrayEvent>) -> Result<(), String> {
        if TRAY_SENDER.set(Mutex::new(sender)).is_err() {
            return Ok(());
        }

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

        unsafe { add_icon(hwnd)? };

        let mut message = MSG::default();
        while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }

        unsafe { remove_icon(hwnd) };
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
                let notification = tray_notification_code(lparam.0);
                match notification {
                    NIN_SELECT | WM_LBUTTONUP if should_handle_left_click(notification) => {
                        toggle_and_send();
                    }
                    WM_RBUTTONUP => unsafe { show_menu(hwnd) },
                    _ => {}
                }

                LRESULT(0)
            }
            WM_COMMAND => {
                match wparam.0 & 0xffff {
                    MENU_OPEN => restore_and_send(TrayEvent::OpenLauncher),
                    MENU_SETTINGS => restore_and_send(TrayEvent::OpenSettings),
                    MENU_EXIT => exit_process(hwnd),
                    _ => {}
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
        data.hIcon = crate::windows_icon::load_tray()?;
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

    fn exit_process(hwnd: HWND) -> ! {
        unsafe { remove_icon(hwnd) };
        std::process::exit(0);
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
        crate::app::request_full_window_repaint();
        if let Err(error) = crate::window_control::restore() {
            send(TrayEvent::Error(error));
            return;
        }

        send(event);
    }

    fn should_handle_left_click(code: u32) -> bool {
        let message_time = unsafe { GetMessageTime() } as u32;
        LEFT_CLICK_FILTER.with(|filter| filter.borrow_mut().should_handle(code, message_time))
    }

    fn toggle_and_send() {
        match crate::window_control::is_visible() {
            Ok(true) => send(TrayEvent::HideLauncher),
            Ok(false) => restore_and_send(TrayEvent::OpenLauncher),
            Err(error) => send(TrayEvent::Error(error)),
        }
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
}

#[cfg(test)]
mod tests {
    use super::LeftClickFilter;

    #[test]
    fn tray_notification_uses_low_word_for_version_four_callbacks() {
        let encoded = ((super::platform_tray_uid_for_test() as isize) << 16) | 0x0400;
        assert_eq!(super::tray_notification_code(encoded), 0x0400);
    }

    #[test]
    fn paired_left_click_notifications_are_handled_once() {
        let mut filter = LeftClickFilter::default();

        assert!(filter.should_handle(0x0400, 1_000));
        assert!(!filter.should_handle(0x0202, 1_001));
    }

    #[test]
    fn repeated_clicks_of_the_same_notification_type_are_preserved() {
        let mut filter = LeftClickFilter::default();

        assert!(filter.should_handle(0x0202, 1_000));
        assert!(filter.should_handle(0x0202, 1_050));
    }

    #[test]
    fn later_notification_of_a_different_type_is_preserved() {
        let mut filter = LeftClickFilter::default();

        assert!(filter.should_handle(0x0400, 1_000));
        assert!(filter.should_handle(0x0202, 1_000 + super::DUPLICATE_LEFT_CLICK_WINDOW_MS + 1));
    }
}

#[cfg(test)]
const fn platform_tray_uid_for_test() -> u32 {
    1
}
