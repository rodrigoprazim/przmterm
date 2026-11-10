//! The SFTP browser (also for SCP sessions): a window per session to move around the remote
//! folders, upload (also by dropping files on it) and download files or
//! folders, create folders, rename and delete.

use std::collections::BTreeSet;
use std::path::PathBuf;

use eframe::egui;
use przmterm_transport::sftp::{Entry, SftpClient, SftpEvent, join, parent};

/// The column the list is sorted by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortBy {
    Name,
    Size,
    Modified,
    Permissions,
}

/// Sorts a listing by `by`, folders always first; equal values by name.
fn sort_entries(entries: &mut [Entry], by: SortBy, ascending: bool) {
    let name = |e: &Entry| e.name.to_lowercase();
    entries.sort_by(|a, b| {
        let order = match by {
            SortBy::Name => std::cmp::Ordering::Equal,
            SortBy::Size => a.size.cmp(&b.size),
            SortBy::Modified => a.modified.cmp(&b.modified),
            SortBy::Permissions => a.permissions.cmp(&b.permissions),
        }
        .then_with(|| name(a).cmp(&name(b)));
        let order = if ascending { order } else { order.reverse() };
        b.is_dir.cmp(&a.is_dir).then(order)
    });
}

enum State {
    Running,
    Done,
    Failed(String),
}

struct Transfer {
    id: u64,
    label: String,
    done: u64,
    total: u64,
    state: State,
}

/// What a dialog of the browser is asking.
enum Ask {
    /// The SCP shell (`sudo su -`…) asks for a password; the text says why.
    Password {
        why: String,
        text: zeroize::Zeroizing<String>,
    },
    NewFolder(String),
    Rename {
        from: String,
        to: String,
    },
    Delete(Vec<(String, bool)>),
}

pub struct SftpBrowser {
    /// The session's name, for the title.
    pub name: String,
    /// `SFTP` or `SCP`, for the title.
    protocol: &'static str,
    id: u64,
    client: SftpClient,
    ready: bool,
    dir: String,
    path_edit: String,
    entries: Vec<Entry>,
    selected: BTreeSet<String>,
    transfers: Vec<Transfer>,
    next_id: u64,
    ask: Option<Ask>,
    error: Option<String>,
    closed: Option<String>,
    /// Where the last download went, to start there next time.
    last_download: Option<PathBuf>,
    /// Show names starting with a dot.
    hidden: bool,
    /// Dragging a selection box: where it started, and what was selected
    /// before (kept with Ctrl/⌘).
    drag_from: Option<(egui::Pos2, BTreeSet<String>)>,
    /// The last row clicked, for Shift+click ranges.
    anchor: Option<String>,
    /// The column the list is sorted by, and whether ascending.
    sort: (SortBy, bool),
}

/// `1.2 MB`.
pub fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// `drwxr-xr-x` from Unix mode bits.
pub fn mode_text(mode: u32, is_dir: bool) -> String {
    let mut s = String::with_capacity(10);
    s.push(if is_dir { 'd' } else { '-' });
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    s
}

impl SftpBrowser {
    pub fn new(name: String, id: u64, client: SftpClient, protocol: &'static str) -> Self {
        Self {
            name,
            protocol,
            id,
            client,
            ready: false,
            dir: String::new(),
            path_edit: String::new(),
            entries: Vec::new(),
            selected: BTreeSet::new(),
            transfers: Vec::new(),
            next_id: 1,
            ask: None,
            error: None,
            closed: None,
            last_download: dirs::download_dir(),
            hidden: false,
            drag_from: None,
            anchor: None,
            sort: (SortBy::Name, true),
        }
    }

    fn go(&mut self, dir: &str) {
        self.client.list(dir);
    }

