//! SFTP 内置编辑器(多标签页)。
//!
//! 双击 txt/sh/yaml/json 等文本文件时,下载内容到内存并用 gpui-component 的
//! CodeEditor 模式打开(自带行号 + Tree Sitter 语法高亮)。
//! 多个文件以 tab 形式合并到同一个编辑器窗口,可互相切换。
//! Ctrl+S 保存当前 tab,Ctrl+W 关闭当前 tab,Esc 关闭整个编辑器。
//! 保存后自动上传覆盖远程文件,无需落地临时文件。

use gpui::{
    AppContext, Entity, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _,
    Render, Styled, Window, px,
};
use gpui::prelude::FluentBuilder as _;
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

/// 单个文件对应的编辑器状态。
struct EditorTab {
    remote_path: String,
    input: Entity<InputState>,
    /// 有未保存的修改。
    dirty: bool,
    /// 正在上传中。
    saving: bool,
}

impl EditorTab {
    fn new(
        remote_path: String,
        content: String,
        window: &mut Window,
        cx: &mut gpui::Context<SftpEditor>,
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

        // 订阅该 tab 的内容变化 → 标记对应 tab dirty
        cx.subscribe(&input, |this, emitter, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                // 通过 emitter 找到是哪个 tab 触发的
                if let Some(tab) = this.tabs.iter_mut().find(|t| t.input == emitter) {
                    if !tab.dirty {
                        tab.dirty = true;
                        cx.notify();
                    }
                }
            }
        })
        .detach();

        Self {
            remote_path,
            input,
            dirty: false,
            saving: false,
        }
    }
}

pub struct SftpEditor {
    sftp: SftpHandle,
    tabs: Vec<EditorTab>,
    /// 当前激活的 tab 索引。
    active_idx: usize,
    /// 请求关闭整个编辑器(由 Ashell 在 render 后检测并清除)。
    pub should_close: bool,
}

impl SftpEditor {
    /// 创建编辑器并打开第一个文件。
    pub fn new(
        remote_path: String,
        content: String,
        sftp: SftpHandle,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        let tab = EditorTab::new(remote_path, content, window, cx);
        Self {
            sftp,
            tabs: vec![tab],
            active_idx: 0,
            should_close: false,
        }
    }

    /// 打开一个文件到新 tab。若该路径已存在,则切换到对应 tab 不重复打开。
    /// 返回该 tab 的索引。
    pub fn open_file(
        &mut self,
        remote_path: String,
        content: String,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> usize {
        // 已存在则切换
        if let Some(idx) = self.tabs.iter().position(|t| t.remote_path == remote_path) {
            self.active_idx = idx;
            cx.notify();
            return idx;
        }
        let tab = EditorTab::new(remote_path, content, window, cx);
        self.tabs.push(tab);
        self.active_idx = self.tabs.len() - 1;
        cx.notify();
        self.active_idx
    }

    /// 是否已打开指定路径的文件。
    pub fn has_path(&self, path: &str) -> bool {
        self.tabs.iter().any(|t| t.remote_path == path)
    }

    /// 切换到指定路径的 tab,成功返回 true。
    pub fn focus_path(&mut self, path: &str, cx: &mut gpui::Context<Self>) -> bool {
        if let Some(idx) = self.tabs.iter().position(|t| t.remote_path == path) {
            if self.active_idx != idx {
                self.active_idx = idx;
                cx.notify();
            }
            true
        } else {
            false
        }
    }

    /// 当前激活 tab。
    fn active_tab(&self) -> Option<&EditorTab> {
        self.tabs.get(self.active_idx)
    }

    /// Ctrl+S:保存当前激活 tab,读取其内容并上传。
    pub fn save_active(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) {
        let Some(tab) = self.tabs.get_mut(self.active_idx) else {
            return;
        };
        if tab.saving {
            return;
        }
        let content = tab.input.read(cx).text().to_string();
        let path = tab.remote_path.clone();
        tab.saving = true;
        tab.dirty = false;
        self.sftp.upload_file_content(path, content);
        cx.notify();
    }

    /// 收到上传完成事件后,按 remote_path 标记对应 tab。
    pub fn mark_uploaded(&mut self, remote_path: &str, cx: &mut gpui::Context<Self>) {
        for tab in &mut self.tabs {
            if tab.remote_path == remote_path {
                tab.saving = false;
            }
        }
        cx.notify();
    }

    /// 关闭当前激活 tab。若关闭后无 tab,则标记整个编辑器关闭。
    fn close_active(&mut self, cx: &mut gpui::Context<Self>) {
        if self.tabs.is_empty() {
            self.should_close = true;
            cx.notify();
            return;
        }
        self.tabs.remove(self.active_idx);
        if self.tabs.is_empty() {
            self.should_close = true;
        } else if self.active_idx >= self.tabs.len() {
            self.active_idx = self.tabs.len() - 1;
        }
        cx.notify();
    }

    /// 关闭指定索引的 tab(点击 tab 上的 x)。
    fn close_tab(&mut self, idx: usize, cx: &mut gpui::Context<Self>) {
        if idx >= self.tabs.len() {
            return;
        }
        self.tabs.remove(idx);
        if self.tabs.is_empty() {
            self.should_close = true;
        } else if self.active_idx >= self.tabs.len() {
            self.active_idx = self.tabs.len() - 1;
        } else if idx < self.active_idx {
            self.active_idx -= 1;
        }
        cx.notify();
    }

    fn close_all(&mut self, cx: &mut gpui::Context<Self>) {
        self.should_close = true;
        cx.notify();
    }

    fn switch_tab(&mut self, idx: usize, cx: &mut gpui::Context<Self>) {
        if idx < self.tabs.len() && idx != self.active_idx {
            self.active_idx = idx;
            cx.notify();
        }
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let ks = &event.keystroke;
        let key_lower = ks.key.to_ascii_lowercase();
        // Ctrl+S / Cmd+S 保存当前 tab
        if (ks.modifiers.control || ks.modifiers.platform) && key_lower == "s" {
            self.save_active(window, cx);
            cx.stop_propagation();
        }
        // Ctrl+W 关闭当前 tab
        if (ks.modifiers.control || ks.modifiers.platform) && key_lower == "w" {
            self.close_active(cx);
            cx.stop_propagation();
        }
        // Esc 关闭整个编辑器
        if key_lower == "escape" {
            self.close_all(cx);
            cx.stop_propagation();
        }
    }
}

impl Render for SftpEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        let active = self.active_tab();
        let filename = active
            .map(|t| base_name(&t.remote_path))
            .unwrap_or("");
        let lang_label = active
            .and_then(|t| language_for_path(&t.remote_path))
            .unwrap_or("text");

