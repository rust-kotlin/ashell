use gpui::{App, AppContext as _, Bounds, WindowOptions, point, px, size};
use gpui_component::Root;

use crate::Ashell;
use crate::session::config::{ConfigStore, SavedWindowBounds};

fn clamp_saved_window_bounds(
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    cx: &App,
) -> Bounds<gpui::Pixels> {
    let displays = cx.displays();
    let Some(display) = displays.iter().max_by(|left, right| {
        let intersection_area = |display: &std::rc::Rc<dyn gpui::PlatformDisplay>| {
            let visible = display.visible_bounds();
            let left_edge = x.max(visible.origin.x.as_f32());
            let top_edge = y.max(visible.origin.y.as_f32());
            let right_edge =
                (x + width).min(visible.origin.x.as_f32() + visible.size.width.as_f32());
            let bottom_edge =
                (y + height).min(visible.origin.y.as_f32() + visible.size.height.as_f32());
            (right_edge - left_edge).max(0.0) * (bottom_edge - top_edge).max(0.0)
        };
        intersection_area(left).total_cmp(&intersection_area(right))
    }) else {
        return Bounds::new(point(px(x), px(y)), size(px(width), px(height)));
    };

    let visible = display.visible_bounds();
    let visible_x = visible.origin.x.as_f32();
    let visible_y = visible.origin.y.as_f32();
    let visible_width = visible.size.width.as_f32();
    let visible_height = visible.size.height.as_f32();
    let width = width.max(1.0).min(visible_width);
    let height = height.max(1.0).min(visible_height);
    let x = if x.is_finite() {
        x.clamp(visible_x, visible_x + visible_width - width)
    } else {
        visible_x + (visible_width - width) / 2.0
    };
    let y = if y.is_finite() {
        y.clamp(visible_y, visible_y + visible_height - height)
    } else {
        visible_y + (visible_height - height) / 2.0
    };

    Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
}

fn restore_window_bounds(bounds: &SavedWindowBounds, cx: &App) -> gpui::WindowBounds {
    match bounds {
        SavedWindowBounds::Fullscreen {
            x,
            y,
            width,
            height,
        } => gpui::WindowBounds::Fullscreen(clamp_saved_window_bounds(*x, *y, *width, *height, cx)),
        SavedWindowBounds::Maximized {
            x,
            y,
            width,
            height,
        } => gpui::WindowBounds::Maximized(clamp_saved_window_bounds(*x, *y, *width, *height, cx)),
        SavedWindowBounds::Windowed {
            x,
            y,
            width,
            height,
        } => gpui::WindowBounds::Windowed(clamp_saved_window_bounds(*x, *y, *width, *height, cx)),
    }
}

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
    let vars = [
        "ALL_PROXY",
        "all_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ];
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
        window_options.window_bounds = Some(restore_window_bounds(bounds, cx));
    } else if let Some(display) = cx.displays().first().cloned() {
        let display_bounds = display.bounds();
        let width = display_bounds.size.width * 0.8;
        let height = display_bounds.size.height * 0.9;

        let x = display_bounds.origin.x + (display_bounds.size.width - width) / 2.0;

        #[cfg(target_os = "macos")]
        let y = display_bounds.origin.y;
        #[cfg(not(target_os = "macos"))]
        let y = display_bounds.origin.y + (display_bounds.size.height - height) / 2.0;

        window_options.window_bounds = Some(gpui::WindowBounds::Windowed(Bounds::new(
            point(x, y),
            size(width, height),
        )));
    }

    cx.open_window(window_options, |window, cx| {
        window.activate_window();
        window.set_window_title("ashell");
        gpui_component::Theme::sync_system_appearance(Some(window), cx);
        let view = cx.new(|cx| Ashell::new(window, cx));
        crate::app::system_menu::set_app_menus(cx);

        tracing::info!("[ui] main application window opened");
        let focus_handle = view.read(cx).focus_handle.clone();
        window.focus(&focus_handle, cx);

        let view_clone = view.clone();
        window.on_window_should_close(cx, move |window: &mut gpui::Window, cx: &mut gpui::App| {
            let handle = window.window_handle();
            if !cx.windows().contains(&handle) {
                tracing::warn!(
                    "[ui] window not found in app during close, skipping save layout state."
                );
                return true;
            }
            view_clone.update(cx, |this, cx| {
                this.save_layout_state(window, cx);
            });
            true
        });

        cx.new(|cx| Root::new(view, window, cx))
    })
    .expect("failed to open window");
}
