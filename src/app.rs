#[cfg(all(feature = "ghostty", target_os = "macos"))]
use crate::ghostty::{TerminalBackend, TerminalView};
use crate::{
    appearance::{self, Icons},
    diagnostics,
    files::{Entry, Event, Request, Worker},
    preview::{CHUNK_BYTES, FilePreview},
    profiles::{self, Profile},
    tree::{DirectoryTree, TreeCache, TreeRow},
};
use eframe::egui::{self, Color32, RichText};
use egui_term::{BackendSettings, PtyEvent};
#[cfg(not(all(feature = "ghostty", target_os = "macos")))]
use egui_term::{TerminalBackend, TerminalView};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::PathBuf,
    sync::{
        atomic::Ordering,
        mpsc::{self, Receiver, Sender},
    },
};
use zeroize::Zeroizing;

struct Tab {
    id: u64,
    label: String,
    backend: TerminalBackend,
    ended: bool,
}
struct ConnectionEditor {
    profile: Profile,
    index: Option<usize>,
    error: String,
    focus_name: bool,
}

pub struct Relay {
    icons: Icons,
    terminal_theme: egui_term::TerminalTheme,
    profiles: Vec<Profile>,
    config_path: Option<PathBuf>,
    config_error: bool,
    selected: Option<usize>,
    draft: Profile,
    connection_editor: Option<ConnectionEditor>,
    search: String,
    message: String,
    tabs: Vec<Tab>,
    #[cfg(all(feature = "ghostty", target_os = "macos"))]
    ghostty_runtime: Option<crate::ghostty::Runtime>,
    active: Option<u64>,
    next_id: u64,
    focus_terminal: bool,
    terminal_events: Receiver<(u64, PtyEvent)>,
    terminal_sender: Sender<(u64, PtyEvent)>,
    files: Worker,
    files_connected: bool,
    files_busy: bool,
    file_list_pending: BTreeSet<String>,
    file_worker_stopped: bool,
    files_label: String,
    secret: Zeroizing<String>,
    pending_profile: Option<Profile>,
    fingerprint: Option<String>,
    root: String,
    destination: String,
    tree: DirectoryTree,
    tree_cache: TreeCache,
    #[cfg(test)]
    painted_tree_rows: usize,
    file_search: String,
    file_search_query: String,
    file_search_expanded: BTreeMap<String, bool>,
    expanded: BTreeSet<String>,
    selected_file: Option<Entry>,
    preview: Option<FilePreview>,
    preview_active: bool,
    preview_inflight: Option<u64>,
    resume_preview_after_connect: bool,
    next_preview_id: u64,
    progress: Option<(String, u64, u64)>,
    file_panel: bool,
    dialog_sender: Sender<DialogResult>,
    dialog_receiver: Receiver<DialogResult>,
}
enum DialogResult {
    Upload(Vec<PathBuf>, String),
    Download(String, PathBuf),
    Cancelled,
}

