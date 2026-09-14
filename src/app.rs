use crate::{
    files::{Entry, Event, Request, Worker},
    profiles::{self, Profile},
};
use eframe::egui::{self, Color32, RichText};
use egui_term::{BackendSettings, PtyEvent, TerminalBackend, TerminalView};
use std::{
    collections::{BTreeMap, BTreeSet},
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
pub struct Relay {
    profiles: Vec<Profile>,
    config_path: Option<PathBuf>,
    config_error: bool,
    selected: Option<usize>,
    draft: Profile,
    search: String,
    message: String,
    tabs: Vec<Tab>,
    active: Option<u64>,
    next_id: u64,
    focus_terminal: bool,
    terminal_events: Receiver<(u64, PtyEvent)>,
    terminal_sender: Sender<(u64, PtyEvent)>,
    files: Worker,
    files_connected: bool,
    files_busy: bool,
    files_label: String,
    secret: Zeroizing<String>,
    pending_profile: Option<Profile>,
    fingerprint: Option<String>,
    root: String,
    destination: String,
    tree: BTreeMap<String, Vec<Entry>>,
    expanded: BTreeSet<String>,
    selected_file: Option<Entry>,
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
        Self::from_context(&cc.egui_ctx)
    }

    fn from_context(ctx: &egui::Context) -> Self {
        ctx.set_visuals(egui::Visuals::dark());
        ctx.style_mut_of(egui::Theme::Dark, |style| {
            style.spacing.item_spacing = egui::vec2(8.0, 8.0);
            style.spacing.button_padding = egui::vec2(10.0, 6.0);
            style.visuals.selection.bg_fill = Color32::from_rgb(34, 90, 93);
            style.visuals.panel_fill = Color32::from_rgb(23, 27, 32);
            style.visuals.window_fill = Color32::from_rgb(27, 32, 38);
        });
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
            profiles,
            config_path,
            config_error,
            selected: None,
            draft: Profile::default(),
            search: String::new(),
            message,
            tabs: Vec::new(),
            active: None,
            next_id: 1,
            focus_terminal: false,
            terminal_events,
            terminal_sender,
            files: Worker::new(ctx.clone()),
            files_connected: false,
            files_busy: false,
            files_label: String::new(),
            secret: Zeroizing::new(String::new()),
            pending_profile: None,
            fingerprint: None,
            root: String::new(),
            destination: String::new(),
            tree: BTreeMap::new(),
            expanded: BTreeSet::new(),
            selected_file: None,
            progress: None,
            file_panel: true,
            dialog_sender,
            dialog_receiver,
        }
    }

    fn save(&mut self) {
        if self.config_error {
            self.message =
                "Resolve the configuration read error and restart before saving connections."
                    .into();
            return;
        }
        if let Err(error) = self.draft.ssh_args() {
            self.message = error;
            return;
        }
        let mut updated = self.profiles.clone();
        if let Some(index) = self.selected {
            updated[index] = self.draft.clone();
        } else {
            updated.push(self.draft.clone());
        }
        if let Some(path) = &self.config_path {
            match profiles::save(path, &updated) {
                Ok(()) => {
                    self.selected = Some(self.selected.unwrap_or(updated.len() - 1));
                    self.profiles = updated;
                    self.message = "Connection saved.".into();
                }
                Err(e) => self.message = e,
            }
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
                self.next_id += 1;
                self.tabs.push(Tab {
                    id,
                    label: self.draft.label().into(),
                    backend,
                    ended: false,
                });
                self.active = Some(id);
                self.focus_terminal = true;
                self.message = "SSH session started. Authentication and host-key prompts appear in the terminal.".into();
            }
            Err(error) => self.message = error,
        }
    }
    fn files_connect(&mut self, trusted: Option<String>) {
        let profile = if trusted.is_some() {
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
        self.files_busy = true;
        self.files_connected = false;
        self.tree.clear();
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
        self.files_busy = true;
        self.send(Request::List(path));
    }
    fn transfer(&mut self, request: Request) {
        self.files.cancel.store(false, Ordering::Relaxed);
        self.files_busy = true;
        self.progress = None;
        self.send(request);
    }
    fn drain_events(&mut self) {
        for (id, event) in self.terminal_events.try_iter() {
            if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id)
                && let PtyEvent::Exit = event
            {
                tab.ended = true;
            }
        }
        while let Ok(event) = self.files.receiver.try_recv() {
            match event {
                Event::Connected(home) => {
                    self.files_connected = true;
                    self.files_busy = false;
                    self.root = home.clone();
                    self.destination = home.clone();
                    self.expanded.insert(home.clone());
                    self.refresh(home);
                    self.secret = Zeroizing::new(String::new());
                    self.message = format!("SFTP connected: {}", self.files_label);
                }
                Event::Trust(fingerprint) => {
                    self.files_busy = false;
                    self.fingerprint = Some(fingerprint);
                }
                Event::Entries(path, entries) => {
                    self.files_busy = false;
                    self.tree.insert(path, entries);
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
    }
    fn host_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.label(RichText::new("CONNECTIONS").small().color(Color32::GRAY));
        ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .hint_text("Find a host…")
                .desired_width(f32::INFINITY),
        );
        egui::ScrollArea::vertical()
            .id_salt("hosts")
            .max_height(180.0)
            .show(ui, |ui| {
                let filter = self.search.to_lowercase();
                for (index, profile) in self.profiles.iter().enumerate() {
                    if !format!("{} {}", profile.label(), profile.host)
                        .to_lowercase()
                        .contains(&filter)
                    {
                        continue;
                    }
                    if ui
                        .selectable_label(self.selected == Some(index), profile.label())
                        .clicked()
                    {
                        self.selected = Some(index);
                        self.draft = profile.clone();
                    }
                }
                if self.profiles.is_empty() {
                    ui.weak("Your saved hosts will appear here.");
                }
            });
        if ui.button("+ New connection").clicked() {
            self.selected = None;
            self.draft = Profile::default();
        }
        ui.separator();
        ui.label(RichText::new("HOST DETAILS").small().color(Color32::GRAY));
        field(ui, "Name", &mut self.draft.name, "Production server");
        field(
            ui,
            "Host / SSH alias",
            &mut self.draft.host,
            "server.example.com",
        );
        field(
            ui,
            "Username",
            &mut self.draft.user,
            "Use SSH config default",
        );
        field(ui, "Port", &mut self.draft.port, "Use SSH config default");
        field(
            ui,
            "Private key",
            &mut self.draft.identity,
            "Optional absolute path",
        );
        ui.horizontal(|ui| {
            if ui.button("Save").clicked() {
                self.save();
            }
            if ui
                .add_enabled(
                    self.selected.is_some() && !self.config_error,
                    egui::Button::new("Remove"),
                )
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
        ui.add_space(8.0);
        if ui
            .add_sized(
                [ui.available_width(), 34.0],
                egui::Button::new("Open terminal").fill(Color32::from_rgb(35, 96, 94)),
            )
            .clicked()
        {
            self.terminal_connect(ui.ctx());
        }
        ui.add_space(6.0);
        ui.weak("Keys and passwords are handled by SSH. Connection details stay on this device.");
        if let Some(path) = &self.config_path {
            ui.label(RichText::new("Local storage").small())
                .on_hover_text(path.display().to_string());
        }
    }
    fn file_panel_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("REMOTE FILES").small().color(Color32::GRAY));
            if self.files_busy {
                ui.spinner();
            }
        });
        if !self.files_connected {
            ui.label("Browse a host and drop files or folders to upload them.");
            ui.weak("Uses the host details on the left.");
            ui.label("Password / key passphrase (optional)");
            ui.add(
                egui::TextEdit::singleline(&mut *self.secret)
                    .password(true)
                    .hint_text("Try SSH agent and keys first")
                    .desired_width(f32::INFINITY),
            );
            if ui
                .add_enabled(
                    !self.files_busy && self.fingerprint.is_none(),
                    egui::Button::new("Connect files"),
                )
                .clicked()
            {
                self.files_connect(None);
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
                        self.files_connect(Some(fingerprint));
                    }
                    if ui.button("Cancel").clicked() {
                        self.fingerprint = None;
                        self.secret = Zeroizing::new(String::new());
                    }
                });
            }
            return;
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new(&self.files_label).strong());
            if ui
                .add_enabled(!self.files_busy, egui::Button::new("Disconnect"))
                .clicked()
            {
                self.files = Worker::new(ui.ctx().clone());
                self.files_connected = false;
                self.tree.clear();
                self.selected_file = None;
            }
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.files_busy, egui::Button::new("Up"))
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
                .add_enabled(!self.files_busy, egui::Button::new("Refresh"))
                .clicked()
            {
                self.refresh(self.destination.clone());
            }
        });
        ui.monospace(&self.destination);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.files_busy, egui::Button::new("Upload…"))
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
                .add_enabled(can_download, egui::Button::new("Download…"))
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
        ui.separator();
        let height = (ui.available_height() - 96.0).max(100.0);
        egui::ScrollArea::both()
            .id_salt("remote_tree")
            .max_height(height)
            .show(ui, |ui| {
                if ui
                    .selectable_label(self.destination == self.root, format!("- {}", self.root))
                    .clicked()
                {
                    self.destination = self.root.clone();
                    self.selected_file = None;
                }
                let root = self.root.clone();
                ui.add_enabled_ui(!self.files_busy, |ui| self.tree_ui(ui, &root, 0));
            });
        ui.separator();
        ui.label(
            RichText::new("Drop files or folders here").color(Color32::from_rgb(125, 203, 187)),
        );
        ui.weak("Uploads go to the selected folder. Existing files are kept.");
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
    fn tree_ui(&mut self, ui: &mut egui::Ui, path: &str, depth: usize) {
        if depth > 64 {
            return;
        }
        let Some(entries) = self.tree.get(path).cloned() else {
            ui.weak("Loading…");
            return;
        };
        if entries.is_empty() {
            ui.weak("Empty folder");
        }
        for entry in entries {
            let expanded = self.expanded.contains(&entry.path);
            ui.horizontal(|ui| {
                ui.add_space((depth + 1) as f32 * 12.0);
                if entry.directory {
                    if ui.small_button(if expanded { "-" } else { "+" }).clicked() {
                        if expanded {
                            self.expanded.remove(&entry.path);
                        } else {
                            self.expanded.insert(entry.path.clone());
                            self.refresh(entry.path.clone());
                        }
                    }
                    if ui
                        .selectable_label(self.destination == entry.path, &entry.name)
                        .clicked()
                    {
                        self.destination = entry.path.clone();
                        self.selected_file = None;
                    }
                } else {
                    let label = if entry.file {
                        entry.name.clone()
                    } else {
                        format!("{} (link / special)", entry.name)
                    };
                    if ui
                        .selectable_label(
                            self.selected_file
                                .as_ref()
                                .is_some_and(|f| f.path == entry.path),
                            label,
                        )
                        .on_hover_text(format!("{} · {}", entry.path, bytes(entry.size)))
                        .clicked()
                    {
                        self.selected_file = Some(entry.clone());
                        self.destination = path.into();
                    }
                }
            });
            if entry.directory && expanded {
                self.tree_ui(ui, &entry.path, depth + 1);
            }
        }
    }
}

