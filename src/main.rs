mod cleanup;
mod scan;
mod tree;
mod treemap;

use cleanup::{Candidate, Safety};
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use scan::{Progress, ScanResult};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, channel};
use std::sync::Arc;
use std::time::Duration;
use tree::{Tree, age_days, format_age, format_size};

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--report") {
        // Headless mode: macdirstat --report <path>
        let root = PathBuf::from(args.get(2).cloned().unwrap_or_else(|| ".".into()));
        let res = scan::scan(std::fs::canonicalize(&root).unwrap_or(root), Arc::new(Progress::default()));
        let t = &res.tree;
        println!(
            "Scanned {} files, {} in {:.2}s",
            t.node(0).files,
            format_size(t.node(0).size),
            res.elapsed.as_secs_f32()
        );
        let tm = std::time::Instant::now();
        let _ = treemap::build(t, 0, 2800, 1800);
        println!("Treemap render 2800x1800: {:.0} ms", tm.elapsed().as_secs_f64() * 1000.0);
        let cands = cleanup::find(t);
        let total: u64 = cands.iter().map(|c| c.size).sum();
        println!("{} cleanup candidates, {} total. Top 40:", cands.len(), format_size(total));
        for c in cands.iter().take(40) {
            let s = if c.safety == Safety::Safe { "SAFE  " } else { "REVIEW" };
            println!("{s} {:>9}  {:<24} {}", format_size(c.size), c.category, t.path(c.node).display());
        }
        return Ok(());
    }
    let initial = args.get(1).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("MacDirStat")
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([800.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native(
        "MacDirStat",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            let mut app = App::default();
            if let Some(p) = initial {
                app.start_scan(p);
            }
            Ok(Box::new(app))
        }),
    )
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Folders,
    Cleanup,
    Types,
}

struct Scanning {
    root: PathBuf,
    progress: Arc<Progress>,
    rx: Receiver<ScanResult>,
}

struct TreemapCache {
    key: (u32, usize, usize, u64),
    map: treemap::Treemap,
    tex: egui::TextureHandle,
}

struct PendingDelete {
    nodes: Vec<u32>,
    permanent: bool,
}

struct Deleting {
    rx: Receiver<Vec<(u32, Result<(), String>)>>,
    count: usize,
}

struct App {
    scanning: Option<Scanning>,
    tree: Option<Tree>,
    scan_secs: f32,
    view_root: u32,
    selected: Option<u32>,
    hovered: Option<u32>,
    expanded: HashSet<u32>,
    rows: Vec<(u32, u16)>,
    rows_dirty: bool,
    scroll_to_selected: bool,
    treemap: Option<TreemapCache>,
    generation: u64,
    candidates: Vec<Candidate>,
    tab: Tab,
    min_size_mb: f64,
    min_age_days: i64,
    show_review: bool,
    confirm: Option<PendingDelete>,
    deleting: Option<Deleting>,
    status: String,
    disk: Option<(u64, u64)>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            scanning: None,
            tree: None,
            scan_secs: 0.0,
            view_root: 0,
            selected: None,
            hovered: None,
            expanded: HashSet::new(),
            rows: Vec::new(),
            rows_dirty: true,
            scroll_to_selected: false,
            treemap: None,
            generation: 0,
            candidates: Vec::new(),
            tab: Tab::Cleanup,
            min_size_mb: 10.0,
            min_age_days: 0,
            show_review: true,
            confirm: None,
            deleting: None,
            status: String::new(),
            disk: disk_space("/"),
        }
    }
}

fn disk_space(path: &str) -> Option<(u64, u64)> {
    let c = std::ffi::CString::new(path).ok()?;
    unsafe {
        let mut s: libc::statfs = std::mem::zeroed();
        if libc::statfs(c.as_ptr(), &mut s) != 0 {
            return None;
        }
        let bs = s.f_bsize as u64;
        Some((s.f_blocks as u64 * bs, s.f_bavail as u64 * bs))
    }
}

fn reveal_in_finder(p: &std::path::Path) {
    let _ = std::process::Command::new("open").arg("-R").arg(p).spawn();
}

fn open_path(p: &std::path::Path) {
    let _ = std::process::Command::new("open").arg(p).spawn();
}

