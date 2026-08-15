use gpui::{Context, PathPromptOptions, Pixels, Point, Window};

use crate::{
    Ashell, SftpContextMenuState,
    sftp::{RemoteEntry, SftpHandle},
    terminal,
};

impl Ashell {
    pub(crate) fn active_sftp(&self) -> Option<&terminal::SftpUiState> {
        self.active_group
            .as_ref()
            .and_then(|id| self.tab_groups.iter().find(|g| &g.id == id))
            .and_then(|g| g.sftp.as_ref())
    }

    pub(crate) fn active_sftp_mut(&mut self) -> Option<&mut terminal::SftpUiState> {
        let active_id = self.active_group.clone()?;
        self.tab_groups
            .iter_mut()
            .find(|g| g.id == active_id)
            .and_then(|g| g.sftp.as_mut())
    }

    pub(crate) fn active_sftp_handle(&self) -> Option<&SftpHandle> {
        self.active_group
            .as_ref()
            .and_then(|id| self.sftp_handles.get(id))
    }

    pub(crate) fn navigate_sftp(&mut self, path: String, cx: &mut Context<Self>) {
        let Some((current_path, home_dir)) = self
            .active_sftp()
            .map(|sftp| (sftp.current_path.clone(), sftp.home_dir.clone()))
        else {
            return;
        };
        let path = crate::sftp::normalize_remote_path(&path, &current_path, &home_dir);
        let Some(handle) = self.active_sftp_handle().cloned() else {
            return;
        };

        tracing::info!("[sftp] navigating to directory: '{}'", path);
        let ancestors = crate::sftp::remote_path_ancestors(&path);
        let mut paths_to_load = Vec::new();
        if let Some(sftp) = self.active_sftp_mut() {
            sftp.current_path = path.clone();
            sftp.selected_path = None;
            sftp.preview = None;
            sftp.selected_entries.clear();
            sftp.expand_to(&path);
            sftp.begin_directory_load(&path);
            for ancestor in ancestors {
                if ancestor != path
                    && !sftp.directory_cache.contains_key(&ancestor)
                    && !sftp.loading_directories.contains(&ancestor)
                {
                    sftp.begin_directory_load(&ancestor);
                    paths_to_load.push(ancestor);
                }
            }
        }
        self.pending_sftp_path_sync = Some(path.clone());
        for ancestor in paths_to_load {
            handle.list_dir(ancestor);
        }
        handle.list_dir(path);
        cx.notify();
    }

    pub(crate) fn toggle_sftp_tree_directory(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(handle) = self.active_sftp_handle().cloned() else {
            return;
        };
        let mut should_load = false;
        if let Some(sftp) = self.active_sftp_mut() {
            let has_error = sftp.directory_errors.contains_key(&path);
            let has_cache = sftp.directory_cache.contains_key(&path);
            if sftp.expanded_directories.contains(&path) && !has_error && has_cache {
                sftp.expanded_directories.remove(&path);
                cx.notify();
                return;
            }
            sftp.expanded_directories.insert(path.clone());
            should_load = (has_error || !has_cache) && !sftp.loading_directories.contains(&path);
            if should_load {
                sftp.begin_directory_load(&path);
            }
        }
        if should_load {
            handle.list_dir(path);
        }
        cx.notify();
    }

    pub(crate) fn reveal_current_sftp_directory(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.active_sftp_handle().cloned() else {
            return;
        };
        let Some(current_path) = self.active_sftp().map(|sftp| sftp.current_path.clone()) else {
            return;
        };
        let ancestors = crate::sftp::remote_path_ancestors(&current_path);
        let mut paths_to_load = Vec::new();
        if let Some(sftp) = self.active_sftp_mut() {
            sftp.expand_to(&current_path);
            for path in ancestors {
                if !sftp.directory_cache.contains_key(&path)
                    && !sftp.loading_directories.contains(&path)
                {
                    sftp.begin_directory_load(&path);
                    paths_to_load.push(path);
                }
            }
        }
        for path in paths_to_load {
            handle.list_dir(path);
        }
        if let Some(index) = self.active_sftp().and_then(|sftp| {
            sftp.tree_rows(self.show_hidden_files)
                .iter()
                .position(|row| row.path == current_path)
        }) {
            self.remote_tree_scroll_handle
                .scroll_to_item(index, gpui::ScrollStrategy::Center);
        }
        cx.notify();
    }