    fn start_upload(&mut self, local: PathBuf) {
        let id = self.next_id;
        self.next_id += 1;
        let label = format!(
            "↑ {}",
            local
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
        self.client.upload(id, local, &self.dir);
        self.transfers.push(Transfer {
            id,
            label,
            done: 0,
            total: 0,
            state: State::Running,
        });
    }

    fn start_downloads(&mut self, names: Vec<String>) {
        if names.is_empty() {
            return;
        }
        let mut picker = rfd::FileDialog::new().set_title("Download to this folder");
        if let Some(dir) = &self.last_download {
            picker = picker.set_directory(dir);
        }
        let Some(folder) = picker.pick_folder() else {
            return;
        };
        for name in names {
            let id = self.next_id;
            self.next_id += 1;
            // Never overwrite: "name (2)" if it is there already.
            let mut target = folder.join(&name);
            let mut n = 2;
            while target.exists() {
                let path = std::path::Path::new(&name);
                let stem = path.file_stem().unwrap_or_default().to_string_lossy();
                target = folder.join(match path.extension() {
                    Some(ext) => format!("{stem} ({n}).{}", ext.to_string_lossy()),
                    None => format!("{stem} ({n})"),
                });
                n += 1;
            }
            self.client.download(id, &join(&self.dir, &name), target);
            self.transfers.push(Transfer {
                id,
                label: format!("↓ {name}"),
                done: 0,
                total: 0,
                state: State::Running,
            });
        }
        self.last_download = Some(folder);
    }

    fn handle(&mut self, event: SftpEvent) {
        match event {
            SftpEvent::Ready { home } => {
                self.ready = true;
                self.go(&home);
            }
            SftpEvent::Listing { dir, entries } => {
                if dir != self.dir {
                    self.selected.clear();
                }
                self.selected
                    .retain(|s| entries.iter().any(|e| &e.name == s));
                self.path_edit = dir.clone();
                self.dir = dir;
                self.entries = entries;
                sort_entries(&mut self.entries, self.sort.0, self.sort.1);
                self.error = None;
            }
            SftpEvent::Progress { id, done, total } => {
                if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
                    t.done = done;
                    t.total = total;
                }
            }
            SftpEvent::TransferDone { id, result } => {
                if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
                    t.state = match result {
                        Ok(()) => State::Done,
                        Err(e) => State::Failed(e),
                    };
                }
            }
            SftpEvent::Changed { dir } => {
                if dir == self.dir || self.dir.is_empty() {
                    let d = self.dir.clone();
                    self.go(&d);
                }
            }
            SftpEvent::Failed { what, error } => self.error = Some(format!("{what}: {error}")),
            SftpEvent::Closed(why) => self.closed = Some(why),
            SftpEvent::PasswordNeeded(why) => {
                self.ask = Some(Ask::Password {
                    why,
                    text: zeroize::Zeroizing::new(String::new()),
                });
            }
        }
    }

    /// Draws the window; returns false once it is closed. Files dropped on
    /// it are uploaded; with `drops` it also takes files dropped elsewhere.
    pub fn show(&mut self, ctx: &egui::Context, drops: bool) -> bool {
        for event in self.client.poll() {
            self.handle(event);
        }
        let mut open = true;
        let response = egui::Window::new(format!("{}: {}", self.protocol, self.name))
            .id(egui::Id::unique(("sftp", self.id)))
            .open(&mut open)
            .default_size([760.0, 520.0])
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .resizable(true)
            .collapsible(true)
            .show(ctx, |ui| self.body(ui));
        // Files dropped on the window are uploaded to the current folder.
        if let Some(r) = response
            && self.ready
            && self.closed.is_none()
        {
            let dropped: Vec<PathBuf> = ctx.input(|i| {
                let over = i
                    .pointer
                    .hover_pos()
                    .is_some_and(|p| r.response.rect.contains(p));
                if over || drops {
                    i.raw
                        .dropped_files
                        .iter()
                        .map(|f| f.path().to_path_buf())
                        .filter(|p| !p.as_os_str().is_empty())
                        .collect()
                } else {
                    Vec::new()
                }
            });
            for path in dropped {
                self.start_upload(path);
            }
        }
        self.dialogs(ctx);
        open
    }

    fn body(&mut self, ui: &mut egui::Ui) {
        if let Some(why) = &self.closed {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("The {} session ended: {why}", self.protocol),
            );
        }
        if !self.ready && self.closed.is_none() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("Opening {}…", self.protocol));
            });
            return;
        }
        let selected: Vec<(String, bool)> = self
            .entries
            .iter()
            .filter(|e| self.selected.contains(&e.name))
            .map(|e| (e.name.clone(), e.is_dir))
            .collect();
        let live = self.closed.is_none();
        ui.horizontal(|ui| {
            ui.add_enabled_ui(live, |ui| {
                if crate::icons::button(ui, crate::icons::Icon::Up, false)
                    .on_hover_text("Parent folder")
                    .clicked()
                {
                    let up = parent(&self.dir);
                    self.go(&up);
                }
                if crate::icons::button(ui, crate::icons::Icon::Reload, false)
                    .on_hover_text("Refresh")
                    .clicked()
                {
                    let d = self.dir.clone();
                    self.go(&d);
                }
                let path = ui.add(
                    egui::TextEdit::singleline(&mut self.path_edit)
                        .desired_width(ui.available_width() - 8.0)
                        .font(egui::TextStyle::Monospace),
                );
                if path.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let target = self.path_edit.trim().to_string();
                    let target = if target.starts_with('/') || target.starts_with('~') {
                        target
                    } else {
                        join(&self.dir, &target)
                    };
                    self.go(&target);
                }
            });
        });
        ui.horizontal(|ui| {
            ui.add_enabled_ui(live, |ui| {
                if ui.button("Upload files…").clicked()
                    && let Some(files) = rfd::FileDialog::new()
                        .set_title("Upload files")
                        .pick_files()
                {
                    for f in files {
                        self.start_upload(f);
                    }
                }
                if ui.button("Upload folder…").clicked()
                    && let Some(folder) = rfd::FileDialog::new()
                        .set_title("Upload a folder")
                        .pick_folder()
                {
                    self.start_upload(folder);
                }
                if ui
                    .add_enabled(!selected.is_empty(), egui::Button::new("Download…"))
                    .on_disabled_hover_text("Select files or folders first")
                    .clicked()
                {
                    self.start_downloads(selected.iter().map(|(n, _)| n.clone()).collect());
                }
                ui.separator();
                if ui.button("New folder…").clicked() {
                    self.ask = Some(Ask::NewFolder(String::new()));
                }
                if ui
                    .add_enabled(selected.len() == 1, egui::Button::new("Rename…"))
                    .clicked()
                {
                    let name = selected[0].0.clone();
                    self.ask = Some(Ask::Rename {
                        from: name.clone(),
                        to: name,
                    });
                }
                if ui
                    .add_enabled(!selected.is_empty(), egui::Button::new("Delete…"))
                    .clicked()
                {
                    self.ask = Some(Ask::Delete(selected.clone()));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.checkbox(&mut self.hidden, "Hidden files")
                        .on_hover_text("Show names that start with a dot");
                });
            });
        });
        if let Some(e) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        ui.separator();

        // The folder's contents.
        let transfers_h = if self.transfers.is_empty() {
            0.0
        } else {
            (self.transfers.len().min(4) as f32) * 26.0 + 36.0
        };
        let list_h = (ui.available_height() - transfers_h - 44.0).max(120.0);
        let width = ui.available_width();
        let (name_w, size_w, date_w) = (width * 0.46, width * 0.14, width * 0.2);
        let gap = ui.spacing().item_spacing.x;
        let perm_w = (width - name_w - size_w - date_w - 3.0 * gap).max(40.0);
        let row_h = ui.spacing().interact_size.y;
        // The headers: a click sorts by the column, again reverses it.
        let sort = self.sort;
        let mut sort_by = None;
        let mut header = |ui: &mut egui::Ui, w: f32, text: &str, by: SortBy| {
            let arrow = match sort {
                (s, true) if s == by => " ▲",
                (s, false) if s == by => " ▼",
                _ => "",
            };
            // Under the text: the highlight, once hovering is known.
            let highlight = ui.painter().add(egui::Shape::Noop);
            // Sizes are right-aligned: so is their header.
            let layout = if by == SortBy::Size {
                egui::Layout::right_to_left(egui::Align::Center)
            } else {
                egui::Layout::left_to_right(egui::Align::Center)
            };
            let cell = ui
                .allocate_ui_with_layout(egui::vec2(w, row_h), layout, |ui| {
                    ui.set_min_width(w);
                    ui.add_space(if by == SortBy::Size { 12.0 } else { 4.0 });
                    ui.strong(format!("{text}{arrow}"));
                })
                .response;
            let click = ui
                .interact(
                    cell.rect,
                    ui.scope_id().with(("sort", text)),
                    egui::Sense::click(),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(format!("Sort by {}", text.to_lowercase()));
            if click.hovered() {
                ui.painter().set(
                    highlight,
                    egui::epaint::RectShape::filled(
                        cell.rect,
                        2.0,
                        ui.visuals().widgets.hovered.weak_bg_fill,
                    ),
                );
            }
            if click.clicked() {
                sort_by = Some(by);
            }
            cell.rect
        };
        let headers = ui
            .horizontal(|ui| {
                [
                    header(ui, name_w, "Name", SortBy::Name),
                    header(ui, size_w, "Size", SortBy::Size),
                    header(ui, date_w, "Modified", SortBy::Modified),
                    header(ui, perm_w, "Permissions", SortBy::Permissions),
                ]
            })
            .inner;
        if let Some(by) = sort_by {
            self.sort = match self.sort {
                (s, ascending) if s == by => (by, !ascending),
                _ => (by, true),
            };
            sort_entries(&mut self.entries, self.sort.0, self.sort.1);
        }
        let mut open_dir = None;
        let mut download = None;
        let (multi, range) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
        // Each row's place on screen, for selecting with a box.
        let mut rows: Vec<(String, egui::Rect)> = Vec::new();
        let list = egui::ScrollArea::vertical()
            .id_salt(("sftp-list", self.id))
            .max_height(list_h)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.entries.is_empty() {
                    ui.weak("This folder is empty.");
                }
                for e in self
                    .entries
                    .iter()
                    .filter(|e| self.hidden || !e.name.starts_with('.'))
                {
                    let on = self.selected.contains(&e.name);
                    let icon = if e.is_dir { "📁" } else { "📄" };
                    let name = if e.is_link {
                        format!("{icon} {} ↗", e.name)
                    } else {
                        format!("{icon} {}", e.name)
                    };
                    let line = ui.horizontal(|ui| {
                        // The name fills its column and is left-aligned, so
                        // the whole cell selects the row.
                        let r = ui
                            .allocate_ui_with_layout(
                                egui::vec2(name_w, row_h),
                                egui::Layout::left_to_right(egui::Align::Center)
                                    .with_main_justify(true),
                                |ui| {
                                    ui.add(
                                        // The empty right text pushes the name to the left.
                                        egui::Button::selectable(on, name)
                                            .right_text("")
                                            .truncate(),
                                    )
                                },
                            )
                            .inner;
                        ui.allocate_ui_with_layout(
                            egui::vec2(size_w, row_h),
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                ui.set_min_width(size_w);
                                ui.add_space(12.0);
                                if !e.is_dir {
                                    ui.label(size_text(e.size));
                                }
                            },
                        );
                        ui.allocate_ui_with_layout(
                            egui::vec2(date_w, row_h),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.set_min_width(date_w);
                                ui.add_space(4.0);
                                ui.label(e.modified.map_or(String::new(), |t| {
                                    chrono::DateTime::from_timestamp(t as i64, 0)
                                        .map(|d| {
                                            d.with_timezone(&chrono::Local)
                                                .format("%Y-%m-%d %H:%M")
                                                .to_string()
                                        })
                                        .unwrap_or_default()
                                }));
                            },
                        );
                        ui.add_space(4.0);
                        ui.monospace(
                            e.permissions
                                .map(|m| mode_text(m, e.is_dir))
                                .unwrap_or_default(),
                        );
                        r
                    });
                    rows.push((e.name.clone(), line.response.rect));
                    let row = line.inner;
                    if row.clicked() {
                        if range && let Some(anchor) = &self.anchor {
                            // Shift+click: everything between the two rows.
                            let names: Vec<&String> = self
                                .entries
                                .iter()
                                .filter(|x| self.hidden || !x.name.starts_with('.'))
                                .map(|x| &x.name)
                                .collect();
                            let a = names.iter().position(|n| *n == anchor);
                            let b = names.iter().position(|n| **n == e.name);
                            if let (Some(a), Some(b)) = (a, b) {
                                let (lo, hi) = (a.min(b), a.max(b));
                                if !multi {
                                    self.selected.clear();
                                }
                                self.selected
                                    .extend(names[lo..=hi].iter().map(|n| (*n).clone()));
                            }
                        } else if multi {
                            if !self.selected.remove(&e.name) {
                                self.selected.insert(e.name.clone());
                            }
                            self.anchor = Some(e.name.clone());
                        } else {
                            self.selected = BTreeSet::from([e.name.clone()]);
                            self.anchor = Some(e.name.clone());
                        }
                    }
                    if row.double_clicked() {
                        if e.is_dir {
                            open_dir = Some(join(&self.dir, &e.name));
                        } else {
                            download = Some(e.name.clone());
                        }
                    }
                }
            });
        // Lines between the columns, and under the headers.
        let line = ui.visuals().widgets.noninteractive.bg_stroke;
        let (top, bottom) = (headers[0].top(), list.inner_rect.bottom());
        for cell in &headers[..3] {
            let x = cell.right() + gap / 2.0;
            ui.painter().vline(x, top..=bottom, line);
        }
        ui.painter().hline(
            headers[0].left()..=list.inner_rect.right(),
            headers[0].bottom() + ui.spacing().item_spacing.y / 2.0,
            line,
        );
        self.box_select(ui, list.inner_rect, &rows, multi);
        if let Some(d) = open_dir {
            self.go(&d);
        }
        if let Some(name) = download {
            self.start_downloads(vec![name]);
        }
        ui.weak("Drag or Shift+click to select several; drop files here to upload.")
            .on_hover_text(
                "Double-click a folder to open it and a file to download it. \
                 Ctrl/⌘ adds to the selection.",
            );

        if !self.transfers.is_empty() {
            ui.separator();
            let mut cancel = None;
            egui::ScrollArea::vertical()
                .id_salt(("sftp-transfers", self.id))
                .max_height(4.0 * 26.0)
                .show(ui, |ui| {
                    for t in self.transfers.iter().rev() {
                        ui.horizontal(|ui| {
                            ui.allocate_ui_with_layout(
                                egui::vec2(220.0, row_h),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.set_min_width(220.0);
                                    ui.add(egui::Label::new(&t.label).truncate());
                                },
                            );
                            match &t.state {
                                State::Running => {
                                    let fraction = if t.total > 0 {
                                        t.done as f32 / t.total as f32
                                    } else {
                                        0.0
                                    };
                                    ui.add(
                                        egui::ProgressBar::new(fraction).desired_width(260.0).text(
                                            format!(
                                                "{} of {}",
                                                size_text(t.done),
                                                size_text(t.total)
                                            ),
                                        ),
                                    );
                                    if ui.button("Cancel").clicked() {
                                        cancel = Some(t.id);
                                    }
                                }
                                State::Done => {
                                    ui.colored_label(
                                        egui::Color32::from_rgb(110, 200, 120),
                                        format!("done, {}", size_text(t.total)),
                                    );
                                }
                                State::Failed(e) => {
                                    ui.colored_label(ui.visuals().error_fg_color, e);
                                }
                            }
                        });
                    }
                });
            if let Some(id) = cancel {
                self.client.cancel(id);
            }
            if self
                .transfers
                .iter()
                .any(|t| !matches!(t.state, State::Running))
                && ui.button("Clear finished").clicked()
            {
                self.transfers.retain(|t| matches!(t.state, State::Running));
            }
        }
    }

    /// Pressing in the list and dragging draws a box; the rows it touches
    /// are selected (added to the selection with Ctrl/⌘).
    fn box_select(
        &mut self,
        ui: &egui::Ui,
        area: egui::Rect,
        rows: &[(String, egui::Rect)],
        keep: bool,
    ) {
        let (pressed, down, pos) = ui.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_down(),
                i.pointer.interact_pos(),
            )
        });
        let Some(pos) = pos else { return };
        if pressed && area.contains(pos) && ui.ui_contains_pointer() {
            let base = if keep {
                self.selected.clone()
            } else {
                BTreeSet::new()
            };
            self.drag_from = Some((pos, base));
            return;
        }
        if !down {
            self.drag_from = None;
            return;
        }
        let Some((from, base)) = &self.drag_from else {
            return;
        };
        // A small move is still a click.
        if from.distance(pos) < 6.0 {
            return;
        }
        let pos = pos.clamp(area.min, area.max);
        let band = egui::Rect::from_two_pos(*from, pos);
        let mut selected = base.clone();
        selected.extend(
            rows.iter()
                .filter(|(_, r)| r.intersects(band))
                .map(|(n, _)| n.clone()),
        );
        self.selected = selected;
        let color = ui.visuals().selection.bg_fill;
        let painter = ui.painter_at(area);
        painter.rect_filled(band, 2.0, color.gamma_multiply(0.25));
        painter.rect_stroke(
            band,
            2.0,
            egui::Stroke::new(1.0, color),
            egui::StrokeKind::Inside,
        );
        ui.ctx().request_repaint();
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(ask) = &mut self.ask else { return };
        let (enter, escape) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        let mut done = None;
        let title = match ask {
            Ask::Password { .. } => "Password",
            Ask::NewFolder(_) => "New folder",
            Ask::Rename { .. } => "Rename",
            Ask::Delete(_) => "Delete",
        };
        egui::Window::new(title)
            .id(egui::Id::unique(("sftp-ask", self.id)))
            // Above the browser, which keeps coming to the front.
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                match ask {
                    Ask::Password { why, text } => {
                        ui.label(why.as_str());
                        ui.add(
                            egui::TextEdit::singleline(&mut **text)
                                .password(true)
                                .desired_width(260.0),
                        )
                        .request_focus();
                    }
                    Ask::NewFolder(name) | Ask::Rename { to: name, .. } => {
                        ui.add(egui::TextEdit::singleline(name).desired_width(260.0))
                            .request_focus();
                    }
                    Ask::Delete(items) => {
                        let folders = items.iter().filter(|(_, d)| *d).count();
                        ui.label(if items.len() == 1 {
                            format!("Delete {}?", items[0].0)
                        } else {
                            format!("Delete {} items?", items.len())
                        });
                        if folders > 0 {
                            ui.colored_label(
                                ui.visuals().warn_fg_color,
                                "Folders are deleted with everything in them.",
                            );
                        }
                    }
                }
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let ok = match ask {
                            Ask::Password { .. } => "OK",
                            Ask::Delete(_) => "Delete",
                            Ask::Rename { .. } => "Rename",
                            Ask::NewFolder(_) => "Create",
                        };
                        if ui.button(ok).clicked() || enter {
                            done = Some(true);
                        }
                        if ui.button("Cancel").clicked() || escape {
                            done = Some(false);
                        }
                    });
                });
            });
        match done {
            Some(true) => match self.ask.take() {
                Some(Ask::Password { text, .. }) => {
                    self.client.set_password(&text);
                    // Once connected, what failed was a listing or a change.
                    if self.ready {
                        let dir = self.dir.clone();
                        self.go(&dir);
                    }
                }
                Some(Ask::NewFolder(name)) if !name.trim().is_empty() => {
                    self.client.mkdir(&join(&self.dir, name.trim()));
                }
                Some(Ask::Rename { from, to }) if !to.trim().is_empty() && to != from => {
                    self.client
                        .rename(&join(&self.dir, &from), &join(&self.dir, to.trim()));
                }
                Some(Ask::Delete(items)) => {
                    for (name, is_dir) in items {
                        self.client.remove(&join(&self.dir, &name), is_dir);
                    }
                    self.selected.clear();
                }
                _ => {}
            },
            Some(false) => {
                let password = matches!(self.ask.take(), Some(Ask::Password { .. }));
                if password && !self.ready {
                    self.error = Some("Not connected: the shell needs its password.".into());
                }
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_by_column_folders_first() {
        let e = |name: &str, dir: bool, size: u64, modified: u64| Entry {
            name: name.into(),
            is_dir: dir,
            is_link: false,
            size,
            modified: Some(modified),
            permissions: Some(if dir { 0o755 } else { 0o644 }),
        };
        let mut list = vec![
            e("b.log", false, 10, 3),
            e("etc", true, 0, 1),
            e("A.txt", false, 300, 1),
            e("bin", true, 0, 2),
            e("c.conf", false, 10, 2),
        ];
        let names = |l: &[Entry]| l.iter().map(|e| e.name.clone()).collect::<Vec<_>>();
        sort_entries(&mut list, SortBy::Name, true);
        assert_eq!(names(&list), ["bin", "etc", "A.txt", "b.log", "c.conf"]);
        sort_entries(&mut list, SortBy::Name, false);
        assert_eq!(names(&list), ["etc", "bin", "c.conf", "b.log", "A.txt"]);
        // Equal sizes by name; folders stay on top.
        sort_entries(&mut list, SortBy::Size, false);
        assert_eq!(names(&list), ["etc", "bin", "A.txt", "c.conf", "b.log"]);
        sort_entries(&mut list, SortBy::Modified, true);
        assert_eq!(names(&list), ["etc", "bin", "A.txt", "c.conf", "b.log"]);
    }

    #[test]
    fn sizes_and_modes() {
        assert_eq!(size_text(512), "512 B");
        assert_eq!(size_text(1536), "1.5 KB");
        assert_eq!(size_text(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(mode_text(0o755, true), "drwxr-xr-x");
        assert_eq!(mode_text(0o640, false), "-rw-r-----");
    }
}
