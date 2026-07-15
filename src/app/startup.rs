use std::{cell::RefCell, rc::Rc};

use gpui::{App, AppContext as _, Bounds, Entity, WindowOptions, point, px, size};
use gpui_component::Root;

use crate::Ashell;
use crate::session::{
    GroupTransfer,
    config::{ConfigStore, Session},
    store::{SessionStore, WindowOwnerId},
};

pub(crate) fn bind_workspace_keys(cx: &mut gpui::App) {
    let config = ConfigStore::load().unwrap_or_else(|_| ConfigStore::in_memory());
    crate::app::keybinding_recorder::bind_workspace_keys_from_config(cx, &config);
}

struct LocalMinutelyRoller {
    dir: std::path::PathBuf,
    prefix: String,
    current_minute: u32,
    file: Option<std::fs::File>,
}

impl LocalMinutelyRoller {
    fn new(dir: std::path::PathBuf, prefix: String) -> Self {
        Self {
            dir,
            prefix,
            current_minute: 60,
            file: None,
        }
    }

    fn rollover(&mut self, now: chrono::DateTime<chrono::Local>) -> std::io::Result<()> {
        use chrono::Timelike;
        let minute = now.minute();
        if self.current_minute != minute || self.file.is_none() {
            let filename = format!("{}-{}.log", self.prefix, now.format("%Y-%m-%d-%H-%M"));
            let path = self.dir.join(filename);
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            self.file = Some(file);
            self.current_minute = minute;

            // Cleanup old files keeping last 6
            if let Ok(entries) = std::fs::read_dir(&self.dir) {
                let mut files: Vec<_> = entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.file_name().to_string_lossy().starts_with(&self.prefix))
                    .collect();
                files.sort_by_key(|e| {
                    e.metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
                });
                if files.len() > 6 {
                    for file in files.iter().take(files.len() - 6) {
                        let _ = std::fs::remove_file(file.path());
                    }
                }
            }
        }
        Ok(())
    }
}

impl std::io::Write for LocalMinutelyRoller {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let now = chrono::Local::now();
        let _ = self.rollover(now);
        if let Some(f) = &mut self.file {
            f.write(buf)
        } else {
            Ok(buf.len())
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(f) = &mut self.file {
            f.flush()
        } else {
            Ok(())
        }
    }
}

pub(crate) fn init_logging() {
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

    let log_dir = directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().join(".config").join("ashell").join("log"))
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    std::fs::create_dir_all(&log_dir).ok();

    let roller = LocalMinutelyRoller::new(log_dir.clone(), "ashell".to_string());

    let (non_blocking, _guard) = tracing_appender::non_blocking(roller);
    // Leak the guard so it lives for the entire duration of the app since GPUI's run might not return
    std::mem::forget(_guard);

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    let stdout_layer = if cfg!(debug_assertions) {
        Some(
            tracing_subscriber::fmt::layer()
                .with_timer(tracing_subscriber::fmt::time::LocalTime::rfc_3339())
                .with_target(true),
        )
    } else {
        None
    };

    let file_layer = tracing_subscriber::fmt::layer()
        .with_timer(tracing_subscriber::fmt::time::LocalTime::rfc_3339())
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer)
        .with(file_layer)
        .init();
}

#[cfg(target_os = "macos")]
pub(crate) fn sync_macos_launch_environment() {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let Ok(output) = std::process::Command::new(&shell)
        .args(["-l", "-c", "env -0"])
        .output()
    else {
        return;
    };
    if !output.status.success() {
        return;
    }

    for entry in output.stdout.split(|b| *b == 0) {
        if entry.is_empty() {
            continue;
        }
        let Some(eq) = entry.iter().position(|b| *b == b'=') else {
            continue;
        };
        let Ok(key) = std::str::from_utf8(&entry[..eq]) else {
            continue;
        };
        let Ok(value) = std::str::from_utf8(&entry[eq + 1..]) else {
            continue;
        };

        let should_import = matches!(
            key,
            "PATH"
                | "MANPATH"
                | "INFOPATH"
                | "LANG"
                | "LC_ALL"
                | "LC_CTYPE"
                | "SHELL"
                | "HOME"
                | "HOMEBREW_PREFIX"
                | "HOMEBREW_CELLAR"
                | "HOMEBREW_REPOSITORY"
                | "HTTP_PROXY"
                | "HTTPS_PROXY"
                | "ALL_PROXY"
                | "http_proxy"
                | "https_proxy"
                | "all_proxy"
        ) || key.starts_with("LC_");

        if should_import {
            unsafe {
                std::env::set_var(key, value);
            }
        }
    }
}