impl Relay {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        #[allow(unused_mut)]
        let mut app = Self::from_context(&cc.egui_ctx);
        #[cfg(all(feature = "ghostty", target_os = "macos"))]
        if crate::ghostty::requested() {
            match crate::ghostty::Runtime::new(cc) {
                Ok(runtime) => {
                    app.ghostty_runtime = Some(runtime);
                    app.message = "Ghostty macOS prototype enabled.".into();
                    diagnostics::record(
                        "ghostty_runtime_started",
                        serde_json::json!({"version": "1.3.1"}),
                    );
                    if std::env::args().any(|arg| arg == "--ghostty-demo") {
                        app.local_ghostty_terminal(&cc.egui_ctx);
                    }
                }
                Err(error) => {
                    diagnostics::record("ghostty_runtime_failed", serde_json::json!({}));
                    app.message = error;
                }
            }
        }
        app
    }

    fn from_context(ctx: &egui::Context) -> Self {
        appearance::configure(ctx);
        let config_path = directories::ProjectDirs::from("app", "relay", "Relay")
            .map(|d| d.config_dir().join("connections.json"));
        let loaded = config_path
            .as_ref()
            .ok_or("Cannot locate the configuration directory".into())
            .and_then(|p| profiles::load(p));
        let config_error = loaded.is_err();
        let message = loaded.as_ref().err().cloned().unwrap_or_default();
        let profiles = loaded.unwrap_or_default();
        let (terminal_sender, terminal_events) = mpsc::channel();
        let (dialog_sender, dialog_receiver) = mpsc::channel();
        Self {
            icons: Icons::new(ctx),
            terminal_theme: appearance::terminal(),
            profiles,
            config_path,
            config_error,
            selected: None,
            draft: Profile::default(),
            connection_editor: None,
            search: String::new(),
            message,
            tabs: Vec::new(),
            #[cfg(all(feature = "ghostty", target_os = "macos"))]
            ghostty_runtime: None,
            active: None,
            next_id: 1,
            focus_terminal: false,
            terminal_events,
            terminal_sender,
            files: Worker::new(ctx.clone()),
            files_connected: false,
            files_busy: false,
            file_list_pending: BTreeSet::new(),
            file_worker_stopped: false,
            files_label: String::new(),
            secret: Zeroizing::new(String::new()),
            pending_profile: None,
            fingerprint: None,
            root: String::new(),
            destination: String::new(),
            tree: DirectoryTree::default(),
            tree_cache: TreeCache::default(),
            #[cfg(test)]
            painted_tree_rows: 0,
            file_search: String::new(),
            file_search_query: String::new(),
            file_search_expanded: BTreeMap::new(),
            expanded: BTreeSet::new(),
            selected_file: None,
            preview: None,
            preview_active: false,
            preview_inflight: None,
            resume_preview_after_connect: false,
            next_preview_id: 1,
            progress: None,
            file_panel: true,
            dialog_sender,
            dialog_receiver,
        }
    }

    fn save_profile(&mut self, profile: &Profile, index: Option<usize>) -> Result<(), String> {
        if self.config_error {
            return Err(
                "Resolve the configuration read error and restart before saving connections."
                    .into(),
            );
        }
        profile.ssh_args()?;
        let mut updated = self.profiles.clone();
        if let Some(index) = index {
            updated[index] = profile.clone();
        } else {
            updated.push(profile.clone());
        }
        let path = self
            .config_path
            .as_ref()
            .ok_or("Cannot locate the configuration directory")?;
        profiles::save(path, &updated)?;
        self.selected = Some(index.unwrap_or(updated.len() - 1));
        self.profiles = updated;
        self.draft = profile.clone();
        self.message = "Connection saved.".into();
        Ok(())
    }

    fn edit_connection(&mut self, index: Option<usize>) {
        self.connection_editor = Some(ConnectionEditor {
            profile: index.map(|i| self.profiles[i].clone()).unwrap_or_default(),
            index,
            error: String::new(),
            focus_name: true,
        });
    }

    fn connection_modal(&mut self, ctx: &egui::Context) {
        let Some(mut editor) = self.connection_editor.take() else {
            return;
        };
        let mut saved = false;
        let response = egui::Modal::new(egui::Id::new("connection_editor"))
            .area(
                egui::Modal::default_area(egui::Id::new("connection_editor")).anchor(
                    egui::Align2::CENTER_CENTER,
                    egui::vec2(
                        0.0,
                        if ctx.content_rect().height() < 640.0 {
                            0.0
                        } else {
                            -20.0
                        },
                    ),
                ),
            )
            .backdrop_color(Color32::from_white_alpha(24))
            .frame(egui::Frame::window(&ctx.style_of(egui::Theme::Light)).inner_margin(32.0))
            .show(ctx, |ui| {
                ui.set_width(464.0_f32.min(ctx.content_rect().width() - 96.0));
                ui.label(
                    RichText::new(if editor.index.is_some() {
                        "Edit connection"
                    } else {
                        "New connection"
                    })
                    .size(24.0)
                    .strong()
                    .variation("wght", 600.0),
                );
                ui.weak("Save a host to your workspace.");
                ui.add_space(18.0);
                egui::ScrollArea::vertical()
                    .scroll_bar_visibility(if ctx.content_rect().height() < 640.0 {
                        egui::scroll_area::ScrollBarVisibility::AlwaysVisible
                    } else {
                        egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded
                    })
                    .max_height((ctx.content_rect().height() - 320.0).max(100.0))
                    .show(ui, |ui| {
                        let name = field(ui, "Name", &mut editor.profile.name, "Production");
                        if editor.focus_name && !ui.is_sizing_pass() {
                            name.request_focus();
                            editor.focus_name = false;
                        }
                        ui.add_space(18.0);
                        field(
                            ui,
                            "Host / SSH alias",
                            &mut editor.profile.host,
                            "server.example.com",
                        );
                        ui.add_space(18.0);
                        let width = ui.available_width();
                        ui.horizontal_top(|ui| {
                            ui.allocate_ui_with_layout(
                                egui::vec2((width - 16.0) * 0.66, 0.0),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    field(
                                        ui,
                                        "Username",
                                        &mut editor.profile.user,
                                        "SSH config default",
                                    );
                                },
                            );
                            ui.add_space(8.0);
                            ui.allocate_ui_with_layout(
                                egui::vec2((width - 16.0) * 0.34, 0.0),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    field(ui, "Port", &mut editor.profile.port, "Default");
                                },
                            );
                        });
                        ui.add_space(18.0);
                        field(
                            ui,
                            "Private key",
                            &mut editor.profile.identity,
                            "Optional absolute path",
                        );
                        if !editor.error.is_empty() {
                            ui.add_space(8.0);
                            ui.colored_label(Color32::from_rgb(175, 37, 30), &editor.error);
                        }
                    });
                ui.add_space(18.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(appearance::primary("Save connection")).clicked() {
                            match self.save_profile(&editor.profile, editor.index) {
                                Ok(()) => saved = true,
                                Err(error) => editor.error = error,
                            }
                        }
                        if ui
                            .add_sized([88.0, 40.0], egui::Button::new("Cancel"))
                            .clicked()
                        {
                            ui.close();
                        }
                    });
                });
            });
        if !saved && !response.should_close() {
            self.connection_editor = Some(editor);
        }
    }
    fn terminal_connect(&mut self, ctx: &egui::Context) {
        match self.draft.ssh_args().and_then(|args| {
            TerminalBackend::new(
                self.next_id,
                ctx.clone(),
                self.terminal_sender.clone(),
                BackendSettings {
                    shell: "ssh".into(),
                    args,
                    working_directory: None,
                },
            )
            .map_err(|e| format!("Could not launch OpenSSH: {e}"))
        }) {
            Ok(backend) => {
                let id = self.next_id;
                diagnostics::record(
                    "terminal_started",
                    serde_json::json!({"tab_id": id, "child_pid": backend.pty_id()}),
                );
                self.next_id += 1;
                self.tabs.push(Tab {
                    id,
                    label: self.draft.label().into(),
                    backend,
                    ended: false,
                });
                self.active = Some(id);
                self.preview_active = false;
                self.focus_terminal = true;
                self.message = "SSH session started. Authentication and host-key prompts appear in the terminal.".into();
            }
            Err(error) => {
                diagnostics::record(
                    "terminal_start_failed",
                    serde_json::json!({"tab_id": self.next_id}),
                );
                self.message = error;
            }
        }
    }
    #[cfg(all(feature = "ghostty", target_os = "macos"))]
    fn local_ghostty_terminal(&mut self, ctx: &egui::Context) {
        let id = self.next_id;
        let settings = BackendSettings {
            shell: "/bin/zsh".into(),
            args: vec!["-f".into()],
            working_directory: None,
        };
        match TerminalBackend::new(id, ctx.clone(), self.terminal_sender.clone(), settings) {
            Ok(backend) => {
                self.tabs.push(Tab {
                    id,
                    label: format!("Local · Ghostty {id}"),
                    backend,
                    ended: false,
                });
                self.next_id += 1;
                self.active = Some(id);
                self.preview_active = false;
                self.focus_terminal = true;
                self.message =
                    "Local Ghostty demo. Open a saved host's terminal to try SSH.".into();
                diagnostics::record(
                    "ghostty_local_terminal_started",
                    serde_json::json!({"tab_id": id}),
                );
            }
            Err(error) => self.message = format!("Could not start Ghostty: {error}"),
        }
    }
    fn files_connect(&mut self, trusted: Option<String>, ctx: &egui::Context) {
        if self.file_worker_stopped {
            self.files = Worker::new(ctx.clone());
            self.file_worker_stopped = false;
        }
        self.file_list_pending.clear();
        let profile = if trusted.is_some() || self.resume_preview_after_connect {
            self.pending_profile
                .clone()
                .unwrap_or_else(|| self.draft.clone())
        } else {
            self.draft.clone()
        };
        if let Err(error) = profile.ssh_args() {
            self.message = error;
            return;
        }
        self.pending_profile = Some(profile.clone());
        if !self.resume_preview_after_connect {
            self.preview = None;
            self.preview_active = false;
        }
        // The old response is ignored by its request id when it arrives.
        self.files_busy = true;
        self.files_connected = false;
        self.tree.clear();
        self.tree_cache = TreeCache::default();
        self.file_search.clear();
        self.file_search_query.clear();
        self.file_search_expanded.clear();
        self.expanded.clear();
        self.selected_file = None;
        self.fingerprint = None;
        self.files_label = profile.label().into();
        self.message = format!("Opening SFTP connection to {}…", profile.host);
        self.send(Request::Connect {
            profile,
            secret: self.secret.clone(),
            trusted,
        });
    }
    fn send(&mut self, request: Request) {
        if self.files.sender.send(request).is_err() {
            self.files_busy = false;
            self.message = "The file worker stopped. Restart the app to reconnect.".into();
        }
    }
    fn refresh(&mut self, path: String) {
        if !self.file_list_pending.insert(path.clone()) {
            return;
        }
        self.tree.retry(&path);
        self.files_busy = true;
        if self.files.sender.send(Request::List(path.clone())).is_err() {
            self.file_list_pending.remove(&path);
            self.tree.fail(
                path,
                "The file worker stopped. Reconnect to try again.".into(),
            );
            self.files_busy = false;
        }
    }
    fn refresh_directory_listing(&mut self) {
        let mut expanded = self.expanded.clone();
        if let Some(matches) = self.filtered_tree_paths() {
            expanded.extend(matches.iter().cloned());
            for (path, open) in &self.file_search_expanded {
                if *open {
                    expanded.insert(path.clone());
                } else {
                    expanded.remove(path);
                }
            }
        }
        self.files_busy = true;
        self.message = "Refreshing directory listing…".into();
        self.send(Request::Refresh {
            root: self.root.clone(),
            expanded,
        });
    }

    fn apply_entries(&mut self, path: String, entries: Vec<Entry>) {
        if let Some(previous) = self.tree.get(&path) {
            let directories: HashSet<_> = entries
                .iter()
                .filter(|e| e.directory)
                .map(|e| e.path.as_str())
                .collect();
            let removed: HashSet<_> = previous
                .iter()
                .filter(|old| old.directory && !directories.contains(old.path.as_str()))
                .map(|e| e.path.clone())
                .collect();
            let removed_path = |candidate: &str| {
                let mut ancestor = candidate;
                loop {
                    if removed.contains(ancestor) {
                        return true;
                    }
                    match ancestor.rsplit_once('/') {
                        Some((parent, _)) => ancestor = parent,
                        None => return false,
                    }
                }
            };
            if !removed.is_empty() {
                self.tree.retain(|p, _| !removed_path(p));
                self.expanded.retain(|p| !removed_path(p));
                self.file_search_expanded.retain(|p, _| !removed_path(p));
                if removed_path(&self.destination) {
                    self.destination = path.clone();
                    self.selected_file = None;
                }
            }
        }
        if let Some(selected) = &self.selected_file
            && selected
                .path
                .rsplit_once('/')
                .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
                == Some(path.as_str())
        {
            self.selected_file = entries
                .iter()
                .find(|e| e.file && e.path == selected.path)
                .cloned();
        }
        self.tree.insert(path, entries);
    }
    fn transfer(&mut self, request: Request) {
        self.files.cancel.store(false, Ordering::Relaxed);
        self.files_busy = true;
        self.progress = None;
        self.send(request);
    }

    fn open_preview(&mut self, entry: &Entry) {
        if !entry.file {
            self.close_preview();
            self.message = "Select a regular file to preview its contents.".into();
            return;
        }
        self.preview = Some(FilePreview {
            id: self.next_preview_id,
            path: entry.path.clone(),
            name: entry.name.clone(),
            host: self.files_label.clone(),
            offset: 0,
            chunk: None,
            lines: vec![],
            binary: false,
            error: None,
        });
        self.next_preview_id += 1;
        self.preview_active = true;
        self.focus_terminal = false;
        self.pump_preview();
    }

    fn preview_page(&mut self, offset: u64) {
        if let Some(preview) = &mut self.preview {
            preview.id = self.next_preview_id;
            self.next_preview_id += 1;
            preview.offset = offset;
            preview.chunk = None;
            preview.lines.clear();
            preview.error = None;
        }
        self.pump_preview();
    }

    fn pump_preview(&mut self) {
        if self.preview_inflight.is_some() || self.files_busy || !self.files_connected {
            return;
        }
        if let Some(preview) = &mut self.preview
            && preview.chunk.is_none()
            && preview.error.is_none()
        {
            let request = Request::Preview {
                id: preview.id,
                remote: preview.path.clone(),
                offset: preview.offset,
            };
            if self.files.sender.send(request).is_ok() {
                self.preview_inflight = Some(preview.id);
            } else {
                preview.error = Some("The file worker stopped. Reconnect to read the file.".into());
            }
        }
    }

    fn close_preview(&mut self) {
        self.resume_preview_after_connect = false;
        self.preview = None;
        self.preview_active = false;
        self.focus_terminal = true;
        let _ = self.files.sender.send(Request::ClosePreview);
    }

    fn preview_ui(&mut self, ui: &mut egui::Ui) {
        let Some(preview) = &self.preview else {
            return;
        };
        ui.label(RichText::new(&preview.name).size(18.0).strong());
        ui.add(
            egui::Label::new(
                RichText::new(format!("{} · {}", preview.host, preview.path))
                    .color(appearance::MUTED),
            )
            .truncate(),
        )
        .on_hover_text(&preview.path);
        let mut offset = None;
        let mut reconnect = false;
        ui.horizontal_wrapped(|ui| {
            if !self.files_connected {
                reconnect = ui
                    .add_enabled(
                        !self.files_busy && self.fingerprint.is_none(),
                        egui::Button::new("Reconnect files"),
                    )
                    .clicked();
            }
            let ready = preview.chunk.is_some() || preview.error.is_some();
            if ui
                .add_enabled(
                    ready && !self.files_busy && self.files_connected && preview.offset > 0,
                    egui::Button::new("Previous"),
                )
                .clicked()
            {
                offset = Some(preview.offset.saturating_sub(CHUNK_BYTES as u64));
            }
            if ui
                .add_enabled(
                    !self.files_busy
                        && self.files_connected
                        && preview.chunk.as_ref().is_some_and(|c| c.has_next()),
                    egui::Button::new("Next"),
                )
                .clicked()
            {
                offset = Some(preview.offset.saturating_add(CHUNK_BYTES as u64));
            }
            if ui
                .add_enabled(
                    ready && !self.files_busy && self.files_connected,
                    self.icons.button("refresh", "Reload"),
                )
                .clicked()
            {
                offset = Some(0);
            }
            ui.weak("Read only · streamed on demand");
        });
        if !self.files_connected
            && preview.chunk.is_some()
            && let Some(error) = &preview.error
        {
            ui.colored_label(Color32::from_rgb(175, 37, 30), error);
        }
        if let Some(chunk) = &preview.chunk {
            let end = (chunk.offset + chunk.bytes.len().min(CHUNK_BYTES) as u64).min(chunk.size);
            ui.small(format!(
                "Bytes {}–{} of {}",
                chunk.offset,
                end,
                bytes(chunk.size)
            ));
            if preview.binary {
                ui.weak("Binary content · hexadecimal view");
            }
            ui.separator();
            if chunk.bytes.is_empty() {
                ui.weak(if chunk.offset == 0 {
                    "This file is empty."
                } else {
                    "End of file. Reload if it has changed."
                });
            } else {
                ui.spacing_mut().item_spacing.y = 2.0;
                let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
                egui::ScrollArea::both()
                    .id_salt(("file_preview", preview.id))
                    .auto_shrink([false, false])
                    .show_rows(ui, row_height, preview.lines.len(), |ui, rows| {
                        for row in rows {
                            ui.add(
                                egui::Label::new(RichText::new(&preview.lines[row]).monospace())
                                    .selectable(true)
                                    .extend(),
                            );
                        }
                    });
            }
        } else if let Some(error) = &preview.error {
            ui.colored_label(Color32::from_rgb(175, 37, 30), error);
        } else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(if self.files_busy {
                    "Waiting for the file operation…"
                } else {
                    "Reading from the remote host…"
                });
            });
        }
        if let Some(offset) = offset {
            self.preview_page(offset);
        }
        if reconnect {
            self.reconnect_preview(ui.ctx());
        }
    }

    fn reconnect_preview(&mut self, ctx: &egui::Context) {
        // Reconnect the host that owned this preview, even if another saved host is selected.
        self.resume_preview_after_connect = self.preview.is_some();
        self.files_connect(None, ctx);
    }

    fn drain_events(&mut self) {
        for (id, event) in self.terminal_events.try_iter() {
            if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
                match event {
                    PtyEvent::ChildExit(status) => {
                        diagnostics::record(
                            "terminal_process_exited",
                            serde_json::json!({"tab_id": id, "status": status.to_string(), "code": status.code(), "success": status.success()}),
                        );
                        tab.ended = true;
                        self.message = format!(
                            "SSH session ended ({status}). Its terminal output is kept in the tab."
                        );
                    }
                    PtyEvent::Exit => {
                        diagnostics::record(
                            "terminal_pty_exited",
                            serde_json::json!({"tab_id": id}),
                        );
                        if !tab.ended {
                            self.message =
                                "SSH session ended. Its terminal output is kept in the tab.".into();
                        }
                        tab.ended = true;
                    }
                    _ => {}
                }
            }
        }
        loop {
            let event = match self.files.receiver.try_recv() {
                Ok(event) => event,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if !self.file_worker_stopped {
                        self.file_worker_stopped = true;
                        diagnostics::record("sftp_worker_channel_closed", serde_json::json!({}));
                        for path in std::mem::take(&mut self.file_list_pending) {
                            self.tree.fail(
                                path,
                                "The file worker stopped. Reconnect to try again.".into(),
                            );
                        }
                        self.files_busy = false;
                        self.files_connected = false;
                        self.preview_inflight = None;
                        self.message =
                            "The file worker stopped. Reconnect the file browser to continue."
                                .into();
                    }
                    break;
                }
            };
            match event {
                Event::Disconnected(message) => {
                    self.files_connected = false;
                    self.files_busy = false;
                    self.progress = None;
                    self.preview_inflight = None;
                    for path in std::mem::take(&mut self.file_list_pending) {
                        self.tree.fail(path, message.clone());
                    }
                    if let Some(preview) = &mut self.preview {
                        preview.error = Some(message.clone());
                    }
                    self.message = message;
                }
                Event::Preview { id, result } => {
                    if self.preview_inflight == Some(id) {
                        self.preview_inflight = None;
                    }
                    if let Some(preview) = &mut self.preview
                        && preview.id == id
                    {
                        preview.accept(result);
                    }
                }
                Event::Connected(home) => {
                    self.files_connected = true;
                    self.files_busy = false;
                    self.root = home.clone();
                    self.destination = home.clone();
                    self.expanded.insert(home.clone());
                    self.refresh(home);
                    self.secret = Zeroizing::new(String::new());
                    self.message = format!("SFTP connected: {}", self.files_label);
                    if self.resume_preview_after_connect {
                        self.resume_preview_after_connect = false;
                        if let Some(preview) = &mut self.preview {
                            preview.id = self.next_preview_id;
                            self.next_preview_id += 1;
                            preview.chunk = None;
                            preview.lines.clear();
                            preview.error = None;
                        }
                    }
                }
                Event::Trust(fingerprint) => {
                    self.files_busy = false;
                    self.fingerprint = Some(fingerprint);
                    if self.resume_preview_after_connect
                        && let Some(preview) = &mut self.preview
                    {
                        preview.error = Some(
                            "Verify the server fingerprint in Remote files to reconnect.".into(),
                        );
                    }
                }
                Event::Entries(path, entries) => {
                    self.file_list_pending.remove(&path);
                    self.files_busy = !self.file_list_pending.is_empty();
                    self.apply_entries(path, entries);
                }
                Event::ListFailed { path, error } => {
                    self.file_list_pending.remove(&path);
                    self.files_busy = !self.file_list_pending.is_empty();
                    self.message = format!("Could not load {path}: {error}");
                    self.tree.fail(path, error);
                }
                Event::Refreshed(listings) => {
                    self.files_busy = !self.file_list_pending.is_empty();
                    let mut errors = vec![];
                    for (path, result) in listings {
                        match result {
                            Ok(entries) => self.apply_entries(path, entries),
                            Err(error) => {
                                errors.push(format!("{path}: {error}"));
                                self.tree.fail(path, error);
                            }
                        }
                    }
                    self.message = if errors.is_empty() {
                        "Directory listing refreshed.".into()
                    } else {
                        format!("Could not refresh {}", errors.join("; "))
                    };
                }
                Event::Progress { name, done, total } => self.progress = Some((name, done, total)),
                Event::Done(message) => {
                    self.files_busy = false;
                    self.progress = None;
                    self.message = message;
                    self.refresh(self.destination.clone());
                }
                Event::Error(message) => {
                    self.files_busy = false;
                    self.progress = None;
                    if self.resume_preview_after_connect
                        && let Some(preview) = &mut self.preview
                    {
                        preview.error = Some(message.clone());
                    }
                    self.message = message;
                }
            }
        }
        while let Ok(result) = self.dialog_receiver.try_recv() {
            match result {
                DialogResult::Upload(local, remote) => {
                    self.transfer(Request::Upload { local, remote })
                }
                DialogResult::Download(remote, local) => {
                    self.transfer(Request::Download { remote, local })
                }
                DialogResult::Cancelled => self.files_busy = false,
            }
        }
        self.pump_preview();
    }
    fn host_panel(&mut self, ui: &mut egui::Ui) {
        let compact = ui.available_height() < 560.0;
        let footer_height = if self.selected.is_none() {
            65.0
        } else if compact {
            165.0
        } else {
            270.0
        };
        ui.label(
            RichText::new("Connections")
                .size(18.0)
                .strong()
                .variation("wght", 600.0),
        );
        ui.add_space(8.0);
        egui::Frame::new()
            .fill(Color32::WHITE)
            .stroke(egui::Stroke::new(1.0, appearance::BORDER))
            .corner_radius(6)
            .inner_margin(8.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(self.icons.image("search", 17.0, appearance::MUTED));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .frame(egui::Frame::NONE)
                            .hint_text("Find a host…")
                            .desired_width(ui.available_width()),
                    );
                });
            });
        ui.add_space(10.0);
        egui::ScrollArea::vertical()
            .id_salt("hosts")
            .max_height((ui.available_height() - footer_height - 70.0).max(48.0))
            .show(ui, |ui| {
                let filter = self.search.to_lowercase();
                let mut shown = 0;
                for (index, profile) in self.profiles.iter().enumerate() {
                    if !format!("{} {}", profile.label(), profile.host)
                        .to_lowercase()
                        .contains(&filter)
                    {
                        continue;
                    }
                    shown += 1;
                    let selected = self.selected == Some(index);
                    let row = egui::Frame::new()
                        .fill(if selected {
                            appearance::SELECTION
                        } else {
                            Color32::TRANSPARENT
                        })
                        .corner_radius(6)
                        .inner_margin(10.0)
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.add(self.icons.image("server", 22.0, appearance::MUTED));
                                ui.vertical(|ui| {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(profile.label())
                                                .strong()
                                                .variation("wght", 600.0),
                                        )
                                        .truncate(),
                                    );
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&profile.host).color(appearance::MUTED),
                                        )
                                        .truncate(),
                                    );
                                });
                            });
                        });
                    if ui
                        .interact(
                            row.response.rect,
                            ui.id().with(("host", index)),
                            egui::Sense::click(),
                        )
                        .clicked()
                    {
                        self.selected = Some(index);
                        self.draft = profile.clone();
                    }
                }
                if shown == 0 {
                    ui.weak(if self.profiles.is_empty() {
                        "Your saved hosts will appear here."
                    } else {
                        "No matching connections."
                    });
                }
            });
        ui.add_space(6.0);
        if ui
            .add_sized(
                [ui.available_width(), 36.0],
                self.icons.button("plus", "New connection"),
            )
            .clicked()
        {
            self.edit_connection(None);
        }
        // Keep connection actions in the lower inspector, as in the selected mockup.
        ui.add_space((ui.available_height() - footer_height).max(16.0));
        ui.separator();
        if self.selected.is_some() {
            ui.label(
                RichText::new(self.draft.label())
                    .size(16.0)
                    .strong()
                    .variation("wght", 600.0),
            );
            if !compact {
                egui::Grid::new("selected_host_details")
                    .min_col_width(45.0)
                    .max_col_width(ui.available_width() - 60.0)
                    .spacing([10.0, 6.0])
                    .show(ui, |ui| {
                        ui.spacing_mut().interact_size.y = 18.0;
                        for (label, value) in [
                            ("Host", self.draft.host.as_str()),
                            (
                                "User",
                                if self.draft.user.is_empty() {
                                    "SSH config default"
                                } else {
                                    &self.draft.user
                                },
                            ),
                            (
                                "Port",
                                if self.draft.port.is_empty() {
                                    "SSH config default"
                                } else {
                                    &self.draft.port
                                },
                            ),
                            (
                                "Identity",
                                if self.draft.identity.is_empty() {
                                    "SSH agent / config"
                                } else {
                                    &self.draft.identity
                                },
                            ),
                        ] {
                            ui.weak(label);
                            ui.add(
                                egui::Label::new(RichText::new(value).color(appearance::MUTED))
                                    .truncate(),
                            )
                            .on_hover_text(value);
                            ui.end_row();
                        }
                    });
            } else {
                ui.add(
                    egui::Label::new(RichText::new(&self.draft.host).color(appearance::MUTED))
                        .truncate(),
                );
            }
            ui.horizontal(|ui| {
                if ui.small_button("Edit…").clicked() {
                    self.edit_connection(self.selected);
                }
                if ui
                    .add_enabled(!self.config_error, egui::Button::new("Remove").small())
                    .clicked()
                    && let (Some(index), Some(path)) = (self.selected, &self.config_path)
                {
                    let mut updated = self.profiles.clone();
                    updated.remove(index);
                    match profiles::save(path, &updated) {
                        Ok(()) => {
                            self.profiles = updated;
                            self.selected = None;
                            self.draft = Profile::default();
                        }
                        Err(e) => self.message = e,
                    }
                }
            });
        }
        ui.add_enabled_ui(self.selected.is_some(), |ui| {
            let button = egui::Button::image_and_text(
                self.icons.image("terminal", 17.0, Color32::WHITE),
                RichText::new("Open terminal").color(Color32::WHITE),
            )
            .fill(appearance::BLUE)
            .stroke(egui::Stroke::NONE);
            if ui.add_sized([ui.available_width(), 38.0], button).clicked() {
                self.terminal_connect(ui.ctx());
            }
        });
    }
    fn file_panel_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Remote files")
                    .size(18.0)
                    .strong()
                    .variation("wght", 600.0),
            );
            if self.files_busy {
                ui.spinner();
            }
        });
        if !self.files_connected {
            ui.label("Browse a host and drop files or folders to upload them.");
            ui.weak("Select a connection on the left to browse its files.");
            ui.label("Password / key passphrase (optional)");
            ui.add(
                egui::TextEdit::singleline(&mut *self.secret)
                    .password(true)
                    .hint_text("Try SSH agent and keys first")
                    .desired_width(f32::INFINITY),
            );
            if ui
                .add_enabled(
                    self.selected.is_some() && !self.files_busy && self.fingerprint.is_none(),
                    appearance::primary("Connect files"),
                )
                .clicked()
            {
                self.files_connect(None, ui.ctx());
            }
            if let Some(fingerprint) = self.fingerprint.clone() {
                ui.separator();
                ui.label("Unknown host key");
                ui.label(
                    "Verify this fingerprint with the server administrator before trusting it.",
                );
                ui.monospace(&fingerprint);
                ui.horizontal(|ui| {
                    if ui.button("Trust this session").clicked() {
                        self.files_connect(Some(fingerprint), ui.ctx());
                    }
                    if ui.button("Cancel").clicked() {
                        self.fingerprint = None;
                        self.secret = Zeroizing::new(String::new());
                    }
                });
            }
            return;
        }
        ui.add_space(6.0);
        let path = egui::Button::new(&self.destination).fill(appearance::SIDEBAR);
        if ui
            .add_sized([ui.available_width(), 38.0], path)
            .on_hover_text(format!(
                "{} · Select the root folder {}",
                self.files_label, self.root
            ))
            .clicked()
        {
            self.destination = self.root.clone();
            self.selected_file = None;
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !self.files_busy,
                    egui::Button::image(self.icons.image("arrow-up", 16.0, appearance::MUTED)),
                )
                .on_hover_text("Go to parent folder")
                .clicked()
            {
                let parent = self
                    .root
                    .trim_end_matches('/')
                    .rsplit_once('/')
                    .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
                    .unwrap_or("/")
                    .to_owned();
                self.root = parent.clone();
                self.destination = parent.clone();
                self.expanded.insert(parent.clone());
                self.refresh(parent);
            }
            if ui
                .add_enabled(!self.files_busy, self.icons.button("refresh", "Refresh"))
                .on_hover_text("Reload the root and all expanded folders from the remote host")
                .clicked()
            {
                self.refresh_directory_listing();
            }
            if ui.add(egui::Button::new("Disconnect").small()).clicked() {
                self.files.cancel.store(true, Ordering::Relaxed);
                self.files = Worker::new(ui.ctx().clone());
                self.file_list_pending.clear();
                self.file_worker_stopped = false;
                self.files_busy = false;
                self.preview = None;
                self.preview_active = false;
                self.preview_inflight = None;
                self.files_connected = false;
                self.resume_preview_after_connect = false;
                self.tree.clear();
                self.tree_cache = TreeCache::default();
                self.file_search.clear();
                self.file_search_query.clear();
                self.file_search_expanded.clear();
                self.selected_file = None;
            }
        });
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.file_search)
                    .id_salt("file_search")
                    .hint_text("Search loaded files…")
                    .desired_width((ui.available_width() - 80.0).max(40.0)),
            )
            .on_hover_text("Filter loaded file and folder names. Searches only this tree, without remote requests.");
            if ui.add_enabled(!self.file_search.is_empty(), egui::Button::new("Clear").small()).clicked() {
                self.file_search.clear();
            }
        });
        ui.small("Name");
        ui.separator();
        let height =
            (ui.available_height() - if self.progress.is_some() { 180.0 } else { 90.0 }).max(80.0);
        ui.add_enabled_ui(!self.files_busy, |ui| self.tree_ui(ui, height));
        ui.separator();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.files_busy, self.icons.button("upload", "Upload"))
                .clicked()
            {
                self.files_busy = true;
                let sender = self.dialog_sender.clone();
                let ctx = ui.ctx().clone();
                let remote = self.destination.clone();
                std::thread::spawn(move || {
                    let result = rfd::FileDialog::new()
                        .set_title("Upload files")
                        .pick_files()
                        .map(|paths| DialogResult::Upload(paths, remote))
                        .unwrap_or(DialogResult::Cancelled);
                    let _ = sender.send(result);
                    ctx.request_repaint();
                });
            }
            let can_download =
                !self.files_busy && self.selected_file.as_ref().is_some_and(|f| f.file);
            if ui
                .add_enabled(can_download, self.icons.button("download", "Download"))
                .clicked()
                && let Some(entry) = self.selected_file.clone()
            {
                self.files_busy = true;
                let sender = self.dialog_sender.clone();
                let ctx = ui.ctx().clone();
                std::thread::spawn(move || {
                    let result = rfd::FileDialog::new()
                        .set_file_name(&entry.name)
                        .save_file()
                        .map(|path| DialogResult::Download(entry.path, path))
                        .unwrap_or(DialogResult::Cancelled);
                    let _ = sender.send(result);
                    ctx.request_repaint();
                });
            }
        });
        ui.small("Drop files into the selected folder")
            .on_hover_text(
                "Files and folders are uploaded to the selected folder. Existing files are kept.",
            );
        if let Some((name, done, total)) = &self.progress {
            ui.label(name);
            let fraction = if *total == 0 {
                0.0
            } else {
                *done as f32 / *total as f32
            };
            ui.add(egui::ProgressBar::new(fraction).text(format!(
                "{} / {}",
                bytes(*done),
                bytes(*total)
            )));
            if ui.button("Cancel transfer").clicked() {
                self.files.cancel.store(true, Ordering::Relaxed);
            }
        }
    }
    fn filtered_tree_paths(&mut self) -> Option<std::sync::Arc<BTreeSet<String>>> {
        let query = self.file_search.trim().to_lowercase();
        if self.file_search_query != query {
            self.file_search_expanded.clear();
            self.file_search_query = query;
            self.tree_cache.invalidate_rows();
        }
        self.tree_cache
            .matches(&self.tree, &self.root, &self.file_search_query)
    }

    fn tree_ui(&mut self, ui: &mut egui::Ui, height: f32) {
        self.filtered_tree_paths();
        let rows = self.tree_cache.rows(
            &self.tree,
            &self.root,
            &self.file_search_query,
            &self.expanded,
            &self.file_search_expanded,
        );
        let row_height = ui.spacing().interact_size.y.max(
            ui.text_style_height(&egui::TextStyle::Button).max(20.0)
                + 2.0 * ui.spacing().button_padding.y,
        );
        let mut scroll = egui::ScrollArea::both()
            .id_salt("remote_tree")
            .auto_shrink([false, false])
            .max_height(height);
        if self.tree_cache.take_scroll_reset() {
            scroll = scroll.vertical_scroll_offset(0.0);
        }
        #[cfg(test)]
        {
            self.painted_tree_rows = 0;
        }
        scroll.show_rows(ui, row_height, rows.len(), |ui, range| {
            for index in range {
                #[cfg(test)]
                {
                    self.painted_tree_rows += 1;
                }
                match &rows[index] {
                    TreeRow::Error { depth, path, error } => {
                        ui.push_id(("directory_error", path), |ui| {
                            ui.horizontal(|ui| {
                                ui.set_min_height(row_height);
                                ui.add_space(*depth as f32 * 12.0);
                                if ui
                                    .add_enabled(
                                        !self.files_busy,
                                        egui::Button::new("Retry").small(),
                                    )
                                    .clicked()
                                {
                                    self.refresh(path.clone());
                                }
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(error.as_str())
                                            .color(Color32::from_rgb(175, 37, 30)),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(error.as_str());
                            });
                        });
                    }
                    TreeRow::Entry {
                        directory,
                        index,
                        depth,
                        expanded,
                    } => {
                        let entry = &directory.entries[*index];
                        ui.push_id(&entry.path, |ui| {
                            self.tree_entry_ui(ui, entry, *depth, *expanded, row_height)
                        });
                    }
                    TreeRow::Message { depth, text } => {
                        ui.horizontal(|ui| {
                            ui.set_min_height(row_height);
                            ui.add_space(*depth as f32 * 12.0);
                            ui.weak(*text);
                        });
                    }
                }
            }
        });
    }

    fn tree_entry_ui(
        &mut self,
        ui: &mut egui::Ui,
        entry: &Entry,
        depth: usize,
        expanded: bool,
        row_height: f32,
    ) {
        ui.horizontal(|ui| {
            ui.set_min_height(row_height);
            ui.add_space((depth + 1) as f32 * 12.0);
            let name = if entry.name.contains(['\n', '\r']) {
                std::borrow::Cow::Owned(entry.name.replace(['\n', '\r'], "⏎"))
            } else {
                std::borrow::Cow::Borrowed(entry.name.as_str())
            };
            if entry.directory {
                if ui
                    .add(
                        egui::Button::image(self.icons.image(
                            if expanded {
                                "chevron-down"
                            } else {
                                "chevron-right"
                            },
                            14.0,
                            appearance::MUTED,
                        ))
                        .frame(false),
                    )
                    .on_hover_text("Expand or collapse folder")
                    .clicked()
                {
                    if !self.file_search_query.is_empty() {
                        self.file_search_expanded
                            .insert(entry.path.clone(), !expanded);
                        if !expanded && !self.tree.contains_key(&entry.path) {
                            self.refresh(entry.path.clone());
                        }
                    } else if expanded {
                        self.expanded.remove(&entry.path);
                    } else {
                        self.expanded.insert(entry.path.clone());
                        self.refresh(entry.path.clone());
                    }
                    self.tree_cache.invalidate_rows();
                }
                if ui
                    .add(
                        egui::Button::image_and_text(
                            self.icons.image("folder", 20.0, appearance::BLUE),
                            name.as_ref(),
                        )
                        .wrap_mode(egui::TextWrapMode::Extend)
                        .frame(false)
                        .selected(self.destination == entry.path),
                    )
                    .clicked()
                {
                    self.destination = entry.path.clone();
                    self.selected_file = None;
                }
            } else {
                let label = if entry.file {
                    name
                } else {
                    std::borrow::Cow::Owned(format!("{name} (link / special)"))
                };
                if ui
                    .add(
                        egui::Button::image_and_text(
                            self.icons.image("file", 19.0, appearance::MUTED),
                            label.as_ref(),
                        )
                        .wrap_mode(egui::TextWrapMode::Extend)
                        .frame(false)
                        .selected(
                            self.selected_file
                                .as_ref()
                                .is_some_and(|f| f.path == entry.path),
                        ),
                    )
                    .on_hover_ui(|ui| {
                        ui.label(format!("{} · {}", entry.path, bytes(entry.size)));
                    })
                    .clicked()
                {
                    self.selected_file = Some(entry.clone());
                    self.destination = entry
                        .path
                        .rsplit_once('/')
                        .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
                        .unwrap_or("/")
                        .into();
                    self.open_preview(entry);
                }
            }
        });
    }
}

