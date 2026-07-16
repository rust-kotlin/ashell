//! SFTP 内置编辑器。
//!
//! 双击 txt/sh 等文本文件时,下载内容到内存并用 gpui-component 的
//! CodeEditor 模式打开(自带行号 + Tree Sitter 语法高亮)。
//! Ctrl+S 保存后自动上传覆盖远程文件,无需落地临时文件。

use gpui::{
    AppContext, Entity, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _,
    Render, Styled, Window, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    v_flex, h_flex,
};
use rust_i18n::t;

use crate::sftp::SftpHandle;

/// 文件扩展名 → Tree Sitter 语言名映射。
/// 返回 None 时回退到纯多行模式(无高亮)。
fn language_for_path(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?.to_lowercase();
    match ext.as_str() {
        "sh" | "bash" | "zsh" => Some("bash"),
        "py" => Some("python"),
        "rs" => Some("rust"),
        "js" | "mjs" | "cjs" => Some("javascript"),
        "ts" => Some("typescript"),
        "json" => Some("json"),
        "yml" | "yaml" => Some("yaml"),
        "md" => Some("markdown"),
        "toml" => Some("toml"),
        "xml" | "html" | "htm" => Some("html"),
        "css" | "scss" => Some("css"),
        "c" | "h" => Some("c"),
        "cpp" | "cc" | "cxx" | "hpp" => Some("cpp"),
        "go" => Some("go"),
        "java" => Some("java"),
        "conf" | "cfg" | "ini" => Some("ini"),
        "sql" => Some("sql"),
        "lua" => Some("lua"),
        "rb" => Some("ruby"),
        "php" => Some("php"),
        "kt" => Some("kotlin"),
        "swift" => Some("swift"),
        _ => None,
    }
}

/// 从路径提取文件名(最后一段)。
fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub struct SftpEditor {
    pub remote_path: String,
    sftp: SftpHandle,
    input: Entity<InputState>,
    /// 有未保存的修改。
    pub dirty: bool,
    /// 正在上传中。
    pub saving: bool,
    /// 请求关闭(由 Ashell 在 render 后检测并清除)。
    pub should_close: bool,
}

impl SftpEditor {
    pub fn new(
        remote_path: String,
        content: String,
        sftp: SftpHandle,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        let lang = language_for_path(&remote_path);
        let input = cx.new(|cx| {
            let state = if let Some(lang) = lang {
                InputState::new(window, cx)
                    .code_editor(lang)
                    .rows(30)
            } else {
                InputState::new(window, cx)
                    .multi_line(true)
                    .rows(30)
            };
            state.default_value(content)
        });

        // 订阅编辑器内容变化 → 标记 dirty
        cx.subscribe(&input, |this, _input, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                this.mark_dirty(cx);
            }
        })
        .detach();

        Self {
            remote_path,
            sftp,
            input,
            dirty: false,
            saving: false,
            should_close: false,
        }
    }

    /// Ctrl+S:读取编辑器内容并上传。
    pub fn save(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) {
        if self.saving {
            return;
        }
        let content = self.input.read(cx).text().to_string();
        self.saving = true;
        self.dirty = false;
        self.sftp.upload_file_content(self.remote_path.clone(), content);
        cx.notify();
    }

    /// 收到上传完成事件后调用。
    pub fn mark_uploaded(&mut self, cx: &mut gpui::Context<Self>) {
        self.saving = false;
        cx.notify();
    }

    /// 标记为有修改(内容变化时由 InputEvent 触发)。
    pub fn mark_dirty(&mut self, cx: &mut gpui::Context<Self>) {
        if !self.dirty {
            self.dirty = true;
            cx.notify();
        }
    }

    fn close(&mut self, cx: &mut gpui::Context<Self>) {
        self.should_close = true;
        cx.notify();
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let ks = &event.keystroke;
        let key_lower = ks.key.to_ascii_lowercase();
        // Ctrl+S / Cmd+S 保存
        if (ks.modifiers.control || ks.modifiers.platform) && key_lower == "s" {
            self.save(window, cx);
            cx.stop_propagation();
        }
        // Esc 关闭
        if key_lower == "escape" {
            self.close(cx);
            cx.stop_propagation();
        }
    }
}

impl Render for SftpEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let filename = base_name(&self.remote_path);
        let lang_label = language_for_path(&self.remote_path)
            .unwrap_or("text");

        // 标题栏状态文本
        let status_text = if self.saving {
            t!("editor_saving").to_string()
        } else if self.dirty {
            t!("editor_unsaved").to_string()
        } else {
            t!("editor_saved").to_string()
        };

        let dirty = self.dirty;
        let saving = self.saving;

        gpui::div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .bg(theme.background.opacity(0.85))
            .flex()
            .items_center()
            .justify_center()
            .on_key_down(cx.listener(Self::handle_key_down))
            .child(
                v_flex()
                    .w(gpui::relative(0.8))
                    .max_w(px(1000.))
                    .h(gpui::relative(0.85))
                    .max_h(px(800.))
                    .bg(theme.background)
                    .border_1()
                    .border_color(theme.border)
                    .rounded_lg()
                    .shadow_lg()
                    .overflow_hidden()
                    // 标题栏
                    .child(
                        h_flex()
                            .w_full()
                            .h(px(44.))
                            .items_center()
                            .px_4()
                            .gap_3()
                            .bg(theme.muted.opacity(0.5))
                            .border_b_1()
                            .border_color(theme.border.opacity(0.5))
                            .child(
                                gpui::div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child(filename.to_string()),
                            )
                            .child(
                                gpui::div()
                                    .text_xs()
                                    .text_color(theme.muted)
                                    .child(format!("({})", lang_label)),
                            )
                            .child(gpui::div().flex_1())
                            .child(
                                gpui::div()
                                    .text_xs()
                                    .text_color(if dirty {
                                        theme.warning
                                    } else if saving {
                                        theme.muted
                                    } else {
                                        theme.success
                                    })
                                    .child(status_text),
                            )
                            .child(
                                Button::new("save-btn")
                                    .primary()
                                    .small()
                                    .disabled(saving)
                                    .label(t!("editor_save"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.save(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("close-btn")
                                    .small()
                                    .label(t!("editor_close"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.close(cx);
                                    })),
                            ),
                    )
                    // 编辑器区域
                    .child(
                        gpui::div()
                            .flex_1()
                            .min_h_0()
                            .child(Input::new(&self.input)),
                    ),
            )
    }
}