fn read_proxy_from_env() -> Option<(String, String, Option<u16>, String, String)> {
    let vars = ["ALL_PROXY", "all_proxy", "HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"];
    for var in vars {
        if let Ok(val) = std::env::var(var) {
            if val.is_empty() {
                continue;
            }
            if let Ok(url) = reqwest::Url::parse(&val) {
                let scheme = url.scheme();
                let proxy_type = match scheme {
                    "socks5" | "socks5h" => "socks5".to_string(),
                    "http" | "https" => "http".to_string(),
                    _ => "socks5".to_string(),
                };
                let host = url.host_str().unwrap_or("").to_string();
                let port = url.port();
                let user = url.username().to_string();
                let password = url.password().unwrap_or("").to_string();
                return Some((proxy_type, host, port, user, password));
            }
        }
    }
    None
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn sync_macos_launch_environment() {}

pub(crate) fn open_main_window(cx: &mut App) {
    let config = ConfigStore::load().unwrap_or_else(|_| ConfigStore::in_memory());

    let _ = crate::session::config::ENV_PROXY.get_or_init(|| {
        read_proxy_from_env().map(|(proxy_type, host, port, user, password)| {
            tracing::info!(
                "[proxy] Loaded proxy configuration from environment: type={}, host={}, port={:?}, user={}",
                proxy_type,
                host,
                port,
                user
            );
            crate::session::config::EnvProxy {
                proxy_type,
                host,
                port,
                user,
                pass: password,
            }
        })
    });

    let window_options = build_window_options(&config, cx, None);
    let session_store = cx.new(|_| SessionStore::new());
    open_window_with_options(window_options, None, session_store, cx);
}

/// Open a new window, optionally auto-connecting to a session.
pub(crate) fn open_new_window(
    session: Option<Session>,
    session_store: Option<Entity<SessionStore>>,
    cx: &mut App,
) {
    let config = ConfigStore::load().unwrap_or_else(|_| ConfigStore::in_memory());
    // Offset new windows so they don't completely overlap
    let offset = Some((px(40.), px(40.)));
    let window_options = build_window_options(&config, cx, offset);
    let session_store = session_store.unwrap_or_else(|| cx.new(|_| SessionStore::new()));
    open_window_with_options(window_options, session, session_store, cx);
}

fn build_window_options(
    config: &ConfigStore,
    cx: &App,
    offset: Option<(gpui::Pixels, gpui::Pixels)>,
) -> WindowOptions {
    let mut window_options = WindowOptions::default();

    if config.title_bar_style() == crate::session::config::TitleBarStyle::Integrated {
        window_options.titlebar = Some(gpui::TitlebarOptions {
            title: None,
            appears_transparent: true,
            traffic_light_position: Some(gpui::point(px(9.0), px(9.0))),
        });
    }

    #[cfg(not(target_os = "macos"))]
    if let Ok(img) = image::load_from_memory(include_bytes!("../../assets/icons/ashell.png")) {
        window_options.icon = Some(std::sync::Arc::new(img.into_rgba8()));
    }

    if let Some(bounds) = config.window_bounds() {
        window_options.window_bounds = Some(match bounds {
            crate::session::config::SavedWindowBounds::Fullscreen {
                x,
                y,
                width,
                height,
            } => gpui::WindowBounds::Fullscreen(Bounds::new(
                point(px(*x), px(*y)),
                size(px(*width), px(*height)),
            )),
            crate::session::config::SavedWindowBounds::Maximized {
                x,
                y,
                width,
                height,
            } => gpui::WindowBounds::Maximized(Bounds::new(
                point(px(*x), px(*y)),
                size(px(*width), px(*height)),
            )),
            crate::session::config::SavedWindowBounds::Windowed {
                x,
                y,
                width,
                height,
            } => {
                let (mx, my) = offset.unwrap_or((px(0.), px(0.)));
                gpui::WindowBounds::Windowed(Bounds::new(
                    point(px(*x) + mx, px(*y) + my),
                    size(px(*width), px(*height)),
                ))
            }
        });
    } else if let Some(display) = cx.displays().first().cloned() {
        let display_bounds = display.bounds();
        let width = display_bounds.size.width * 0.8;
        let height = display_bounds.size.height * 0.9;

        let x = display_bounds.origin.x + (display_bounds.size.width - width) / 2.0;

        #[cfg(target_os = "macos")]
        let y = display_bounds.origin.y;
        #[cfg(not(target_os = "macos"))]
        let y = display_bounds.origin.y + (display_bounds.size.height - height) / 2.0;

        let (ox, oy) = offset.unwrap_or((px(0.), px(0.)));
        window_options.window_bounds = Some(gpui::WindowBounds::Windowed(Bounds::new(
            point(x + ox, y + oy),
            size(width, height),
        )));
    }

    window_options
}

fn open_window_with_options(
    window_options: WindowOptions,
    session: Option<Session>,
    session_store: Entity<SessionStore>,
    cx: &mut App,
) {
    open_window_with_initializer(
        window_options,
        session_store,
        move |view, cx| {
            if let Some(session) = session {
                view.update(cx, |this, cx| this.open_ssh_session(session, cx));
            }
        },
        cx,
    )
    .expect("failed to open window");
}

pub(crate) fn open_new_window_with_group(
    transfer: GroupTransfer,
    source_owner_id: WindowOwnerId,
    session_store: Entity<SessionStore>,
    cx: &mut App,
) -> Result<(), (String, GroupTransfer)> {
    let config = ConfigStore::load().unwrap_or_else(|_| ConfigStore::in_memory());
    let window_options = build_window_options(&config, cx, Some((px(40.), px(40.))));
    let pending = Rc::new(RefCell::new(Some(transfer)));
    let failure = Rc::new(RefCell::new(None));
    let pending_for_window = pending.clone();
    let failure_for_window = failure.clone();

    let opened = open_window_with_initializer(
        window_options,
        session_store,
        move |view, cx| {
            let Some(transfer) = pending_for_window.borrow_mut().take() else {
                return;
            };
            if let Err((message, transfer)) = view.update(cx, |this, cx| {
                this.receive_group_transfer(transfer, source_owner_id, cx)
            }) {
                *failure_for_window.borrow_mut() = Some(message);
                *pending_for_window.borrow_mut() = Some(transfer);
            }
        },
        cx,
    );

    let message = failure.borrow_mut().take().or_else(|| opened.err());
    let remaining = pending.borrow_mut().take();
    match (message, remaining) {
        (None, None) => Ok(()),
        (Some(message), Some(transfer)) => Err((message, transfer)),
        (None, Some(transfer)) => Err((
            "new window did not accept the transferred tab group".to_string(),
            transfer,
        )),
        (Some(message), None) => {
            tracing::warn!("[ui] window reported an error after accepting transfer: {message}");
            Ok(())
        }
    }
}

fn open_window_with_initializer(
    window_options: WindowOptions,
    session_store: Entity<SessionStore>,
    initialize: impl FnOnce(Entity<Ashell>, &mut App) + 'static,
    cx: &mut App,
) -> Result<(), String> {
    cx.open_window(window_options, |window, cx| {
        window.activate_window();
        window.set_window_title("ashell");
        gpui_component::Theme::sync_system_appearance(Some(window), cx);
        let view = cx.new(|cx| Ashell::new(window, session_store.clone(), cx));

        crate::app::register_window(window.window_handle(), view.clone());
        initialize(view.clone(), cx);

        tracing::info!("[ui] application window opened");
        let focus_handle = view.read(cx).focus_handle.clone();
        window.focus(&focus_handle, cx);

        let view_clone = view.clone();
        window.on_window_should_close(cx, move |window: &mut gpui::Window, cx: &mut gpui::App| {
            let handle = window.window_handle();
            if !cx.windows().contains(&handle) {
                tracing::warn!("[ui] window not found in app during close, skipping save layout state.");
                return true;
            }
            view_clone.update(cx, |this, cx| {
                this.cancel_tab_drag(cx);
                this.save_layout_state(window, cx);
                this.cleanup_on_window_close();
                cx.notify();
            });
            crate::app::deregister_window(handle, cx);
            true
        });

        cx.new(|cx| Root::new(view, window, cx))
    })
    .map(|_| ())
    .map_err(|error| format!("failed to open window: {error:?}"))
}