        let status_text = if let Some(t) = active {
            if t.saving {
                t!("editor_saving").to_string()
            } else if t.dirty {
                t!("editor_unsaved").to_string()
            } else {
                t!("editor_saved").to_string()
            }
        } else {
            String::new()
        };

        let dirty = active.map(|t| t.dirty).unwrap_or(false);
        let saving = active.map(|t| t.saving).unwrap_or(false);
        let active_idx = self.active_idx;
        let tab_count = self.tabs.len();

        // tab 栏数据快照
        let tab_snapshots: Vec<(String, bool, bool)> = self
            .tabs
            .iter()
            .map(|t| (base_name(&t.remote_path).to_string(), t.dirty, t.saving))
            .collect();

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
                    // tab 栏(多文件时显示)
                    .when(tab_count > 1, |this| {
                        this.child(
                            h_flex()
                                .w_full()
                                .overflow_hidden()
                                .bg(theme.muted.opacity(0.3))
                                .border_b_1()
                                .border_color(theme.border.opacity(0.5))
                                .children(
                                    tab_snapshots
                                        .iter()
                                        .enumerate()
                                        .map(|(idx, (name, dirty, _saving))| {
                                            let is_active = idx == active_idx;
                                            h_flex()
                                                .id(("editor-tab", idx))
                                                .items_center()
                                                .gap_1()
                                                .px_3()
                                                .py_2()
                                                .min_w(px(120.))
                                                .max_w(px(220.))
                                                .cursor_pointer()
                                                .border_r_1()
                                                .border_color(theme.border.opacity(0.3))
                                                .bg(if is_active {
                                                    theme.background
                                                } else {
                                                    gpui::transparent_black()
                                                })
                                                .text_color(if is_active {
                                                    theme.foreground
                                                } else {
                                                    theme.muted
                                                })
                                                .text_sm()
                                                .when(*dirty, |this| {
                                                    this.child(
                                                        gpui::div()
                                                            .w(px(6.))
                                                            .h(px(6.))
                                                            .rounded_full()
                                                            .bg(theme.warning),
                                                    )
                                                })
                                                .child(
                                                    gpui::div()
                                                        .flex_1()
                                                        .overflow_hidden()
                                                        .text_ellipsis()
                                                        .whitespace_nowrap()
                                                        .child(name.clone()),
                                                )
                                                .child(
                                                    gpui::div()
                                                        .id(("tab-close", idx))
                                                        .cursor_pointer()
                                                        .text_color(theme.muted)
                                                        .hover(|this| {
                                                            this.text_color(theme.foreground)
                                                        })
                                                        .child("×")
                                                        .on_mouse_down(
                                                            gpui::MouseButton::Left,
                                                            cx.listener(move |this, _ev, _window, cx| {
                                                                this.close_tab(idx, cx);
                                                            }),
                                                        ),
                                                )
                                                .on_mouse_down(
                                                    gpui::MouseButton::Left,
                                                    cx.listener(move |this, _ev, _window, cx| {
                                                        this.switch_tab(idx, cx);
                                                    }),
                                                )
                                        })
                                        .collect::<Vec<_>>(),
                                ),
                        )
                    })
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
                                        this.save_active(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("close-btn")
                                    .small()
                                    .label(t!("editor_close"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.close_all(cx);
                                    })),
                            ),
                    )
                    // 编辑器区域(渲染当前激活 tab 的 input)
                    .child(
                        gpui::div()
                            .flex_1()
                            .min_h_0()
                            .when_some(self.active_tab(), |this, t| {
                                this.child(Input::new(&t.input))
                            }),
                    ),
            )
    }
}