    pub(crate) fn collapse_sftp_tree(&mut self, cx: &mut Context<Self>) {
        if let Some(sftp) = self.active_sftp_mut() {
            sftp.collapse_all();
            cx.notify();
        }
    }

    pub(crate) fn select_sftp_entry(&mut self, entry: RemoteEntry, cx: &mut Context<Self>) {
        if entry.is_dir {
            self.navigate_sftp(entry.full_path, cx);
            return;
        }
        self.mark_sftp_entry_selected(&entry.full_path, cx);
        if let Some(sftp) = self.active_sftp_mut() {
            if !sftp.selected_entries.remove(&entry.full_path) {
                sftp.selected_entries.insert(entry.full_path);
            }
        }
    }

    pub(crate) fn mark_sftp_entry_selected(&mut self, path: &str, cx: &mut Context<Self>) {
        if let Some(sftp) = self.active_sftp_mut() {
            sftp.selected_path = Some(path.to_string());
        }
        cx.notify();
    }

    pub(crate) fn sftp_parent_path(path: &str) -> String {
        if path == "/" {
            return "/".to_string();
        }
        path.trim_end_matches('/')
            .rsplit_once('/')
            .map(|(parent, _)| {
                if parent.is_empty() {
                    "/".to_string()
                } else {
                    parent.to_string()
                }
            })
            .unwrap_or_else(|| "/".to_string())
    }

    pub(crate) fn refresh_sftp(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.active_sftp().map(|sftp| sftp.current_path.clone()) {
            self.navigate_sftp(path, cx);
        }
    }

