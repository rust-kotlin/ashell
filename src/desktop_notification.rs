#[cfg(target_os = "macos")]
const APP_BUNDLE_ID: &str = "dev.ashell.app";

#[cfg(target_os = "windows")]
const APP_USER_MODEL_ID: &str = "dev.ashell.app";

pub(crate) fn initialize() {
    #[cfg(target_os = "macos")]
    match desktop_notify::set_application(APP_BUNDLE_ID) {
        Ok(()) => {
            tracing::info!("registered the macOS notification application identity {APP_BUNDLE_ID}")
        }
        Err(error) => {
            tracing::warn!(
                "failed to register the macOS notification application identity {APP_BUNDLE_ID}: {error}"
            );
        }
    }

    #[cfg(target_os = "windows")]
    if let Err(error) = unsafe {
        windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID(windows::core::w!(
            "dev.ashell.app"
        ))
    } {
        tracing::warn!("failed to set Windows application identity: {error}");
    }
}

pub(crate) fn show_terminal_notification(title: String, body: String) {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    if let Err(error) = std::thread::Builder::new()
        .name("desktop-notification".to_string())
        .spawn(move || show_native_notification(&title, &body))
    {
        tracing::warn!("failed to start desktop notification thread: {error}");
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (title, body);
    }
}

#[cfg(target_os = "macos")]
fn show_native_notification(title: &str, body: &str) {
    let result = desktop_notify::Notification::new()
        .summary(title)
        .body(body)
        .show();
    match result {
        Ok(_) => tracing::info!("submitted terminal notification to macOS"),
        Err(error) => tracing::warn!("failed to show macOS terminal notification: {error}"),
    }
}

#[cfg(target_os = "windows")]
fn show_native_notification(title: &str, body: &str) {
    let result = desktop_notify::Notification::new()
        .appname("ashell")
        .summary(title)
        .body(body)
        .app_id(APP_USER_MODEL_ID)
        .show();
    if result.is_ok() {
        return;
    }

    if let Err(error) = desktop_notify::Notification::new()
        .appname("ashell")
        .summary(title)
        .body(body)
        .show()
    {
        tracing::warn!("failed to show Windows terminal notification: {error}");
    }
}

pub(crate) fn native_window_handle(window: &gpui::Window) -> Option<isize> {
    #[cfg(target_os = "windows")]
    {
        use raw_window_handle::RawWindowHandle;

        let handle = raw_window_handle::HasWindowHandle::window_handle(window).ok()?;
        return match handle.as_raw() {
            RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
            _ => None,
        };
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = window;
        None
    }
}

pub(crate) fn set_unread_indicator(unread: bool, window_handle: Option<isize>) {
    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use objc2_app_kit::NSApplication;
        use objc2_foundation::NSString;

        let _ = window_handle;
        let Some(main_thread) = MainThreadMarker::new() else {
            tracing::warn!("attempted to update the Dock badge outside the main thread");
            return;
        };
        let application = NSApplication::sharedApplication(main_thread);
        let dock_tile = application.dockTile();
        let label = unread.then(|| NSString::from_str("\u{2022}"));
        dock_tile.setBadgeLabel(label.as_deref());
    }

    #[cfg(target_os = "windows")]
    if let Some(window_handle) = window_handle {
        if let Err(error) = set_windows_taskbar_badge(window_handle, unread) {
            tracing::warn!("failed to update Windows taskbar badge: {error}");
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = (unread, window_handle);
}

pub(crate) fn clear_unread_indicator(window_handle: Option<isize>) {
    set_unread_indicator(false, window_handle);
}

#[cfg(target_os = "windows")]
fn set_windows_taskbar_badge(window_handle: isize, unread: bool) -> windows::core::Result<()> {
    use windows::{
        Win32::{
            Foundation::HWND,
            System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
            UI::{
                Shell::{ITaskbarList3, TaskbarList},
                WindowsAndMessaging::{
                    CreateIconFromResourceEx, DestroyIcon, HICON, LR_DEFAULTCOLOR,
                },
            },
        },
        core::{IUnknown, PCWSTR, w},
    };

    let taskbar: ITaskbarList3 =
        unsafe { CoCreateInstance(&TaskbarList, None::<&IUnknown>, CLSCTX_INPROC_SERVER)? };
    unsafe { taskbar.HrInit()? };

    let hwnd = HWND(window_handle);
    if !unread {
        return unsafe { taskbar.SetOverlayIcon(hwnd, HICON(0), PCWSTR::null()) };
    }

    let icon_resource = windows_badge_icon_resource();
    let icon = unsafe {
        CreateIconFromResourceEx(&icon_resource, true, 0x0003_0000, 16, 16, LR_DEFAULTCOLOR)?
    };
    let result = unsafe { taskbar.SetOverlayIcon(hwnd, icon, w!("Unread terminal notification")) };
    let destroy_result = unsafe { DestroyIcon(icon) };
    result?;
    destroy_result
}

#[cfg(target_os = "windows")]
fn windows_badge_icon_resource() -> Vec<u8> {
    const WIDTH: usize = 16;
    const HEIGHT: usize = 16;
    const BITMAP_HEADER_SIZE: u32 = 40;
    const XOR_BYTES: usize = WIDTH * HEIGHT * 4;
    const AND_ROW_BYTES: usize = 4;

    let mut resource = Vec::with_capacity(BITMAP_HEADER_SIZE as usize + XOR_BYTES + 64);
    resource.extend_from_slice(&BITMAP_HEADER_SIZE.to_le_bytes());
    resource.extend_from_slice(&(WIDTH as i32).to_le_bytes());
    resource.extend_from_slice(&((HEIGHT * 2) as i32).to_le_bytes());
    resource.extend_from_slice(&1u16.to_le_bytes());
    resource.extend_from_slice(&32u16.to_le_bytes());
    resource.extend_from_slice(&0u32.to_le_bytes());
    resource.extend_from_slice(&(XOR_BYTES as u32).to_le_bytes());
    resource.extend_from_slice(&0i32.to_le_bytes());
    resource.extend_from_slice(&0i32.to_le_bytes());
    resource.extend_from_slice(&0u32.to_le_bytes());
    resource.extend_from_slice(&0u32.to_le_bytes());

    for y in (0..HEIGHT).rev() {
        for x in 0..WIDTH {
            if windows_badge_pixel_is_red(x, y) {
                resource.extend_from_slice(&[42, 48, 239, 255]);
            } else {
                resource.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }

    for y in (0..HEIGHT).rev() {
        let mut row = [0u8; AND_ROW_BYTES];
        for x in 0..WIDTH {
            if !windows_badge_pixel_is_red(x, y) {
                row[x / 8] |= 0x80 >> (x % 8);
            }
        }
        resource.extend_from_slice(&row);
    }
    resource
}

#[cfg(target_os = "windows")]
fn windows_badge_pixel_is_red(x: usize, y: usize) -> bool {
    let dx = x as f32 - 7.5;
    let dy = y as f32 - 7.5;
    dx * dx + dy * dy <= 42.25
}