impl App {
    fn start_scan(&mut self, root: PathBuf) {
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        let progress = Arc::new(Progress::default());
        let (tx, rx) = channel();
        let p2 = progress.clone();
        let r2 = root.clone();
        std::thread::spawn(move || {
            let res = scan::scan(r2, p2);
            let _ = tx.send(res);
        });
        self.scanning = Some(Scanning { root, progress, rx });
        self.status.clear();
    }

    fn finish_scan(&mut self, res: ScanResult) {
        let tree = res.tree;
        self.scan_secs = res.elapsed.as_secs_f32();
        self.candidates = cleanup::find(&tree);
        self.expanded.clear();
        self.expanded.insert(0);
        self.view_root = 0;
        self.selected = None;
        self.hovered = None;
        self.rows_dirty = true;
        self.treemap = None;
        self.generation += 1;
        self.status = format!(
            "Scanned {} files in {:.1}s",
            tree.node(0).files,
            self.scan_secs
        );
        self.disk = disk_space(&tree.root_path.to_string_lossy()).or_else(|| disk_space("/"));
        self.tree = Some(tree);
    }

    fn select(&mut self, idx: u32, scroll: bool) {
        self.selected = Some(idx);
        if let Some(t) = &self.tree {
            for a in t.ancestors(idx) {
                if self.expanded.insert(a) {
                    self.rows_dirty = true;
                }
            }
        }
        if scroll {
            self.scroll_to_selected = true;
        }
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        let Some(t) = &self.tree else { return };
        let mut stack: Vec<(u32, u16)> = vec![(0, 0)];
        while let Some((idx, depth)) = stack.pop() {
            self.rows.push((idx, depth));
            if self.expanded.contains(&idx) {
                let kids: Vec<u32> = t.children(idx).collect();
                for &c in kids.iter().rev() {
                    stack.push((c, depth + 1));
                }
            }
        }
        self.rows_dirty = false;
    }

    fn request_delete(&mut self, nodes: Vec<u32>, permanent: bool) {
        if nodes.is_empty() {
            return;
        }
        self.confirm = Some(PendingDelete { nodes, permanent });
    }

    fn do_delete(&mut self, pd: PendingDelete) {
        let Some(t) = &self.tree else { return };
        let trash_dir = cleanup::home_dir().map(|h| h.join(".Trash"));
        let jobs: Vec<(u32, PathBuf, bool)> = pd
            .nodes
            .iter()
            .filter(|&&n| n != 0 && !t.is_deleted(n))
            .map(|&n| {
                let p = t.path(n);
                // Items already in the Trash can only be deleted permanently.
                let perm = pd.permanent || trash_dir.as_ref().is_some_and(|d| p.starts_with(d));
                (n, p, perm)
            })
            .collect();
        let count = jobs.len();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            use trash::macos::{DeleteMethod, TrashContextExtMacos};
            let mut ctx = trash::TrashContext::default();
            ctx.set_delete_method(DeleteMethod::NsFileManager);
            let results: Vec<(u32, Result<(), String>)> = jobs
                .into_iter()
                .map(|(n, p, perm)| {
                    let r = if perm {
                        if p.is_dir() && !p.is_symlink() {
                            std::fs::remove_dir_all(&p)
                        } else {
                            std::fs::remove_file(&p)
                        }
                        .map_err(|e| format!("{}: {e}", p.display()))
                    } else {
                        ctx.delete(&p).map_err(|e| format!("{}: {e}", p.display()))
                    };
                    (n, r)
                })
                .collect();
            let _ = tx.send(results);
        });
        self.deleting = Some(Deleting { rx, count });
    }

    fn finish_delete(&mut self, results: Vec<(u32, Result<(), String>)>) {
        let Some(t) = &mut self.tree else { return };
        let mut freed = 0u64;
        let mut errors = Vec::new();
        let mut ok = 0;
        for (n, r) in results {
            match r {
                Ok(()) => {
                    freed += t.node(n).size;
                    t.remove(n);
                    ok += 1;
                }
                Err(e) => errors.push(e),
            }
        }
        self.candidates.retain(|c| !t.is_deleted(c.node));
        if let Some(s) = self.selected {
            if t.is_deleted(s) {
                self.selected = None;
            }
        }
        while self.view_root != 0 && t.is_deleted(self.view_root) {
            self.view_root = t.node(self.view_root).parent;
        }
        self.generation += 1;
        self.rows_dirty = true;
        self.disk = disk_space(&t.root_path.to_string_lossy()).or_else(|| disk_space("/"));
        self.status = format!("Removed {ok} item(s), {} freed.", format_size(freed));
        if !errors.is_empty() {
            self.status += &format!(" {} error(s): {}", errors.len(), errors[0]);
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Poll background work.
        if let Some(s) = &self.scanning {
            if let Ok(res) = s.rx.try_recv() {
                self.scanning = None;
                self.finish_scan(res);
            } else {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
        if let Some(d) = &self.deleting {
            if let Ok(res) = d.rx.try_recv() {
                self.deleting = None;
                self.finish_delete(res);
            } else {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
        if self.rows_dirty {
            self.rebuild_rows();
        }

        self.toolbar(ui);
        self.status_bar(ui);

        if let Some(s) = &self.scanning {
            let progress = s.progress.clone();
            let root = s.root.clone();
            egui::CentralPanel::default().show(ui, |ui| {
                scanning_ui(ui, &root, &progress);
            });
        } else if self.tree.is_none() {
            egui::CentralPanel::default().show(ui, |ui| self.welcome(ui));
        } else {
            egui::Panel::left("side")
                .resizable(true)
                .default_size(620.0)
                .size_range(320.0..=1400.0)
                .show(ui, |ui| self.side_panel(ui));
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| self.treemap_ui(ui));
        }

        self.confirm_dialog(&ctx);
    }
}

fn scanning_ui(ui: &mut egui::Ui, root: &std::path::Path, p: &Progress) {
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() * 0.3);
        ui.heading(format!("Scanning {}", root.display()));
        ui.add_space(10.0);
        ui.spinner();
        ui.add_space(10.0);
        ui.label(format!(
            "{} files  ·  {} folders  ·  {}",
            p.files.load(Ordering::Relaxed),
            p.dirs.load(Ordering::Relaxed),
            format_size(p.bytes.load(Ordering::Relaxed))
        ));
        if let Ok(c) = p.current.lock() {
            ui.weak(c.as_str());
        }
        ui.add_space(10.0);
        if ui.button("Cancel").clicked() {
            p.cancel.store(true, Ordering::Relaxed);
        }
    });
}