    pub(crate) fn sync_sftp_path_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.pending_sftp_path_sync.take() else {
            return;
        };
        self.sftp_path_input.update(cx, |state, cx| {
            state.set_value(path, window, cx);
        });
    }

    pub(crate) fn open_sftp_context_menu(
        &mut self,
        remote_path: String,
        is_dir: bool,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.sftp_context_menu = Some(SftpContextMenuState {
            remote_path,
            is_dir,
            position,
        });
        cx.notify();
    }

    pub(crate) fn dismiss_sftp_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.sftp_context_menu.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn trigger_sftp_context_download(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(menu) = self.sftp_context_menu.take() else {
            return;
        };
        self.download_sftp_entry(menu.remote_path, window, cx);
        cx.notify();
    }

    pub(crate) fn trigger_sftp_context_edit(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(menu) = self.sftp_context_menu.take() else {
            return;
        };
        tracing::info!("[sftp] opening in-app editor for: '{}'", menu.remote_path);
        self.show_sftp_editor_dialog(menu.remote_path, window, cx);
    }

    pub(crate) fn trigger_sftp_context_rename(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(menu) = self.sftp_context_menu.take() else {
            return;
        };
        self.show_sftp_rename_dialog(menu.remote_path, window, cx);
    }

    pub(crate) fn download_sftp_entry(
        &mut self,
        remote_path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(handle) = self.active_sftp_handle().cloned() else {
            return;
        };
        let path_prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Select Download Folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            match path_prompt.await {
                Ok(Ok(Some(mut paths))) => {
                    if let Some(folder) = paths.pop() {
                        let local_path = folder.to_string_lossy().to_string();
                        tracing::info!(
                            "[sftp] initiating download of '{}' to '{}'",
                            remote_path,
                            local_path
                        );
                        handle.download(remote_path, local_path);
                        this.update(cx, |this, cx| {
                            this.show_transfers_dialog = true;
                            cx.notify();
                        })?;
                    }
                }
                Ok(Err(err)) => {
                    this.update(cx, |this, cx| {
                        this.status = format!("download picker failed: {err}").into();
                        cx.notify();
                    })?;
                }
                _ => {}
            }
            Ok::<(), anyhow::Error>(())
        })
        .detach();
    }

    pub(crate) fn upload_sftp_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(handle) = self.active_sftp_handle().cloned() else {
            return;
        };
        let remote_dir = self
            .active_sftp()
            .map(|sftp| sftp.current_path.clone())
            .unwrap_or_else(|| "/".into());
        let path_prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Select File to Upload".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            match path_prompt.await {
                Ok(Ok(Some(mut paths))) => {
                    if let Some(file) = paths.pop() {
                        let local_path = file.to_string_lossy().to_string();
                        tracing::info!(
                            "[sftp] initiating upload of file '{}' to '{}'",
                            local_path,
                            remote_dir
                        );
                        handle.upload_paths(vec![local_path], remote_dir);
                        this.update(cx, |this, cx| {
                            this.show_transfers_dialog = true;
                            cx.notify();
                        })?;
                    }
                }
                Ok(Err(err)) => {
                    this.update(cx, |this, cx| {
                        this.status = format!("upload picker failed: {err}").into();
                        cx.notify();
                    })?;
                }
                _ => {}
            }
            Ok::<(), anyhow::Error>(())
        })
        .detach();
    }

    pub(crate) fn upload_sftp_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(handle) = self.active_sftp_handle().cloned() else {
            return;
        };
        let remote_dir = self
            .active_sftp()
            .map(|sftp| sftp.current_path.clone())
            .unwrap_or_else(|| "/".into());
        let path_prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Select Folder to Upload".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            match path_prompt.await {
                Ok(Ok(Some(mut paths))) => {
                    if let Some(folder) = paths.pop() {
                        let local_path = folder.to_string_lossy().to_string();
                        tracing::info!(
                            "[sftp] initiating upload of folder '{}' to '{}'",
                            local_path,
                            remote_dir
                        );
                        handle.upload_paths(vec![local_path], remote_dir);
                        this.update(cx, |this, cx| {
                            this.show_transfers_dialog = true;
                            cx.notify();
                        })?;
                    }
                }
                Ok(Err(err)) => {
                    this.update(cx, |this, cx| {
                        this.status = format!("upload picker failed: {err}").into();
                        cx.notify();
                    })?;
                }
                _ => {}
            }
            Ok::<(), anyhow::Error>(())
        })
        .detach();
    }

    pub(crate) fn toggle_sftp_entry(
        &mut self,
        path: String,
        checked: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(sftp) = self.active_sftp_mut() {
            if checked {
                sftp.selected_entries.insert(path);
            } else {
                sftp.selected_entries.remove(&path);
            }
            cx.notify();
        }
    }

    pub(crate) fn toggle_all_sftp_entries(&mut self, checked: bool, cx: &mut Context<Self>) {
        if let Some(sftp) = self.active_sftp_mut() {
            if checked {
                let paths: Vec<String> = sftp
                    .current_entries()
                    .iter()
                    .map(|entry| entry.full_path.clone())
                    .collect();
                for path in paths {
                    sftp.selected_entries.insert(path);
                }
            } else {
                sftp.selected_entries.clear();
            }
            cx.notify();
        }
    }

    pub(crate) fn download_selected_sftp_entries(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(sftp) = self.active_sftp() else {
            return;
        };
        let selected: Vec<String> = sftp.selected_entries.iter().cloned().collect();
        if selected.is_empty() {
            return;
        }

        let Some(handle) = self.active_sftp_handle().cloned() else {
            return;
        };

        let path_prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Select Download Folder".into()),
        });

        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(mut paths))) = path_prompt.await {
                if let Some(folder) = paths.pop() {
                    let local_dir = folder.to_string_lossy().to_string();
                    tracing::info!(
                        "[sftp] initiating batch download of {} entries to '{}'",
                        selected.len(),
                        local_dir
                    );
                    for remote in selected {
                        let _ = handle.commands.send(crate::sftp::SftpCommand::Download {
                            remote,
                            local_dir: local_dir.clone(),
                        });
                    }

                    let _ = this.update(cx, |this, cx| {
                        if let Some(sftp_mut) = this.active_sftp_mut() {
                            sftp_mut.selected_entries.clear();
                        }
                        this.show_transfers_dialog = true;
                        cx.notify();
                    });
                }
            }
            Ok::<(), anyhow::Error>(())
        })
        .detach();
    }

    pub(crate) fn upload_sftp_files_batch(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        if let Some(sftp) = self.active_sftp() {
            if let Some(handle) = self.active_sftp_handle() {
                tracing::info!(
                    "[sftp] initiating batch upload of {} files to '{}'",
                    paths.len(),
                    sftp.current_path
                );
                let _ = handle.commands.send(crate::sftp::SftpCommand::UploadPaths {
                    locals: paths,
                    remote_dir: sftp.current_path.clone(),
                });
                self.show_transfers_dialog = true;
                cx.notify();
            }
        }
    }
}
