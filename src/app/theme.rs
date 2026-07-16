use anyhow::{Context as _, Result};
use gpui::{App, Context, SharedString, Window, px};
use gpui_component::{ActiveTheme as _, Theme, ThemeMode, ThemeRegistry};

use crate::Ashell;

pub(crate) const EMBEDDED_THEME_JSONS: &[&str] = &[
    include_str!("../../assets/themes/matrix.json"),
    include_str!("../../assets/themes/tokyonight.json"),
    include_str!("../../assets/themes/gruvbox.json"),
    include_str!("../../assets/themes/solarized.json"),
    include_str!("../../assets/themes/phygerr.json"),
];

use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) static USING_SYSTEM_MAPLE: AtomicBool = AtomicBool::new(false);

pub(crate) fn load_fonts(cx: &mut App) -> Result<()> {
    let has_system_maple = cx
        .text_system()
        .all_font_names()
        .contains(&"Maple Mono NF CN".to_string());
    if has_system_maple {
        USING_SYSTEM_MAPLE.store(true, Ordering::Relaxed);
    } else {
        let regular = std::borrow::Cow::Borrowed(
            include_bytes!("../../assets/fonts/MapleMono-NF-CN-Regular.ttf").as_slice(),
        );
        let bold = std::borrow::Cow::Borrowed(
            include_bytes!("../../assets/fonts/MapleMono-NF-CN-Bold.ttf").as_slice(),
        );
        cx.text_system()
            .add_fonts(vec![regular, bold])
            .context("load Maple Mono NF CN fonts")?;
    }
    set_theme_font_names(cx.global_mut::<Theme>(), ".SystemUIFont");
    Ok(())
}

pub(crate) fn load_embedded_themes(cx: &mut App) {
    let registry = ThemeRegistry::global_mut(cx);
    for theme_json in EMBEDDED_THEME_JSONS {
        if let Err(err) = registry.load_themes_from_str(theme_json) {
            tracing::warn!("failed to load embedded theme: {err:#}");
        }
    }
}

pub(crate) fn set_theme_font_names(theme: &mut Theme, ui_font_family: &str) {
    theme.font_family = ui_font_family.into();
    theme.mono_font_family = ui_font_family.into();
}

impl Ashell {
    pub(crate) fn switch_theme_mode(
        &mut self,
        mode: ThemeMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.follow_system_theme = false;
        self.theme_mode = mode;
        self.apply_theme_preferences(window, cx);
        self.status = format!("theme mode: {}", cx.theme().mode.name()).into();
        self.persist_theme_preferences();
        cx.notify();
    }

    pub(crate) fn apply_theme(
        &mut self,
        name: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(theme_config) = ThemeRegistry::global(cx).themes().get(&name).cloned() else {
            self.status = format!("theme not found: {name}").into();
            cx.notify();
            return;
        };

        if theme_config.mode.is_dark() {
            self.dark_theme_name = name.clone();
        } else {
            self.light_theme_name = name.clone();
        }
        self.apply_theme_preferences(window, cx);
        self.status = format!("theme: {name}").into();
        self.persist_theme_preferences();
        window.refresh();
        cx.notify();
    }

    pub(crate) fn set_follow_system_theme(
        &mut self,
        follow: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.follow_system_theme = follow;
        if follow {
            self.status = "theme mode: system".into();
        } else {
            self.status = format!("theme mode: {}", cx.theme().mode.name()).into();
        }
        self.apply_theme_preferences(window, cx);
        self.persist_theme_preferences();
        cx.notify();
    }

    pub(crate) fn set_display_language(
        &mut self,
        locale: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.config.set_locale(locale);
        let mut active_locale = locale.to_string();
        if active_locale == "system" {
            active_locale = sys_locale::get_locale().unwrap_or_else(|| "en".to_string());
            if active_locale.starts_with("zh") {
                active_locale = "zh-CN".to_string();
            } else {
                active_locale = "en".to_string();
            }
        }
        rust_i18n::set_locale(&active_locale);
        gpui_component::set_locale(&active_locale);
        if let Err(err) = self.config.save() {
            tracing::warn!("failed to save language preferences: {err:#}");
        }
        window.refresh();
        cx.notify();
    }

    pub(crate) fn apply_theme_preferences(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let light_theme = ThemeRegistry::global(cx)
            .themes()
            .get(&self.light_theme_name)
            .cloned()
            .unwrap_or_else(|| ThemeRegistry::global(cx).default_light_theme().clone());
        let dark_theme = ThemeRegistry::global(cx)
            .themes()
            .get(&self.dark_theme_name)
            .cloned()
            .unwrap_or_else(|| ThemeRegistry::global(cx).default_dark_theme().clone());
        let theme = Theme::global_mut(cx);
        theme.light_theme = light_theme;
        theme.dark_theme = dark_theme;
        theme.font_size = px(self.ui_font_size);
        set_theme_font_names(theme, &self.ui_font_family);

        if self.follow_system_theme {
            Theme::sync_system_appearance(Some(window), cx);
        } else {
            Theme::change(self.theme_mode, Some(window), cx);
        }
    }

    pub(crate) fn persist_theme_preferences(&mut self) {
        let theme_mode_str = match self.theme_mode {
            ThemeMode::Light => "light",
            ThemeMode::Dark => "dark",
        };
        self.config.set_theme_preferences(
            self.follow_system_theme,
            theme_mode_str,
            self.light_theme_name.to_string(),
            self.dark_theme_name.to_string(),
        );
        if let Err(err) = self.config.save() {
            tracing::warn!("failed to save theme preferences: {err:#}");
        }
    }
}
