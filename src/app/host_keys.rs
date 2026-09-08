use gpui::{Context, ParentElement as _, Styled as _, Window, div, px};
use gpui_component::{
    ActiveTheme as _, WindowExt as _, button::ButtonVariants as _, dialog::Dialog, h_flex, v_flex,
};
use rust_i18n::t;

use crate::{
    Ashell,
    app::{DialogKind, controls::pointer_button},
    session::host_keys::{self, HostKeyRequest},
    terminal::TabKind,
};

impl Ashell {
    fn host_key_request_is_current(&self, request: &HostKeyRequest) -> bool {
        request.is_current()
            && self.active_tab.as_deref() == Some(request.tab_id.as_str())
            && self.tabs.iter().any(|tab| {
                tab.id == request.tab_id
                    && tab.kind == TabKind::Ssh
                    && tab.session.as_ref().is_some_and(|session| {
                        session.host == request.host && session.port == request.port
                    })
            })
    }

    pub(crate) fn prompt_pending_host_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_host_keys.retain(|request| {
            request.is_current() && self.tabs.iter().any(|tab| tab.id == request.tab_id)
        });
        if self.active_dialog.is_some() {
            return;
        }
        let Some(index) = self
            .pending_host_keys
            .iter()
            .position(|request| self.host_key_request_is_current(request))
        else {
            return;
        };
        let request = self.pending_host_keys.remove(index);
        match host_keys::is_trusted(&request) {
            Ok(true) => {
                self.retry_disconnected_tab(&request.tab_id, cx);
                return;
            }
            Err(error) => {
                self.status = format!("{}: {error:#}", t!("ssh_verify_host_failed")).into();
                cx.notify();
                return;
            }
            Ok(false) => {}
        }
        self.active_dialog = Some(DialogKind::HostKeyVerification);
        self.host_key_error = None;
        let view = cx.entity();
        window.defer(cx, move |window, cx| {
            view.update(cx, |this, cx| {
                this.show_host_key_dialog(request, window, cx)
            });
        });
    }

    fn show_host_key_dialog(
        &mut self,
        request: HostKeyRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.host_key_request_is_current(&request) {
            self.finish_host_key_dialog(cx);
            return;
        }
        let fingerprint = request.identity.fingerprint().unwrap_or_default();
        let old_fingerprint = request
            .previous
            .as_ref()
            .and_then(|identity| identity.fingerprint().ok());
        let issuer = request.identity.authority_fingerprint().ok().flatten();
        let old_issuer = request
            .previous
            .as_ref()
            .and_then(|identity| identity.authority_fingerprint().ok().flatten());
        let changed = request.previous.is_some();
        let view = cx.entity();
        window.open_dialog(cx, move |dialog: Dialog, _, _| {
            dialog
                .title(t!("ssh_verify_host_title").to_string())
                .w(px(560.))
                .keyboard(false)
                .overlay_closable(false)
                .on_close({
                    let view = view.clone();
                    let request = request.clone();
                    move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            this.reject_host_key(&request, cx);
                        });
                    }
                })
                .content({
                    let view = view.clone();
                    let request = request.clone();
                    let fingerprint = fingerprint.clone();
                    let old_fingerprint = old_fingerprint.clone();
                    let issuer = issuer.clone();
                    let old_issuer = old_issuer.clone();
                    move |content, _, cx| {
                        let mut body = v_flex()
                            .gap_3()
                            .child(format!("{}:{}", request.host, request.port))
                            .child(div().whitespace_normal().child(if changed {
                                t!("ssh_host_key_changed").to_string()
                            } else {
                                t!("ssh_host_key_first_use").to_string()
                            }))
                            .child(format!("{}: {fingerprint}", t!("ssh_host_fingerprint")));
                        if let Some(previous) = &old_fingerprint {
                            body = body
                                .child(format!("{}: {previous}", t!("ssh_previous_fingerprint")));
                        }
                        if let Some(issuer) = &issuer {
                            body =
                                body.child(format!("{}: {issuer}", t!("ssh_certificate_issuer")));
                        }
                        if let Some(issuer) = &old_issuer {
                            body = body.child(format!(
                                "{}: {issuer}",
                                t!("ssh_previous_certificate_issuer")
                            ));
                        }
                        if let Some(error) = &view.read(cx).host_key_error {
                            body = body.child(
                                div()
                                    .text_color(cx.theme().danger)
                                    .whitespace_normal()
                                    .child(error.clone()),
                            );
                        }
                        content.child(body)
                    }
                })
                .footer({
                    let view = view.clone();
                    let request = request.clone();
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap_2()
                        .child(
                            pointer_button("host-key-reject")
                                .ghost()
                                .label(t!("cancel").to_string())
                                .on_click({
                                    let view = view.clone();
                                    let request = request.clone();
                                    move |_, window, cx| {
                                        view.update(cx, |this, cx| {
                                            this.reject_host_key(&request, cx)
                                        });
                                        window.close_dialog(cx);
                                    }
                                }),
                        )
                        .child(
                            pointer_button("host-key-accept")
                                .primary()
                                .label(t!("ssh_trust_and_reconnect").to_string())
                                .on_click(move |_, window, cx| {
                                    let approved = view.update(cx, |this, cx| {
                                        if !this.host_key_request_is_current(&request) {
                                            this.host_key_error =
                                                Some(t!("ssh_host_request_expired").to_string());
                                            cx.notify();
                                            return false;
                                        }
                                        if let Err(error) = host_keys::approve(&request) {
                                            this.host_key_error = Some(format!("{error:#}"));
                                            cx.notify();
                                            return false;
                                        }
                                        if let Some(tab) = this
                                            .tabs
                                            .iter_mut()
                                            .find(|tab| tab.id == request.tab_id)
                                        {
                                            tab.connected = false;
                                            tab.disconnected_reason =
                                                Some(t!("ssh_verify_host_required").to_string());
                                            tab.clear_terminal_activity();
                                        }
                                        this.retry_disconnected_tab(&request.tab_id, cx);
                                        this.finish_host_key_dialog(cx);
                                        true
                                    });
                                    if approved {
                                        window.close_dialog(cx);
                                    }
                                }),
                        )
                })
        });
    }

    fn finish_host_key_dialog(&mut self, cx: &mut Context<Self>) {
        if self.active_dialog == Some(DialogKind::HostKeyVerification) {
            self.active_dialog = None;
        }
        self.pending_host_keys
            .retain(|pending| pending.is_current());
        self.host_key_error = None;
        cx.notify();
    }

    /// Invalidate both setup attempts so their late verification events cannot
    /// reopen a prompt that the user just rejected.
    fn reject_host_key(&mut self, request: &HostKeyRequest, cx: &mut Context<Self>) {
        if self.host_key_request_is_current(request) {
            if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == request.tab_id) {
                tab.advance_backend_events();
                tab.send_backend(crate::terminal::BackendCommand::Close);
                tab.connected = false;
                tab.disconnected_reason = Some(t!("ssh_verify_host_required").to_string());
            }
            if let Some(group) = self
                .tab_groups
                .iter()
                .find(|group| group.sftp_tab_id.as_deref() == Some(request.tab_id.as_str()))
            {
                if let Some(handle) = self.sftp_handles.get(&group.id) {
                    handle.close();
                }
            }
        }
        self.finish_host_key_dialog(cx);
    }
}