impl eframe::App for Relay {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.render(ui);
    }
}

impl Relay {
    fn render(&mut self, ui: &mut egui::Ui) {
        self.drain_events();
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("RELAY").strong().size(19.0));
                ui.weak("SSH workspace");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.toggle_value(&mut self.file_panel, "Files");
                });
            });
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.small(&self.message);
            });
        });
        egui::Panel::left("connections")
            .resizable(true)
            .default_size(235.0)
            .size_range(210.0..=340.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.host_panel(ui));
            });
        if self.file_panel {
            egui::Panel::right("files")
                .resizable(true)
                .default_size(300.0)
                .size_range(240.0..=600.0)
                .show(ui, |ui| self.file_panel_ui(ui));
        }
        let dropped: Vec<_> = ui.ctx().input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
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
        egui::CentralPanel::default().show(ui, |ui| {
            let mut close = None;
            ui.horizontal_wrapped(|ui| {
                for tab in &self.tabs {
                    let label = format!("{}{}", tab.label, if tab.ended { " · ended" } else { "" });
                    if ui
                        .selectable_label(self.active == Some(tab.id), label)
                        .clicked()
                    {
                        self.active = Some(tab.id);
                        self.focus_terminal = true;
                    }
                    if ui
                        .small_button("×")
                        .on_hover_text("Close SSH session")
                        .clicked()
                    {
                        close = Some(tab.id);
                    }
                }
            });
            if let Some(id) = close {
                self.tabs.retain(|tab| tab.id != id);
                if self.active == Some(id) {
                    self.active = self.tabs.last().map(|tab| tab.id);
                }
            }
            ui.separator();
            if let Some(tab) = self.tabs.iter_mut().find(|tab| Some(tab.id) == self.active) {
                let focus = std::mem::take(&mut self.focus_terminal);
                let terminal = TerminalView::new(ui, &mut tab.backend).set_focus(focus);
                ui.add(terminal);
            } else {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.26);
                    ui.label(RichText::new("Your servers. One quiet workspace.").size(23.0));
                    ui.add_space(12.0);
                    ui.weak("Add a host on the left to open an SSH terminal.");
                    ui.weak("Connect the file browser to explore folders and transfer files.");
                });
            }
        });
    }
}
fn field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) {
    ui.label(label);
    ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .desired_width(f32::INFINITY),
    );
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