impl eframe::App for Relay {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.render(ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        diagnostics::record(
            "app_shutdown",
            serde_json::json!({"tabs": self.tabs.len(), "live_sessions": self.tabs.iter().filter(|tab| !tab.ended).count()}),
        );
    }
}

impl Relay {
    fn render(&mut self, ui: &mut egui::Ui) {
        #[cfg(all(feature = "ghostty", target_os = "macos"))]
        crate::ghostty::tick();
        if ui.ctx().input(|input| input.viewport().close_requested()) {
            diagnostics::record(
                "window_close_requested",
                serde_json::json!({"tabs": self.tabs.len()}),
            );
        }
        self.drain_events();
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(&self.message)
                            .small()
                            .color(appearance::MUTED),
                    )
                    .truncate(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.toggle_value(&mut self.file_panel, "Files");
                    #[cfg(all(feature = "ghostty", target_os = "macos"))]
                    if self.ghostty_runtime.is_some() {
                        ui.small("Ghostty prototype");
                        if ui.small_button("Local terminal").clicked() {
                            self.local_ghostty_terminal(ui.ctx());
                        }
                    }
                    if let Some(path) = &self.config_path {
                        ui.small("Local storage").on_hover_ui(|ui| {
                            ui.label(path.display().to_string());
                            if let Some(log) = diagnostics::path() {
                                ui.label(format!("Diagnostics: {}", log.display()));
                            }
                        });
                    }
                });
            });
        });
        egui::Panel::left("connections")
            .resizable(true)
            .default_size(280.0)
            .size_range(220.0..=360.0)
            .frame(
                egui::Frame::new()
                    .fill(appearance::SIDEBAR)
                    .inner_margin(14.0),
            )
            .show(ui, |ui| self.host_panel(ui));
        if self.file_panel {
            egui::Panel::right("files")
                .resizable(true)
                .default_size(350.0)
                .size_range(270.0..=600.0)
                .frame(egui::Frame::new().fill(Color32::WHITE).inner_margin(14.0))
                .show(ui, |ui| self.file_panel_ui(ui));
        }
        let dropped: Vec<_> = ui.ctx().input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if !dropped.is_empty() && self.connection_editor.is_none() {
            if !self.files_connected {
                self.message = "Connect the file browser before dropping files.".into();
            } else if self.files_busy {
                self.message =
                    "Wait for the current file operation before uploading more files.".into();
            } else if self.file_panel {
                self.transfer(Request::Upload {
                    local: dropped,
                    remote: self.destination.clone(),
                });
            } else {
                self.message = "Drop files onto the Remote files panel to upload them.".into();
            }
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(Color32::WHITE).inner_margin(10.0))
            .show(ui, |ui| {
                let mut close = None;
                ui.horizontal_wrapped(|ui| {
                    for tab in &self.tabs {
                        let label =
                            format!("{}{}", tab.label, if tab.ended { " · ended" } else { "" });
                        if ui
                            .add(
                                self.icons
                                    .button("terminal", &label)
                                    .selected(!self.preview_active && self.active == Some(tab.id)),
                            )
                            .clicked()
                        {
                            self.active = Some(tab.id);
                            self.preview_active = false;
                            self.focus_terminal = true;
                        }
                        if ui
                            .add(
                                egui::Button::image(self.icons.image("x", 14.0, appearance::MUTED))
                                    .frame(false),
                            )
                            .on_hover_text("Close SSH session")
                            .clicked()
                        {
                            close = Some(tab.id);
                        }
                    }
                    if let Some(preview) = &self.preview {
                        if ui
                            .add(
                                self.icons
                                    .button("file", &preview.name)
                                    .selected(self.preview_active),
                            )
                            .on_hover_text(&preview.path)
                            .clicked()
                        {
                            self.preview_active = true;
                        }
                        if ui
                            .add(
                                egui::Button::image(self.icons.image("x", 14.0, appearance::MUTED))
                                    .frame(false),
                            )
                            .on_hover_text("Close file preview")
                            .clicked()
                        {
                            self.close_preview();
                        }
                    }
                });
                if let Some(id) = close {
                    diagnostics::record("terminal_close_button", serde_json::json!({"tab_id": id}));
                    self.tabs.retain(|tab| tab.id != id);
                    if self.active == Some(id) {
                        self.active = self.tabs.last().map(|tab| tab.id);
                    }
                }
                ui.separator();
                if self.preview_active {
                    self.preview_ui(ui);
                } else if let Some(tab) =
                    self.tabs.iter_mut().find(|tab| Some(tab.id) == self.active)
                {
                    let modal_open = self.connection_editor.is_some();
                    let focus = !modal_open && std::mem::take(&mut self.focus_terminal);
                    ui.add_enabled_ui(!modal_open, |ui| {
                        ui.set_opacity(1.0);
                        let terminal = TerminalView::new(ui, &mut tab.backend)
                            .set_theme(self.terminal_theme.clone())
                            .set_focus(focus);
                        ui.add(terminal);
                    });
                } else {
                    ui.vertical_centered(|ui| {
                        ui.add_space(ui.available_height() * 0.26);
                        ui.label(RichText::new("Your servers. One quiet workspace.").size(24.0));
                        ui.add_space(12.0);
                        ui.weak("Choose New connection to add a host, then open its terminal.");
                        ui.weak("Connect the file browser to explore folders and transfer files.");
                    });
                }
            });
        for tab in &mut self.tabs {
            tab.backend
                .set_visible(!self.preview_active && self.active == Some(tab.id));
        }
        self.connection_modal(ui.ctx());
    }
}
fn field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) -> egui::Response {
    ui.label(RichText::new(label).variation("wght", 600.0));
    ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .margin(egui::vec2(12.0, 10.0))
            .desired_width(f32::INFINITY),
    )
}
fn bytes(size: u64) -> String {
    if size >= 1024 * 1024 * 1024 {
        format!("{:.1} GiB", size as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if size >= 1024 * 1024 {
        format!("{:.1} MiB", size as f64 / (1024.0 * 1024.0))
    } else if size >= 1024 {
        format!("{:.1} KiB", size as f64 / 1024.0)
    } else {
        format!("{size} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::AtomicBool};

    #[test]
    fn disconnected_preview_reconnects_original_host_and_resumes_at_same_offset() {
        let ctx = egui::Context::default();
        let mut app = Relay::from_context(&ctx);
        let (sender, requests) = mpsc::channel();
        let (events, receiver) = mpsc::channel();
        app.files = Worker {
            sender,
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        app.pending_profile = Some(Profile {
            host: "original.example".into(),
            ..Default::default()
        });
        app.draft = Profile {
            host: "another.example".into(),
            ..Default::default()
        };
        app.files_connected = true;
        app.preview_active = true;
        app.preview_inflight = Some(7);
        app.next_preview_id = 8;
        app.preview = Some(FilePreview {
            id: 7,
            path: "/remote/log.txt".into(),
            name: "log.txt".into(),
            host: "original.example".into(),
            offset: CHUNK_BYTES as u64,
            chunk: None,
            lines: vec![],
            binary: false,
            error: None,
        });
        events
            .send(Event::Disconnected(
                "The file connection was lost. Reconnect files to continue.".into(),
            ))
            .unwrap();
        app.drain_events();
        assert!(!app.files_connected);
        assert!(app.preview_inflight.is_none());
        assert!(requests.try_recv().is_err());
        click_label(&mut app, &ctx, "Reconnect files");
        assert!(
            matches!(requests.try_recv().unwrap(), Request::Connect { profile, trusted: None, .. } if profile.host == "original.example")
        );
        assert!(app.resume_preview_after_connect);
        events.send(Event::Trust("SHA256:fixture".into())).unwrap();
        app.drain_events();
        app.files_connect(Some("SHA256:fixture".into()), &ctx);
        assert!(
            matches!(requests.try_recv().unwrap(), Request::Connect { profile, trusted: Some(fingerprint), .. } if profile.host == "original.example" && fingerprint == "SHA256:fixture")
        );
        assert_eq!(app.preview.as_ref().unwrap().offset, CHUNK_BYTES as u64);
        events.send(Event::Connected("/remote".into())).unwrap();
        app.drain_events();
        assert!(matches!(requests.try_recv().unwrap(), Request::List(path) if path == "/remote"));
        events
            .send(Event::Entries("/remote".into(), vec![]))
            .unwrap();
        app.drain_events();
        assert!(
            matches!(requests.try_recv().unwrap(), Request::Preview { id: 8, remote, offset } if remote == "/remote/log.txt" && offset == CHUNK_BYTES as u64)
        );
        events
            .send(Event::Preview {
                id: 7,
                result: Err("stale response".into()),
            })
            .unwrap();
        app.drain_events();
        assert!(app.preview.as_ref().unwrap().error.is_none());
        assert_eq!(app.preview_inflight, Some(8));
    }

    #[test]
    fn failed_folder_shows_error_and_retry_recovers_without_repeated_requests() {
        let ctx = egui::Context::default();
        let mut app = Relay::from_context(&ctx);
        let (sender, requests) = mpsc::channel();
        let (events, receiver) = mpsc::channel();
        app.files = Worker {
            sender,
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        app.files_connected = true;
        app.root = "/remote".into();
        app.expanded.insert("/remote/folder".into());
        app.tree.insert(
            "/remote".into(),
            vec![Entry {
                path: "/remote/folder".into(),
                name: "folder".into(),
                directory: true,
                file: false,
                size: 0,
            }],
        );
        app.refresh("/remote/folder".into());
        app.refresh("/remote/folder".into());
        assert!(
            matches!(requests.try_recv().unwrap(), Request::List(path) if path == "/remote/folder")
        );
        assert!(requests.try_recv().is_err());
        events
            .send(Event::ListFailed {
                path: "/remote/folder".into(),
                error: "Permission denied".into(),
            })
            .unwrap();
        app.drain_events();
        assert!(!app.files_busy);
        for _ in 0..3 {
            render_input(&mut app, &ctx, vec![]);
        }
        let rows = app
            .tree_cache
            .rows(&app.tree, &app.root, "", &app.expanded, &BTreeMap::new());
        assert!(
            rows.iter()
                .any(|row| matches!(row, TreeRow::Error { path, .. } if path == "/remote/folder"))
        );
        assert!(!rows.iter().any(|row| matches!(
            row,
            TreeRow::Message {
                text: "Loading…",
                ..
            }
        )));
        assert!(requests.try_recv().is_err());
        click_label(&mut app, &ctx, "Retry");
        assert!(
            matches!(requests.try_recv().unwrap(), Request::List(path) if path == "/remote/folder")
        );
        assert!(app.files_busy);
        events
            .send(Event::Entries("/remote/folder".into(), vec![]))
            .unwrap();
        app.drain_events();
        let rows = app
            .tree_cache
            .rows(&app.tree, &app.root, "", &app.expanded, &BTreeMap::new());
        assert!(rows.iter().any(|row| matches!(
            row,
            TreeRow::Message {
                text: "Empty folder",
                ..
            }
        )));
        assert!(!rows.iter().any(|row| matches!(row, TreeRow::Error { .. })));
        // A later refresh failure should also replace Loading with an error.
        events
            .send(Event::Refreshed(vec![(
                "/remote/folder".into(),
                Err("Timed out".into()),
            )]))
            .unwrap();
        app.drain_events();
        let rows = app
            .tree_cache
            .rows(&app.tree, &app.root, "", &app.expanded, &BTreeMap::new());
        assert!(rows.iter().any(
            |row| matches!(row, TreeRow::Error { error, .. } if error.as_str() == "Timed out")
        ));
    }

    #[test]
    fn stopped_file_worker_clears_pending_listing_and_allows_reconnect() {
        let ctx = egui::Context::default();
        let mut app = Relay::from_context(&ctx);
        let (sender, _requests) = mpsc::channel();
        let (events, receiver) = mpsc::channel();
        app.files = Worker {
            sender,
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        app.files_connected = true;
        app.refresh("/remote".into());
        drop(events);
        app.drain_events();
        assert!(app.file_worker_stopped);
        assert!(!app.files_busy);
        assert!(!app.files_connected);
        assert!(app.file_list_pending.is_empty());
        assert!(app.message.contains("Reconnect"));
    }

    #[cfg(all(unix, feature = "terminal-fixture"))]
    #[test]
    fn ssh_exit_preserves_tab_output_and_reports_exit_status() {
        use std::os::unix::process::ExitStatusExt;
        let ctx = egui::Context::default();
        let mut app = Relay::from_context(&ctx);
        let mut backend = TerminalBackend::in_memory(42);
        backend.feed_output(b"Connection reset by peer\r\n");
        app.tabs.push(Tab {
            id: 42,
            label: "test".into(),
            backend,
            ended: false,
        });
        app.active = Some(42);
        app.terminal_sender
            .send((
                42,
                PtyEvent::ChildExit(std::process::ExitStatus::from_raw(255 << 8)),
            ))
            .unwrap();
        app.terminal_sender.send((42, PtyEvent::Exit)).unwrap();
        app.drain_events();
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.active, Some(42));
        assert!(app.tabs[0].ended);
        assert!(app.message.contains("255"));
        #[cfg(not(all(feature = "ghostty", target_os = "macos")))]
        let text: String = app.tabs[0]
            .backend
            .sync()
            .cells
            .iter()
            .map(|cell| cell.c)
            .collect();
        #[cfg(all(feature = "ghostty", target_os = "macos"))]
        let text = app.tabs[0].backend.fixture_text();
        assert!(text.contains("Connection reset by peer"));
    }

    #[test]
    fn large_tree_renders_only_viewport_rows_and_selects_correct_file_after_scrolling() {
        for count in [1_000, 10_000, 100_000] {
            let ctx = egui::Context::default();
            let mut app = Relay::from_context(&ctx);
            let (sender, requests) = mpsc::channel();
            let (_events, receiver) = mpsc::channel();
            app.files = Worker {
                sender,
                receiver,
                cancel: Arc::new(AtomicBool::new(false)),
            };
            app.files_connected = true;
            app.root = "/mock".into();
            app.tree.insert(
                app.root.clone(),
                (0..count)
                    .map(|i| Entry {
                        path: format!("/mock/file-{i:06}.txt"),
                        name: format!("file-{i:06}.txt"),
                        directory: false,
                        file: true,
                        size: 4096,
                    })
                    .collect(),
            );
            let render = |app: &mut Relay, events, bottom: bool| {
                ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(500.0, 400.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        if bottom {
                            let id = ui.make_persistent_id(egui::IdSalt::new("remote_tree"));
                            let mut state =
                                egui::scroll_area::State::load(&ctx, id).unwrap_or_default();
                            state.offset.y = 100_000_000.0;
                            state.store(&ctx, id);
                        }
                        app.tree_ui(ui, 350.0);
                    },
                )
            };
            for _ in 0..3 {
                render(&mut app, vec![], false);
            }
            assert!(
                app.painted_tree_rows <= 12,
                "Only viewport rows should be painted for {count} entries"
            );
            assert_eq!(app.tree_cache.row_builds, 1);
            assert_eq!(app.tree_cache.search_builds, 0);
            render(&mut app, vec![], true);
            let output = render(&mut app, vec![], false);
            let last = format!("file-{:06}.txt", count - 1);
            let pos = rendered_label(&output.shapes, &last);
            assert!(
                pos.y >= 0.0 && pos.y <= 350.0,
                "Last row is inside the viewport: {pos:?}"
            );
            for pressed in [true, false] {
                render(
                    &mut app,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    false,
                );
            }
            assert!(
                matches!(requests.try_recv().unwrap(), Request::Preview { remote, offset: 0, .. } if remote == format!("/mock/{last}"))
            );
            assert!(requests.try_recv().is_err());
            assert!(app.painted_tree_rows <= 12);
            assert_eq!(
                app.tree_cache.row_builds, 1,
                "Scrolling must reuse flattened rows"
            );
            app.file_search = last.clone();
            let output = render(&mut app, vec![], false);
            rendered_label(&output.shapes, &last);
            assert_eq!(app.painted_tree_rows, 1);
            for _ in 0..3 {
                render(&mut app, vec![], false);
            }
            assert_eq!(app.tree_cache.search_builds, 1);
            assert_eq!(app.tree_cache.row_builds, 2);
            assert!(
                requests.try_recv().is_err(),
                "Searching and scrolling never request remote data"
            );
        }
    }

    #[test]
    fn filtered_folder_arrows_expand_cached_contents_and_load_only_on_click() {
        let ctx = egui::Context::default();
        let mut app = Relay::from_context(&ctx);
        let (sender, requests) = mpsc::channel();
        let (events, receiver) = mpsc::channel();
        app.files = Worker {
            sender,
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        app.files_connected = true;
        app.root = "/remote".into();
        app.file_search = "folder".into();
        let entry = |path: &str, directory: bool| Entry {
            path: path.into(),
            name: path.rsplit('/').next().unwrap().into(),
            directory,
            file: !directory,
            size: 0,
        };
        app.tree
            .insert("/remote".into(), vec![entry("/remote/folder", true)]);
        app.tree.insert(
            "/remote/folder".into(),
            vec![entry("/remote/folder/readme.txt", false)],
        );
        let render_tree = |app: &mut Relay, input_events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(600.0, 400.0),
                    )),
                    events: input_events,
                    ..Default::default()
                },
                |ui| {
                    app.tree_ui(ui, 350.0);
                },
            )
        };
        let click_arrow = |app: &mut Relay| {
            let output = render_tree(app, vec![]);
            fn image_center(shape: &egui::Shape) -> Option<egui::Pos2> {
                match shape {
                    egui::Shape::Mesh(mesh) => Some(mesh.calc_bounds().center()),
                    egui::Shape::Rect(rect) if rect.brush.is_some() => Some(rect.rect.center()),
                    egui::Shape::Vec(shapes) => shapes.iter().find_map(image_center),
                    _ => None,
                }
            }
            let pos = output
                .shapes
                .iter()
                .find_map(|shape| image_center(&shape.shape))
                .expect("Rendered folder arrow");
            for pressed in [true, false] {
                render_tree(
                    app,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
            }
        };
        for _ in 0..3 {
            render_tree(&mut app, vec![]);
        }
        click_arrow(&mut app);
        assert_eq!(app.file_search_expanded.get("/remote/folder"), Some(&true));
        let output = render_tree(&mut app, vec![]);
        rendered_label(&output.shapes, "readme.txt");
        assert!(
            requests.try_recv().is_err(),
            "Cached expansion needs no remote read"
        );
        click_arrow(&mut app);
        assert_eq!(app.file_search_expanded.get("/remote/folder"), Some(&false));
        app.tree.remove("/remote/folder");
        render_tree(&mut app, vec![]);
        assert!(requests.try_recv().is_err());
        click_arrow(&mut app);
        assert!(
            matches!(requests.try_recv().unwrap(), Request::List(path) if path == "/remote/folder")
        );
        assert!(requests.try_recv().is_err());
        events
            .send(Event::Entries(
                "/remote/folder".into(),
                vec![entry("/remote/folder/new.txt", false)],
            ))
            .unwrap();
        app.drain_events();
        let output = render_tree(&mut app, vec![]);
        rendered_label(&output.shapes, "new.txt");
        assert!(
            app.expanded.is_empty(),
            "Search expansion preserves the unfiltered tree"
        );
        app.file_search.clear();
        render_tree(&mut app, vec![]);
        assert!(app.file_search_expanded.is_empty());
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn file_search_filters_cached_names_without_remote_requests_or_expansion_changes() {
        let ctx = egui::Context::default();
        let mut app = Relay::from_context(&ctx);
        let (sender, requests) = mpsc::channel();
        let (_events, receiver) = mpsc::channel();
        app.files = Worker {
            sender,
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        app.files_connected = true;
        app.root = "/remote".into();
        app.destination = app.root.clone();
        let entry = |path: &str, directory: bool| Entry {
            path: path.into(),
            name: path.rsplit('/').next().unwrap().into(),
            directory,
            file: !directory,
            size: 0,
        };
        app.tree.insert(
            "/remote".into(),
            vec![
                entry("/remote/app", true),
                entry("/remote/unopened", true),
                entry("/remote/CONFIG.json", false),
                entry("/remote/readme.txt", false),
            ],
        );
        app.tree.insert(
            "/remote/app".into(),
            vec![entry("/remote/app/config.toml", false)],
        );
        // A cache left from navigating elsewhere must not appear in this tree.
        app.tree.insert(
            "/elsewhere".into(),
            vec![entry("/elsewhere/config.yaml", false)],
        );
        let original_expanded = app.expanded.clone();
        click_label(&mut app, &ctx, "Search loaded files…");
        render_input(&mut app, &ctx, vec![egui::Event::Text("  ConFiG  ".into())]);
        assert_eq!(
            *app.filtered_tree_paths().unwrap(),
            [
                "/remote/app",
                "/remote/app/config.toml",
                "/remote/CONFIG.json",
            ]
            .map(String::from)
            .into_iter()
            .collect()
        );
        let output = render_input(&mut app, &ctx, vec![]);
        rendered_label(&output.shapes, "app");
        rendered_label(&output.shapes, "config.toml");
        rendered_label(&output.shapes, "CONFIG.json");
        assert_eq!(app.expanded, original_expanded);
        assert!(requests.try_recv().is_err());

        app.file_search = "unopened".into();
        let output = render_input(&mut app, &ctx, vec![]);
        rendered_label(&output.shapes, "unopened");
        assert_eq!(app.filtered_tree_paths().unwrap().len(), 1);
        app.file_search = "missing".into();
        let output = render_input(&mut app, &ctx, vec![]);
        rendered_label(&output.shapes, "No matching files or folders.");
        click_label(&mut app, &ctx, "Clear");
        assert!(app.filtered_tree_paths().is_none());
        assert_eq!(app.expanded, original_expanded);
        let output = render_input(&mut app, &ctx, vec![]);
        rendered_label(&output.shapes, "readme.txt");
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn refresh_button_updates_open_tree_independently_of_upload_destination() {
        let ctx = egui::Context::default();
        let mut app = Relay::from_context(&ctx);
        let (sender, requests) = mpsc::channel();
        let (events, receiver) = mpsc::channel();
        app.files = Worker {
            sender,
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        app.files_connected = true;
        app.root = "/remote".into();
        app.destination = "/remote/gone".into();
        let entry = |path: &str, directory: bool| Entry {
            path: path.into(),
            name: path.rsplit('/').next().unwrap().into(),
            directory,
            file: !directory,
            size: 0,
        };
        app.expanded = ["/remote", "/remote/open", "/remote/gone"]
            .map(String::from)
            .into_iter()
            .collect();
        app.tree.insert(
            "/remote".into(),
            vec![entry("/remote/open", true), entry("/remote/gone", true)],
        );
        app.tree.insert("/remote/open".into(), vec![]);
        app.tree.insert(
            "/remote/gone".into(),
            vec![entry("/remote/gone/old.txt", false)],
        );
        app.selected_file = Some(entry("/remote/gone/old.txt", false));
        click_label(&mut app, &ctx, "Refresh");
        assert!(
            matches!(requests.try_recv().unwrap(), Request::Refresh { root, expanded } if root == "/remote" && expanded.contains("/remote/open"))
        );
        assert!(app.files_busy);
        events
            .send(Event::Refreshed(vec![
                (
                    "/remote".into(),
                    Ok(vec![
                        entry("/remote/open", true),
                        entry("/remote/new.txt", false),
                    ]),
                ),
                (
                    "/remote/open".into(),
                    Ok(vec![entry("/remote/open/nested.txt", false)]),
                ),
            ]))
            .unwrap();
        app.drain_events();
        assert!(!app.files_busy);
        assert!(app.expanded.contains("/remote/open"));
        assert!(!app.expanded.contains("/remote/gone"));
        assert!(!app.tree.contains_key("/remote/gone"));
        assert!(app.selected_file.is_none());
        assert_eq!(app.destination, "/remote");
        let output = render_input(&mut app, &ctx, vec![]);
        rendered_label(&output.shapes, "new.txt");
        rendered_label(&output.shapes, "nested.txt");
        assert_eq!(app.message, "Directory listing refreshed.");
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn clicking_remote_file_opens_preview_and_next_requests_only_the_next_range() {
        let ctx = egui::Context::default();
        let mut app = Relay::from_context(&ctx);
        let (sender, requests) = mpsc::channel();
        let (events, receiver) = mpsc::channel();
        app.files = Worker {
            sender,
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        app.files_connected = true;
        app.root = "/remote".into();
        app.destination = app.root.clone();
        app.tree.insert(
            app.root.clone(),
            vec![Entry {
                path: "/remote/config.toml".into(),
                name: "config.toml".into(),
                file: true,
                directory: false,
                size: 200_000,
            }],
        );
        click_label(&mut app, &ctx, "config.toml");
        let id = match requests.try_recv().unwrap() {
            Request::Preview {
                id,
                remote,
                offset: 0,
            } => {
                assert_eq!(remote, "/remote/config.toml");
                id
            }
            _ => panic!("Expected a range read, not a download"),
        };
        assert!(app.preview_active);
        let mut data = b"port = 8080\n".to_vec();
        data.resize(CHUNK_BYTES + 3, b' ');
        events
            .send(Event::Preview {
                id,
                result: Ok(crate::preview::Chunk {
                    offset: 0,
                    size: 200_000,
                    bytes: data,
                }),
            })
            .unwrap();
        app.drain_events();
        let output = render_input(&mut app, &ctx, vec![]);
        rendered_label(&output.shapes, "port = 8080");
        click_label(&mut app, &ctx, "Next");
        assert!(
            matches!(requests.try_recv().unwrap(), Request::Preview { offset, .. } if offset == CHUNK_BYTES as u64)
        );
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn preview_coalesces_selection_ignores_old_results_and_requests_pages_on_demand() {
        let ctx = egui::Context::default();
        let mut app = Relay::from_context(&ctx);
        let (sender, requests) = mpsc::channel();
        let (events, receiver) = mpsc::channel();
        app.files = Worker {
            sender,
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        app.files_connected = true;
        app.files_label = "Test host".into();
        let entry = |name: &str| Entry {
            name: name.into(),
            path: format!("/remote/{name}"),
            file: true,
            directory: false,
            size: 1_000_000,
        };
        app.open_preview(&entry("first.txt"));
        let first_id = match requests.try_recv().unwrap() {
            Request::Preview { id, offset: 0, .. } => id,
            _ => panic!("Expected initial range"),
        };
        app.open_preview(&entry("second.txt"));
        app.open_preview(&entry("latest.txt"));
        assert!(
            requests.try_recv().is_err(),
            "Only one read may be in flight"
        );
        events
            .send(Event::Preview {
                id: first_id,
                result: Err("stale error".into()),
            })
            .unwrap();
        app.drain_events();
        assert!(app.preview.as_ref().unwrap().error.is_none());
        let current_id = match requests.try_recv().unwrap() {
            Request::Preview {
                id,
                remote,
                offset: 0,
            } => {
                assert_eq!(remote, "/remote/latest.txt");
                id
            }
            _ => panic!("Expected only the latest selection"),
        };
        events
            .send(Event::Preview {
                id: current_id,
                result: Ok(crate::preview::Chunk {
                    offset: 0,
                    size: 1_000_000,
                    bytes: vec![b'x'; CHUNK_BYTES + 3],
                }),
            })
            .unwrap();
        app.drain_events();
        assert!(app.preview.as_ref().unwrap().chunk.is_some());
        app.pump_preview();
        assert!(
            requests.try_recv().is_err(),
            "No speculative full-file fetching"
        );
        app.preview_page(CHUNK_BYTES as u64);
        assert!(
            app.preview.as_ref().unwrap().chunk.is_none(),
            "Release the previous chunk"
        );
        let next_id = match requests.try_recv().unwrap() {
            Request::Preview { id, offset, .. } => {
                assert_eq!(offset, CHUNK_BYTES as u64);
                id
            }
            _ => panic!("Expected next range"),
        };
        app.close_preview();
        assert!(matches!(
            requests.try_recv().unwrap(),
            Request::ClosePreview
        ));
        events
            .send(Event::Preview {
                id: next_id,
                result: Err("late result".into()),
            })
            .unwrap();
        app.drain_events();
        assert!(app.preview.is_none());
        assert!(!app.preview_active);
    }

    fn rendered_label(shapes: &[egui::epaint::ClippedShape], label: &str) -> egui::Pos2 {
        fn position(shape: &egui::Shape, label: &str) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Text(text) if text.galley.job.text == label => {
                    Some(egui::Rect::from_min_size(text.pos, text.galley.size()).center())
                }
                egui::Shape::Vec(shapes) => shapes.iter().find_map(|s| position(s, label)),
                _ => None,
            }
        }
        shapes
            .iter()
            .find_map(|s| position(&s.shape, label))
            .unwrap_or_else(|| panic!("Missing visible label: {label}"))
    }

    fn render_input(
        app: &mut Relay,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1100.0, 720.0),
                )),
                events,
                ..Default::default()
            },
            |ui| app.render(ui),
        )
    }

    fn click_label(app: &mut Relay, ctx: &egui::Context, label: &str) {
        for _ in 0..3 {
            render_input(app, ctx, vec![]);
        }
        let output = render_input(app, ctx, vec![]);
        let pos = rendered_label(&output.shapes, label);
        for pressed in [true, false] {
            render_input(
                app,
                ctx,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
    }

    #[test]
    fn modal_buttons_text_entry_validation_and_save_work_together() {
        let ctx = egui::Context::default();
        let mut app = Relay::from_context(&ctx);
        let temp = tempfile::tempdir().unwrap();
        app.config_path = Some(temp.path().join("connections.json"));
        app.config_error = false;
        app.profiles.clear();
        click_label(&mut app, &ctx, "New connection");
        assert!(app.connection_editor.is_some());
        render_input(&mut app, &ctx, vec![]);
        render_input(&mut app, &ctx, vec![egui::Event::Text("Work".into())]);
        assert_eq!(app.connection_editor.as_ref().unwrap().profile.name, "Work");
        click_label(&mut app, &ctx, "Save connection");
        assert!(!app.connection_editor.as_ref().unwrap().error.is_empty());
        assert!(app.profiles.is_empty());
        click_label(&mut app, &ctx, "server.example.com");
        render_input(
            &mut app,
            &ctx,
            vec![egui::Event::Text("test.example.com".into())],
        );
        click_label(&mut app, &ctx, "Save connection");
        assert!(app.connection_editor.is_none());
        assert_eq!(app.profiles[0].host, "test.example.com");
        assert_eq!(
            profiles::load(app.config_path.as_ref().unwrap()).unwrap(),
            app.profiles
        );
        click_label(&mut app, &ctx, "New connection");
        click_label(&mut app, &ctx, "Cancel");
        assert!(app.connection_editor.is_none());
        assert_eq!(app.profiles.len(), 1);
    }

    #[test]
    fn connection_actions_remain_visible_in_a_small_window_with_many_hosts() {
        let ctx = egui::Context::default();
        let mut app = Relay::from_context(&ctx);
        app.profiles = (0..30)
            .map(|i| Profile {
                host: format!("host{i}.example.com"),
                ..Default::default()
            })
            .collect();
        app.selected = Some(0);
        app.draft = app.profiles[0].clone();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(760.0, 480.0),
            )),
            ..Default::default()
        };
        for _ in 0..3 {
            let _ = ctx.run_ui(input.clone(), |ui| app.render(ui));
        }
        let output = ctx.run_ui(input, |ui| app.render(ui));
        for label in ["New connection", "Open terminal", "Edit…"] {
            let pos = rendered_label(&output.shapes, label);
            assert!(pos.y < 450.0, "{label} fell below the window: {pos:?}");
        }
        app.edit_connection(None);
        for _ in 0..4 {
            let _ = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(760.0, 480.0),
                    )),
                    ..Default::default()
                },
                |ui| app.render(ui),
            );
        }
        let modal = ctx
            .memory(|m| m.area_rect(egui::Id::new("connection_editor")))
            .unwrap();
        assert!(
            modal.top() >= 0.0 && modal.bottom() <= 480.0,
            "Modal escaped the window: {modal:?}"
        );
    }

    #[test]
    fn connection_save_validates_and_persists_before_changing_selection() {
        let ctx = egui::Context::default();
        let mut app = Relay::from_context(&ctx);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("connections.json");
        app.config_path = Some(path.clone());
        app.config_error = false;
        app.profiles.clear();
        let profile = Profile {
            host: "example.com".into(),
            ..Default::default()
        };
        assert!(app.save_profile(&Profile::default(), None).is_err());
        assert!(app.profiles.is_empty());
        assert!(!path.exists());
        app.save_profile(&profile, None).unwrap();
        assert_eq!(app.selected, Some(0));
        assert_eq!(app.draft, profile);
        assert_eq!(profiles::load(&path).unwrap(), vec![profile.clone()]);

        let edited = Profile {
            name: "Production".into(),
            ..profile.clone()
        };
        // A failed write must leave the saved connection and selection intact.
        app.config_path = Some(temp.path().to_owned());
        assert!(app.save_profile(&edited, Some(0)).is_err());
        assert_eq!(app.draft, profile);
        app.config_path = Some(path.clone());
        app.save_profile(&edited, Some(0)).unwrap();
        assert_eq!(profiles::load(&path).unwrap(), vec![edited.clone()]);
        assert_eq!(app.draft, edited);
    }

    #[test]
    fn escape_discards_connection_edits_and_preserves_active_host() {
        let ctx = egui::Context::default();
        let mut app = Relay::from_context(&ctx);
        let profile = Profile {
            host: "existing.example.com".into(),
            ..Default::default()
        };
        app.profiles = vec![profile.clone()];
        app.selected = Some(0);
        app.draft = profile.clone();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(760.0, 480.0),
            )),
            ..Default::default()
        };
        for index in [None, Some(0)] {
            app.edit_connection(index);
            app.connection_editor.as_mut().unwrap().profile.host = "unsaved.example.com".into();
            let _ = ctx.run_ui(input.clone(), |ui| app.render(ui));
            let _ = ctx.run_ui(input.clone(), |ui| app.render(ui));
            let mut escape = input.clone();
            escape.events.push(egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
            let _ = ctx.run_ui(escape, |ui| app.render(ui));
            assert!(app.connection_editor.is_none());
            assert_eq!(app.selected, Some(0));
            assert_eq!(app.draft, profile);
            assert_eq!(app.profiles, vec![profile.clone()]);
        }
    }

    #[test]
    fn desktop_file_drop_targets_selected_directory_and_blocks_when_busy() {
        let ctx = egui::Context::default();
        let mut app = Relay::from_context(&ctx);
        let (sender, requests) = mpsc::channel();
        let (_events, receiver) = mpsc::channel();
        app.files = Worker {
            sender,
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        app.files_connected = true;
        app.root = "/remote".into();
        app.destination = "/remote/selected folder".into();
        app.tree.insert(app.root.clone(), vec![]);
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("example.txt");
        std::fs::write(&file, b"test").unwrap();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1100.0, 720.0),
            )),
            dropped_files: vec![egui::DroppedFile {
                path: Some(file.clone()),
                ..Default::default()
            }],
            ..Default::default()
        };
        app.edit_connection(None);
        let _ = ctx.run_ui(input.clone(), |ui| app.render(ui));
        assert!(
            requests.try_recv().is_err(),
            "A modal must block file uploads"
        );
        app.connection_editor = None;
        let _ = ctx.run_ui(input.clone(), |ui| app.render(ui));
        match requests.try_recv().unwrap() {
            Request::Upload { local, remote } => {
                assert_eq!(local, vec![file]);
                assert_eq!(remote, "/remote/selected folder");
            }
            _ => panic!("Drop did not produce an upload"),
        }
        let _ = ctx.run_ui(input.clone(), |ui| app.render(ui));
        assert!(requests.try_recv().is_err());
        app.files_busy = false;
        app.files_connected = false;
        let _ = ctx.run_ui(input, |ui| app.render(ui));
        assert!(requests.try_recv().is_err());
    }
}