impl App {
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                let busy = self.scanning.is_some();
                ui.add_enabled_ui(!busy, |ui| {
                    if ui.button("Scan Home").clicked() {
                        if let Some(h) = cleanup::home_dir() {
                            self.start_scan(h);
                        }
                    }
                    if ui.button("Scan Whole Disk").clicked() {
                        self.start_scan(PathBuf::from("/"));
                    }
                    if ui.button("Choose Folder…").clicked() {
                        if let Some(p) = rfd::FileDialog::new().pick_folder() {
                            self.start_scan(p);
                        }
                    }
                    if let Some(t) = &self.tree {
                        if ui.button("Rescan").clicked() {
                            let p = t.root_path.clone();
                            self.start_scan(p);
                        }
                    }
                });
                if let Some(t) = &self.tree {
                    ui.separator();
                    let can_up = self.view_root != 0;
                    if ui.add_enabled(can_up, egui::Button::new("Up")).clicked() {
                        self.view_root = t.node(self.view_root).parent;
                    }
                    if ui.add_enabled(can_up, egui::Button::new("Top")).clicked() {
                        self.view_root = 0;
                    }
                    if let Some(s) = self.selected {
                        let target = if t.node(s).is_dir { s } else { t.node(s).parent };
                        if ui
                            .add_enabled(target != self.view_root, egui::Button::new("Zoom to Selection"))
                            .clicked()
                        {
                            self.view_root = target;
                        }
                    }
                    ui.label(
                        egui::RichText::new(t.path(self.view_root).display().to_string()).weak(),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some((total, free)) = self.disk {
                        let used = total.saturating_sub(free);
                        let frac = used as f32 / total.max(1) as f32;
                        ui.add(
                            egui::ProgressBar::new(frac)
                                .desired_width(160.0)
                                .text(format!("{} free of {}", format_size(free), format_size(total))),
                        );
                        ui.label("Disk:");
                    }
                });
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(d) = &self.deleting {
                    ui.spinner();
                    ui.label(format!("Deleting {} item(s)…", d.count));
                } else if let (Some(t), Some(h)) = (&self.tree, self.hovered) {
                    let n = t.node(h);
                    ui.label(format!("{}  —  {}", t.path(h).display(), format_size(n.size)));
                } else {
                    ui.label(&self.status);
                }
            });
        });
    }

    fn welcome(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.25);
            ui.heading(egui::RichText::new("MacDirStat").size(32.0));
            ui.label("See what is taking up space on your Mac, and find things that are safe to remove.");
            ui.add_space(20.0);
            let bs = Vec2::new(260.0, 36.0);
            if ui.add_sized(bs, egui::Button::new("Scan Home Folder (recommended)")).clicked() {
                if let Some(h) = cleanup::home_dir() {
                    self.start_scan(h);
                }
            }
            if ui.add_sized(bs, egui::Button::new("Scan Whole Disk  ( / )")).clicked() {
                self.start_scan(PathBuf::from("/"));
            }
            if ui.add_sized(bs, egui::Button::new("Choose Folder…")).clicked() {
                if let Some(p) = rfd::FileDialog::new().pick_folder() {
                    self.start_scan(p);
                }
            }
            ui.add_space(20.0);
            ui.weak("Tip: for complete results give MacDirStat \"Full Disk Access\" in");
            ui.weak("System Settings → Privacy & Security → Full Disk Access.");
        });
    }

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tab, Tab::Cleanup, "Cleanup Suggestions");
            ui.selectable_value(&mut self.tab, Tab::Folders, "Folders");
            ui.selectable_value(&mut self.tab, Tab::Types, "File Types");
        });
        ui.separator();
        match self.tab {
            Tab::Folders => self.folders_ui(ui),
            Tab::Cleanup => self.cleanup_ui(ui),
            Tab::Types => self.types_ui(ui),
        }
    }

    fn context_menu(&mut self, ui: &mut egui::Ui, idx: u32) {
        let Some(t) = &self.tree else { return };
        let path = t.path(idx);
        let is_dir = t.node(idx).is_dir;
        if ui.button("Reveal in Finder").clicked() {
            reveal_in_finder(&path);
            ui.close();
        }
        if ui.button("Open").clicked() {
            open_path(&path);
            ui.close();
        }
        if ui.button("Copy Path").clicked() {
            ui.ctx().copy_text(path.display().to_string());
            ui.close();
        }
        if is_dir && ui.button("Zoom Treemap Here").clicked() {
            self.view_root = idx;
            ui.close();
        }
        ui.separator();
        if idx != 0 {
            if ui.button("Move to Trash…").clicked() {
                self.request_delete(vec![idx], false);
                ui.close();
            }
            if ui.button("Delete Permanently…").clicked() {
                self.request_delete(vec![idx], true);
                ui.close();
            }
        }
    }

    fn folders_ui(&mut self, ui: &mut egui::Ui) {
        let Some(t) = &self.tree else { return };
        let row_h = 20.0;
        let total_rows = self.rows.len();
        let col_size = 90.0;
        let col_pct = 70.0;
        let col_files = 80.0;
        let col_age = 60.0;
        let right_cols = col_size + col_pct + col_files + col_age;

        // Header
        let (hr, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::hover());
        {
            let p = ui.painter();
            let font = egui::FontId::proportional(12.0);
            let c = ui.visuals().weak_text_color();
            p.text(hr.left_center() + Vec2::new(4.0, 0.0), egui::Align2::LEFT_CENTER, "Name", font.clone(), c);
            let mut x = hr.right() - right_cols;
            for (label, w) in [("Size", col_size), ("% of parent", col_pct), ("Files", col_files), ("Modified", col_age)] {
                p.text(Pos2::new(x + w - 6.0, hr.center().y), egui::Align2::RIGHT_CENTER, label, font.clone(), c);
                x += w;
            }
        }

        let mut scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if self.scroll_to_selected {
            if let Some(s) = self.selected {
                if let Some(pos) = self.rows.iter().position(|r| r.0 == s) {
                    let spacing = ui.spacing().item_spacing.y;
                    let offset = (pos as f32 * (row_h + spacing) - ui.available_height() / 2.0).max(0.0);
                    scroll = scroll.vertical_scroll_offset(offset);
                }
            }
            self.scroll_to_selected = false;
        }

        let mut toggle: Option<u32> = None;
        let mut select: Option<u32> = None;
        let mut ctx_menu_target: Option<(egui::Response, u32)> = None;
        let selected = self.selected;
        let rows = &self.rows;
        let expanded = &self.expanded;

        scroll.show_rows(ui, row_h, total_rows, |ui, range| {
            for &(idx, depth) in &rows[range] {
                let n = t.node(idx);
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::click());
                let p = ui.painter();
                if selected == Some(idx) {
                    p.rect_filled(rect, 2.0, ui.visuals().selection.bg_fill);
                } else if resp.hovered() {
                    p.rect_filled(rect, 2.0, ui.visuals().widgets.hovered.weak_bg_fill);
                }
                let font = egui::FontId::proportional(13.0);
                let text_c = ui.visuals().text_color();
                let indent = 4.0 + depth as f32 * 14.0;
                let has_kids = n.is_dir && n.child_count > 0;
                let tri_rect = Rect::from_center_size(
                    Pos2::new(rect.left() + indent + 6.0, rect.center().y),
                    Vec2::splat(12.0),
                );
                if has_kids {
                    let open = expanded.contains(&idx);
                    p.text(
                        tri_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        if open { "⏷" } else { "⏵" },
                        egui::FontId::proportional(11.0),
                        ui.visuals().weak_text_color(),
                    );
                }
                // color swatch for files
                let name_x = rect.left() + indent + 16.0;
                let name_clip = Rect::from_min_max(
                    Pos2::new(name_x, rect.top()),
                    Pos2::new(rect.right() - right_cols - 4.0, rect.bottom()),
                );
                if !n.is_dir {
                    let c = t.color(idx);
                    p.rect_filled(
                        Rect::from_center_size(Pos2::new(name_x - 6.0 + 2.0, rect.center().y), Vec2::splat(8.0)),
                        2.0,
                        Color32::from_rgb(c[0], c[1], c[2]),
                    );
                }
                let name: &str = if idx == 0 { &t.root_path.to_str().unwrap_or("/") } else { &n.name };
                let label = if n.is_dir { format!("{name}/") } else { name.to_string() };
                p.with_clip_rect(name_clip).text(
                    Pos2::new(name_x + 4.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    label,
                    font.clone(),
                    text_c,
                );
                // right columns
                let mut x = rect.right() - right_cols;
                let cy = rect.center().y;
                p.text(Pos2::new(x + col_size - 6.0, cy), egui::Align2::RIGHT_CENTER, format_size(n.size), font.clone(), text_c);
                x += col_size;
                let parent_size = if idx == 0 { n.size } else { t.node(n.parent).size };
                let frac = if parent_size > 0 { n.size as f32 / parent_size as f32 } else { 0.0 };
                let bar = Rect::from_min_size(Pos2::new(x + 4.0, cy - 5.0), Vec2::new(col_pct - 10.0, 10.0));
                p.rect_filled(bar, 2.0, Color32::from_gray(50));
                p.rect_filled(
                    Rect::from_min_size(bar.min, Vec2::new(bar.width() * frac, bar.height())),
                    2.0,
                    Color32::from_rgb(90, 140, 230),
                );
                x += col_pct;
                p.text(Pos2::new(x + col_files - 6.0, cy), egui::Align2::RIGHT_CENTER, n.files.to_string(), font.clone(), ui.visuals().weak_text_color());
                x += col_files;
                p.text(Pos2::new(x + col_age - 6.0, cy), egui::Align2::RIGHT_CENTER, format_age(age_days(n.mtime)), font.clone(), ui.visuals().weak_text_color());

                if resp.clicked() {
                    let on_tri = resp.interact_pointer_pos().is_some_and(|pp| pp.x < name_x);
                    if on_tri && has_kids {
                        toggle = Some(idx);
                    } else {
                        select = Some(idx);
                    }
                }
                if resp.double_clicked() && has_kids {
                    toggle = Some(idx);
                }
                if resp.secondary_clicked() {
                    select = Some(idx);
                }
                if resp.context_menu_opened() || resp.secondary_clicked() {
                    ctx_menu_target = Some((resp, idx));
                }
            }
        });

        if let Some(idx) = toggle {
            if !self.expanded.remove(&idx) {
                self.expanded.insert(idx);
            }
            self.rows_dirty = true;
        }
        if let Some(idx) = select {
            self.selected = Some(idx);
        }
        if let Some((resp, idx)) = ctx_menu_target {
            resp.context_menu(|ui| self.context_menu(ui, idx));
        }
    }

    fn cleanup_ui(&mut self, ui: &mut egui::Ui) {
        if self.tree.is_none() {
            return;
        }
        let min_size = (self.min_size_mb * 1_000_000.0) as u64;
        let visible: Vec<usize> = self
            .candidates
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                c.size >= min_size
                    && c.age_days >= self.min_age_days
                    && (self.show_review || c.safety == Safety::Safe)
            })
            .map(|(i, _)| i)
            .collect();
        let total_safe: u64 = visible
            .iter()
            .map(|&i| &self.candidates[i])
            .filter(|c| c.safety == Safety::Safe)
            .map(|c| c.size)
            .sum();
        let total_review: u64 = visible
            .iter()
            .map(|&i| &self.candidates[i])
            .filter(|c| c.safety == Safety::Review)
            .map(|c| c.size)
            .sum();
        let sel: Vec<u32> = self.candidates.iter().filter(|c| c.selected).map(|c| c.node).collect();
        let sel_size: u64 = self.candidates.iter().filter(|c| c.selected).map(|c| c.size).sum();

        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(format!("{} safe to remove", format_size(total_safe))).color(Color32::from_rgb(110, 200, 120)).strong());
            ui.label("·");
            ui.label(egui::RichText::new(format!("{} worth reviewing", format_size(total_review))).color(Color32::from_rgb(230, 170, 80)).strong());
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Min size");
            ui.add(egui::DragValue::new(&mut self.min_size_mb).range(0.0..=100_000.0).speed(1.0).suffix(" MB"));
            ui.label("Not modified for at least");
            ui.add(egui::DragValue::new(&mut self.min_age_days).range(0..=3650).suffix(" days"));
            ui.checkbox(&mut self.show_review, "Show \"Review\" items");
        });
        ui.horizontal_wrapped(|ui| {
            if ui.button("Select all Safe").clicked() {
                for &i in &visible {
                    let c = &mut self.candidates[i];
                    c.selected = c.safety == Safety::Safe;
                }
            }
            if ui.button("Select none").clicked() {
                for c in &mut self.candidates {
                    c.selected = false;
                }
            }
            let has_sel = !sel.is_empty() && self.deleting.is_none();
            if ui
                .add_enabled(has_sel, egui::Button::new(format!("Move {} to Trash", format_size(sel_size))))
                .clicked()
            {
                self.request_delete(sel.clone(), false);
            }
            if ui
                .add_enabled(has_sel, egui::Button::new("Delete Permanently"))
                .clicked()
            {
                self.request_delete(sel.clone(), true);
            }
        });
        ui.separator();

        if visible.is_empty() {
            ui.weak("Nothing found with the current filters.");
            return;
        }

        let Some(t) = &self.tree else { return };
        let home = cleanup::home_dir().map(|h| h.display().to_string()).unwrap_or_default();
        let row_h = 38.0;
        let mut select: Option<u32> = None;
        let mut menu: Option<(egui::Response, u32)> = None;
        let selected = self.selected;
        let candidates = &mut self.candidates;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, row_h, visible.len(), |ui, range| {
                for &ci in &visible[range] {
                    let c = &mut candidates[ci];
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::click());
                    let p = ui.painter().clone();
                    if selected == Some(c.node) {
                        p.rect_filled(rect, 3.0, ui.visuals().selection.bg_fill);
                    } else if resp.hovered() {
                        p.rect_filled(rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill);
                    }
                    // checkbox
                    let cb_rect = Rect::from_min_size(rect.min + Vec2::new(4.0, 9.0), Vec2::splat(20.0));
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(cb_rect));
                    child.checkbox(&mut c.selected, "");

                    let x0 = rect.left() + 30.0;
                    let (badge, bc) = match c.safety {
                        Safety::Safe => ("SAFE", Color32::from_rgb(70, 160, 90)),
                        Safety::Review => ("REVIEW", Color32::from_rgb(200, 140, 50)),
                    };
                    let badge_rect = Rect::from_min_size(Pos2::new(x0, rect.top() + 4.0), Vec2::new(54.0, 15.0));
                    p.rect_filled(badge_rect, 3.0, bc);
                    p.text(badge_rect.center(), egui::Align2::CENTER_CENTER, badge, egui::FontId::proportional(10.0), Color32::WHITE);
                    let size_c = ui.visuals().strong_text_color();
                    p.text(Pos2::new(x0 + 62.0, rect.top() + 11.0), egui::Align2::LEFT_CENTER, format_size(c.size), egui::FontId::proportional(13.0), size_c);
                    let age_txt = format!("{} · modified {}", c.category, format_age(c.age_days));
                    p.text(Pos2::new(x0 + 140.0, rect.top() + 11.0), egui::Align2::LEFT_CENTER, age_txt, egui::FontId::proportional(13.0), ui.visuals().text_color());
                    let path = t.path(c.node).display().to_string();
                    let short = if !home.is_empty() && path.starts_with(&home) { format!("~{}", &path[home.len()..]) } else { path };
                    p.with_clip_rect(rect).text(Pos2::new(x0, rect.top() + 28.0), egui::Align2::LEFT_CENTER, short, egui::FontId::monospace(11.0), ui.visuals().weak_text_color());

                    let resp = resp.on_hover_text(&c.reason);
                    if resp.clicked() || resp.secondary_clicked() {
                        select = Some(c.node);
                    }
                    if resp.double_clicked() {
                        c.selected = !c.selected;
                    }
                    if resp.context_menu_opened() || resp.secondary_clicked() {
                        menu = Some((resp, c.node));
                    }
                }
            });
        if let Some(idx) = select {
            self.select(idx, false);
        }
        if let Some((resp, idx)) = menu {
            resp.context_menu(|ui| self.context_menu(ui, idx));
        }
    }

    fn types_ui(&mut self, ui: &mut egui::Ui) {
        let Some(t) = &self.tree else { return };
        let mut order: Vec<usize> = (0..t.exts.len()).filter(|&i| t.exts[i].bytes > 0).collect();
        order.sort_by(|&a, &b| t.exts[b].bytes.cmp(&t.exts[a].bytes));
        let total = t.node(0).size.max(1);
        let row_h = 20.0;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, row_h, order.len(), |ui, range| {
                for &i in &order[range] {
                    let e = &t.exts[i];
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::hover());
                    let p = ui.painter();
                    let cy = rect.center().y;
                    p.rect_filled(
                        Rect::from_center_size(Pos2::new(rect.left() + 10.0, cy), Vec2::new(14.0, 12.0)),
                        2.0,
                        Color32::from_rgb(e.color[0], e.color[1], e.color[2]),
                    );
                    let f = egui::FontId::proportional(13.0);
                    let tc = ui.visuals().text_color();
                    p.text(Pos2::new(rect.left() + 24.0, cy), egui::Align2::LEFT_CENTER, format!(".{}", e.name), f.clone(), tc);
                    p.text(Pos2::new(rect.left() + 220.0, cy), egui::Align2::RIGHT_CENTER, format_size(e.bytes), f.clone(), tc);
                    p.text(
                        Pos2::new(rect.left() + 290.0, cy),
                        egui::Align2::RIGHT_CENTER,
                        format!("{:.1}%", e.bytes as f64 * 100.0 / total as f64),
                        f.clone(),
                        ui.visuals().weak_text_color(),
                    );
                    p.text(
                        Pos2::new(rect.left() + 390.0, cy),
                        egui::Align2::RIGHT_CENTER,
                        format!("{} files", e.count),
                        f,
                        ui.visuals().weak_text_color(),
                    );
                }
            });
    }

    fn treemap_ui(&mut self, ui: &mut egui::Ui) {
        let Some(t) = &self.tree else { return };
        let rect = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(rect, Sense::click());
        let ppp = ui.ctx().pixels_per_point();
        let w = (rect.width() * ppp).round().max(1.0) as usize;
        let h = (rect.height() * ppp).round().max(1.0) as usize;
        let key = (self.view_root, w, h, self.generation);

        if self.treemap.as_ref().map(|c| c.key) != Some(key) {
            let map = treemap::build(t, self.view_root, w, h);
            let mut rgba = Vec::with_capacity(w * h * 4);
            for px in &map.pixels {
                rgba.extend_from_slice(px);
            }
            let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
            let opts = egui::TextureOptions::NEAREST;
            match &mut self.treemap {
                Some(cache) => {
                    cache.tex.set(img, opts);
                    cache.map = map;
                    cache.key = key;
                }
                None => {
                    let tex = ui.ctx().load_texture("treemap", img, opts);
                    self.treemap = Some(TreemapCache { key, map, tex });
                }
            }
        }
        let cache = self.treemap.as_ref().unwrap();
        let painter = ui.painter_at(rect);
        painter.image(
            cache.tex.id(),
            rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );

        let to_screen = |r: &treemap::R| {
            Rect::from_min_max(
                Pos2::new(rect.left() + r.x0 as f32 / ppp, rect.top() + r.y0 as f32 / ppp),
                Pos2::new(rect.left() + r.x1 as f32 / ppp, rect.top() + r.y1 as f32 / ppp),
            )
        };

        // Selection highlight (fall back to nearest drawn ancestor).
        if let Some(s) = self.selected {
            let mut cur = s;
            loop {
                if let Some(r) = cache.map.rects.get(&cur) {
                    let sr = to_screen(r);
                    painter.rect_stroke(sr, 0.0, Stroke::new(3.0, Color32::BLACK), StrokeKind::Inside);
                    painter.rect_stroke(sr, 0.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
                    break;
                }
                if cur == self.view_root || cur == u32::MAX {
                    break;
                }
                cur = t.node(cur).parent;
            }
        }

        self.hovered = None;
        if let Some(pos) = resp.hover_pos() {
            let px = ((pos.x - rect.left()) * ppp) as usize;
            let py = ((pos.y - rect.top()) * ppp) as usize;
            if let Some(id) = cache.map.id_at(px, py) {
                self.hovered = Some(id);
                // outline hovered item's parent folder lightly
                if let Some(r) = cache.map.rects.get(&id) {
                    painter.rect_stroke(to_screen(r), 0.0, Stroke::new(1.0, Color32::from_white_alpha(180)), StrokeKind::Inside);
                }
            }
        }

        if let Some(h) = self.hovered {
            let n = t.node(h);
            let path = t.path(h);
            let resp2 = resp.clone();
            resp2.on_hover_ui_at_pointer(|ui| {
                ui.strong(n.name.as_ref());
                ui.label(format_size(n.size));
                ui.weak(path.display().to_string());
            });
        }

        if resp.clicked() || resp.secondary_clicked() {
            if let Some(h) = self.hovered {
                self.select(h, true);
                self.tab = if self.tab == Tab::Types { Tab::Types } else { self.tab };
            }
        }
        if resp.double_clicked() {
            // Zoom into the child of the current root that contains the clicked item.
            if let (Some(h), Some(t)) = (self.hovered, &self.tree) {
                let mut cur = h;
                while t.node(cur).parent != self.view_root && t.node(cur).parent != u32::MAX {
                    cur = t.node(cur).parent;
                }
                if t.node(cur).is_dir && cur != self.view_root {
                    self.view_root = cur;
                }
            }
        }
        if let Some(sel) = self.selected {
            resp.context_menu(|ui| self.context_menu(ui, sel));
        }
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(pd) = &self.confirm else { return };
        let Some(t) = &self.tree else {
            self.confirm = None;
            return;
        };
        let size: u64 = pd.nodes.iter().map(|&n| t.node(n).size).sum();
        let mut decision: Option<bool> = None;
        egui::Modal::new(egui::Id::new("confirm_delete")).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.heading(if pd.permanent { "Delete permanently?" } else { "Move to Trash?" });
            ui.add_space(6.0);
            ui.label(format!("{} item(s), {} total:", pd.nodes.len(), format_size(size)));
            egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                for &n in pd.nodes.iter().take(200) {
                    ui.monospace(t.path(n).display().to_string());
                }
                if pd.nodes.len() > 200 {
                    ui.weak(format!("…and {} more", pd.nodes.len() - 200));
                }
            });
            ui.add_space(6.0);
            if pd.permanent {
                ui.colored_label(Color32::from_rgb(230, 100, 90), "This cannot be undone.");
            } else {
                ui.weak("Space is freed once you empty the Trash.");
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    decision = Some(false);
                }
                let label = if pd.permanent { "Delete" } else { "Move to Trash" };
                if ui.button(egui::RichText::new(label).strong()).clicked() {
                    decision = Some(true);
                }
            });
        });
        match decision {
            Some(true) => {
                let pd = self.confirm.take().unwrap();
                self.do_delete(pd);
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }
}
