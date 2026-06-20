use gpui::{App, AppContext as _, Bounds, WindowOptions, point, px, size};
use gpui_component::Root;

use crate::Ashell;
use crate::app::constants::{
    COLLAPSED_SIDEBAR_WIDTH, SIDEBAR_WIDTH, TAB_BAR_HEIGHT, TERMINAL_PADDING_X,
    TERMINAL_PADDING_Y,
};
use crate::session::config::ConfigStore;

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
        ) || key.starts_with("LC_");

        if should_import {
            unsafe {
                std::env::set_var(key, value);
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn sync_macos_launch_environment() {}

pub(crate) fn open_main_window(cx: &mut App) {
    let config = ConfigStore::load().unwrap_or_else(|_| ConfigStore::in_memory());

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
            } => gpui::WindowBounds::Windowed(Bounds::new(
                point(px(*x), px(*y)),
                size(px(*width), px(*height)),
            )),
        });
    } else if let Some(display) = cx.displays().first().cloned() {
        let display_bounds = display.bounds();
        let (cols, rows) = config.terminal_size();
        let font_size = config.terminal_font_size();
        let cell_width = (font_size * 0.646).max(6.0);
        let line_height = (font_size * 1.385).max(font_size + 2.0);
        let sidebar_width = if config.sidebar_collapsed() {
            COLLAPSED_SIDEBAR_WIDTH
        } else {
            SIDEBAR_WIDTH
        };
        let desired_width = cols as f32 * cell_width + sidebar_width + TERMINAL_PADDING_X + 16.0;
        let desired_height =
            rows as f32 * line_height + TAB_BAR_HEIGHT + TERMINAL_PADDING_Y + 248.0;
        let width = px(desired_width)
            .min(display_bounds.size.width * 0.85)
            .max(px(720.));
        let height = px(desired_height)
            .min(display_bounds.size.height * 0.86)
            .max(px(560.));

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

        tracing::info!("[ui] main application window opened");
        let focus_handle = view.read(cx).focus_handle.clone();
        window.focus(&focus_handle, cx);

        let sftp_minimized = view.read(cx).config.sftp_panel_minimized();
        if sftp_minimized {
            window.defer(cx, {
                let view = view.clone();
                move |window, cx| {
                    view.update(cx, |this, cx| {
                        this.toggle_sftp_minimized(window, cx);
                    });
                }
            });
        }

        let workspace_panels_clone = view.read(cx).workspace_panels.clone();
        let body_panels_clone = view.read(cx).body_panels.clone();
        let view_clone = view.clone();
        window.on_window_should_close(cx, move |window: &mut gpui::Window, cx: &mut gpui::App| {
            if view_clone.read(cx).is_layout_reset {
                tracing::info!("[ui] layout was reset, skipping save layout state.");
                return true;
            }
            tracing::info!("[ui] main application window closed, saving layout state...");
            let mut config = ConfigStore::load().unwrap_or_else(|_| ConfigStore::in_memory());
            let current_bounds = window.window_bounds();
            let saved_bounds = match current_bounds {
                gpui::WindowBounds::Fullscreen(b) => {
                    crate::session::config::SavedWindowBounds::Fullscreen {
                        x: b.origin.x.into(),
                        y: b.origin.y.into(),
                        width: b.size.width.into(),
                        height: b.size.height.into(),
                    }
                }
                gpui::WindowBounds::Maximized(b) => {
                    crate::session::config::SavedWindowBounds::Maximized {
                        x: b.origin.x.into(),
                        y: b.origin.y.into(),
                        width: b.size.width.into(),
                        height: b.size.height.into(),
                    }
                }
                gpui::WindowBounds::Windowed(b) => {
                    crate::session::config::SavedWindowBounds::Windowed {
                        x: b.origin.x.into(),
                        y: b.origin.y.into(),
                        width: b.size.width.into(),
                        height: b.size.height.into(),
                    }
                }
            };
            let workspace_sizes: Vec<f32> = workspace_panels_clone
                .read(cx)
                .sizes()
                .iter()
                .map(|s| s.into())
                .collect();
            let mut body_sizes: Vec<f32> = body_panels_clone
                .read(cx)
                .sizes()
                .iter()
                .map(|s| s.into())
                .collect();

            if view_clone.read(cx).sftp_panel_minimized {
                if let Some(prev) = view_clone.read(cx).prev_monitoring_size {
                    if body_sizes.len() > 1 {
                        body_sizes[1] = prev.into();
                    }
                }
            }

            config.set_layout_state(Some(saved_bounds), Some(workspace_sizes), Some(body_sizes));
            config.set_sidebar_collapsed(view_clone.read(cx).sidebar_collapsed);
            config.set_sftp_panel_minimized(view_clone.read(cx).sftp_panel_minimized);
            let _ = config.save();
            true
        });

        cx.new(|cx| Root::new(view, window, cx))
    })
    .expect("failed to open window");
}
