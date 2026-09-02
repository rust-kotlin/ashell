#[cfg(any(target_os = "windows", test))]
use std::sync::mpsc::{SyncSender, TrySendError};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::{collections::VecDeque, sync::Mutex};
#[cfg(target_os = "windows")]
use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

#[cfg(target_os = "macos")]
const APP_BUNDLE_ID: &str = "dev.ashell.app";

#[cfg(target_os = "windows")]
const APP_USER_MODEL_ID: &str = "dev.ashell.app";

#[cfg(any(target_os = "macos", target_os = "windows"))]
const MAX_PENDING_TERMINAL_NOTIFICATION_ACTIVATIONS: usize = 64;
#[cfg(any(target_os = "macos", target_os = "windows"))]
static TERMINAL_NOTIFICATION_ACTIVATIONS: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
#[cfg(target_os = "windows")]
static WINDOWS_NOTIFICATION_CLEAR_SENDER: OnceLock<SyncSender<()>> = OnceLock::new();
#[cfg(target_os = "windows")]
const SLOW_WINDOWS_NOTIFICATION_CLEAR_THRESHOLD: Duration = Duration::from_millis(100);

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

    #[cfg(target_os = "windows")]
    initialize_windows_notification_clear_worker();
}

#[cfg(any(target_os = "windows", test))]
fn enqueue_coalesced_signal(sender: &SyncSender<()>) -> bool {
    match sender.try_send(()) {
        Ok(()) | Err(TrySendError::Full(())) => true,
        Err(TrySendError::Disconnected(())) => false,
    }
}

#[cfg(target_os = "windows")]
fn initialize_windows_notification_clear_worker() {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    if WINDOWS_NOTIFICATION_CLEAR_SENDER.set(sender).is_err() {
        return;
    }

    if let Err(error) = std::thread::Builder::new()
        .name("windows-notification-clear".to_string())
        .spawn(move || {
            use windows::Win32::System::WinRT::{
                RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize,
            };

            if let Err(error) = unsafe { RoInitialize(RO_INIT_MULTITHREADED) } {
                tracing::warn!("failed to initialize WinRT notification worker: {error}");
                return;
            }

            while receiver.recv().is_ok() {
                let started_at = Instant::now();
                clear_windows_notification_history();
                let elapsed = started_at.elapsed();
                if elapsed >= SLOW_WINDOWS_NOTIFICATION_CLEAR_THRESHOLD {
                    tracing::warn!(
                        elapsed_ms = elapsed.as_millis(),
                        "Windows notification history clear exceeded the latency threshold"
                    );
                }
            }

            unsafe { RoUninitialize() };
        })
    {
        tracing::warn!("failed to start Windows notification clear worker: {error}");
    }
}

#[cfg(target_os = "windows")]
#[allow(deprecated)]
fn clear_windows_notification_history() {
    use windows::UI::Notifications::ToastNotificationManager;

    if let Err(error) = ToastNotificationManager::History().and_then(|history| history.Clear()) {
        tracing::warn!("failed to clear Windows notification history: {error}");
    }
}

pub(crate) fn show_terminal_notification(tab_id: String, title: String, body: String) {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    if let Err(error) = std::thread::Builder::new()
        .name("desktop-notification".to_string())
        .spawn(move || show_native_notification(&title, &body, &tab_id))
    {
        tracing::warn!("failed to start desktop notification thread: {error}");
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (tab_id, title, body);
    }
}

#[cfg(target_os = "macos")]
fn show_native_notification(title: &str, body: &str, tab_id: &str) {
    let result = mac_notification_sys::Notification::new()
        .title(title)
        .message(body)
        .wait_for_click(true)
        .send();
    match result {
        Ok(mac_notification_sys::NotificationResponse::Click) => {
            queue_terminal_notification_activation(tab_id.to_string());
        }
        Ok(_) => tracing::info!("terminal notification closed without activation on macOS"),
        Err(error) => tracing::warn!("failed to show macOS terminal notification: {error}"),
    }
}

#[cfg(target_os = "windows")]
fn show_native_notification(title: &str, body: &str, tab_id: &str) {
    match show_windows_notification(APP_USER_MODEL_ID, title, body, tab_id) {
        Ok(()) => tracing::info!("submitted terminal notification to Windows"),
        Err(primary_error) => {
            if let Err(fallback_error) = show_windows_notification(
                winrt_notification::Toast::POWERSHELL_APP_ID,
                title,
                body,
                tab_id,
            ) {
                tracing::warn!(
                    "failed to show Windows terminal notification: {primary_error}; fallback failed: {fallback_error}"
                );
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn show_windows_notification(
    app_id: &str,
    title: &str,
    body: &str,
    tab_id: &str,
) -> winrt_notification::Result<()> {
    let activation_tab_id = tab_id.to_string();
    winrt_notification::Toast::new(app_id)
        .title(title)
        .text2(body)
        .duration(winrt_notification::Duration::Short)
        .sound(None)
        .on_activated(move |_| {
            queue_terminal_notification_activation(activation_tab_id.clone());
            Ok(())
        })
        .show()
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn queue_terminal_notification_activation(tab_id: String) {
    let Ok(mut activations) = TERMINAL_NOTIFICATION_ACTIVATIONS.lock() else {
        tracing::warn!("failed to lock terminal notification activation queue");
        return;
    };
    if activations.len() >= MAX_PENDING_TERMINAL_NOTIFICATION_ACTIVATIONS {
        activations.pop_front();
    }
    activations.push_back(tab_id);
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(crate) fn take_terminal_notification_activation() -> Option<String> {
    let Ok(mut activations) = TERMINAL_NOTIFICATION_ACTIVATIONS.lock() else {
        tracing::warn!("failed to lock terminal notification activation queue");
        return None;
    };
    activations.pop_front()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) fn take_terminal_notification_activation() -> Option<String> {
    None
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

#[allow(deprecated)]
/// Clears notifications delivered by the current application from Notification Center.
pub(crate) fn clear_current_app_delivered_notifications() {
    #[cfg(target_os = "macos")]
    {
        use objc2_foundation::NSUserNotificationCenter;

        // The default center is scoped to this application, so other apps' notifications remain untouched.
        NSUserNotificationCenter::defaultUserNotificationCenter().removeAllDeliveredNotifications();
    }

    #[cfg(target_os = "windows")]
    {
        let Some(sender) = WINDOWS_NOTIFICATION_CLEAR_SENDER.get() else {
            tracing::warn!("Windows notification clear worker is unavailable");
            return;
        };
        if !enqueue_coalesced_signal(sender) {
            tracing::warn!("Windows notification clear worker disconnected");
        }
    }
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

    let hwnd = HWND(window_handle as *mut std::ffi::c_void);
    if !unread {
        return unsafe {
            taskbar.SetOverlayIcon(hwnd, HICON(std::ptr::null_mut()), PCWSTR::null())
        };
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

#[cfg(test)]
mod tests {
    use super::enqueue_coalesced_signal;

    #[test]
    fn coalesces_pending_non_blocking_worker_signals() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);

        assert!(enqueue_coalesced_signal(&sender));
        assert!(enqueue_coalesced_signal(&sender));
        assert_eq!(receiver.try_iter().count(), 1);

        drop(receiver);
        assert!(!enqueue_coalesced_signal(&sender));
    }
}
