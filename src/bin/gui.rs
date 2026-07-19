//! `ablemod-gui` — a graphical front-end: open a tracker module or VGM/VGZ file, pick what to
//! do with it (convert to an Ableton Live Set, extract samples/MIDI/WAV stems, live-preview a
//! VGM/VGZ's chip channels, or render that preview to a video file), point-and-click instead of
//! typing CLI flags.
//!
//! This binary never calls into `ablemod`'s own `export::`/`formats::` modules directly — every
//! action shells out to the `ablemod` CLI binary itself (see `cli_binary_path`), on a background
//! thread so the UI stays responsive, reporting back over a channel. Two reasons, not one:
//! keeping all the actual conversion/rendering logic in exactly one place (this binary is a thin
//! launcher, nothing here can drift out of sync with what the CLI does), and sidestepping a real
//! macOS constraint — SDL2 (`ablemod preview`'s own toolkit) and eframe's winit backend each
//! expect to own the process' main thread run loop, which two GUI toolkits sharing one process
//! can't both do. Running `preview` as its own separate OS process, with its own main thread,
//! avoids that entirely.
//!
//! Feature-gated behind `gui` (see Cargo.toml's own comment) — a default `cargo build` of the
//! CLI never needs egui/eframe/rfd.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, Sender};

use eframe::egui;
use egui::{Color32, RichText, Rounding, Stroke};

/// Result of one background action: the subcommand name (for the status line) and either the
/// CLI's own combined stdout+stderr on success, or an error message (a nonzero exit is treated
/// as failure even if the process itself launched fine — same convention `ablemod`'s own exit
/// code uses).
enum ActionResult {
    Success { action: String, output: String },
    Failure { action: String, message: String },
}

/// Locates the `ablemod` CLI binary next to this one (how `cargo build` lays out multiple
/// `[[bin]]` targets from the same package) — falling back to bare `"ablemod"` so it still
/// works if this GUI is ever invoked from somewhere that only has the CLI on PATH.
fn cli_binary_path() -> PathBuf {
    let name = if cfg!(windows) { "ablemod.exe" } else { "ablemod" };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    PathBuf::from(name)
}

fn is_vgm_path(path: &Path) -> bool {
    matches!(path.extension().and_then(|e| e.to_str()).map(|s| s.to_ascii_lowercase()).as_deref(), Some("vgm") | Some("vgz"))
}

fn is_supported_path(path: &Path) -> bool {
    is_vgm_path(path)
        || matches!(path.extension().and_then(|e| e.to_str()).map(|s| s.to_ascii_lowercase()).as_deref(), Some("mod" | "xm" | "s3m"))
}

/// Runs `ablemod <args>` to completion on a background thread, sending the result back once
/// it's done — `action` is just a human-readable label for the status line, not itself part of
/// the command.
fn spawn_action(action: &str, args: Vec<String>, tx: Sender<ActionResult>) {
    let action = action.to_string();
    std::thread::spawn(move || {
        let result = Command::new(cli_binary_path()).args(&args).output();
        let message = match result {
            Ok(output) => {
                let combined = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
                if output.status.success() {
                    ActionResult::Success { action, output: combined }
                } else {
                    ActionResult::Failure { action, message: if combined.trim().is_empty() { output.status.to_string() } else { combined } }
                }
            }
            Err(e) => ActionResult::Failure { action, message: format!("failed to launch ablemod: {e}") },
        };
        // The receiving end only ever drops if the GUI window itself has already closed —
        // nothing left to report status to at that point, so a failed send is not an error.
        let _ = tx.send(message);
    });
}

/// Runs `ablemod preview <file> [--record <path>]` — unlike every other action, launched
/// *without* waiting for it to exit when there's no `--record` (the live preview is its own
/// long-running, interactive window; this GUI has nothing useful to wait for or report once
/// it's launched). `--record` still runs to completion in the background, same as any other
/// action, so its own success/failure reaches the status line.
fn spawn_preview(file: &Path, record_to: Option<&Path>, tx: Sender<ActionResult>) {
    if let Some(out) = record_to {
        let args = vec!["preview".to_string(), file.display().to_string(), "--record".to_string(), out.display().to_string()];
        spawn_action("Export Video", args, tx);
        return;
    }
    let file = file.to_path_buf();
    std::thread::spawn(move || {
        let result = Command::new(cli_binary_path()).args(["preview", &file.display().to_string()]).spawn();
        let message = match result {
            Ok(_child) => ActionResult::Success { action: "Preview".to_string(), output: "opened a preview window".to_string() },
            Err(e) => ActionResult::Failure { action: "Preview".to_string(), message: format!("failed to launch ablemod: {e}") },
        };
        let _ = tx.send(message);
    });
}

// A small, self-contained design system — one place to tune the whole app's look. Dark by
// design (matches `ablemod preview`'s own dark canvas, so the two feel like one product), with
// an accent blue reused verbatim from `preview.rs`'s own TRACK_COLORS[0] for brand consistency
// across both binaries.
mod theme {
    use super::Color32;

    pub const BG: Color32 = Color32::from_rgb(24, 25, 29);
    pub const PANEL: Color32 = Color32::from_rgb(32, 34, 39);
    pub const PANEL_BORDER: Color32 = Color32::from_rgb(52, 55, 61);
    pub const CONSOLE_BG: Color32 = Color32::from_rgb(18, 19, 22);
    pub const ACCENT: Color32 = Color32::from_rgb(90, 170, 255); // preview.rs's TRACK_COLORS[0]
    pub const TEXT: Color32 = Color32::from_rgb(225, 227, 230);
    pub const TEXT_MUTED: Color32 = Color32::from_rgb(140, 144, 152);
    pub const SUCCESS: Color32 = Color32::from_rgb(120, 220, 140); // TRACK_COLORS[2]
    pub const ERROR: Color32 = Color32::from_rgb(230, 110, 110);
    pub const VGM_BADGE: Color32 = Color32::from_rgb(90, 170, 255);
    pub const TRACKER_BADGE: Color32 = Color32::from_rgb(255, 140, 90); // TRACK_COLORS[1]
}

/// Builds a small 64x64 RGBA app icon procedurally (no bundled asset needed): a rounded dark
/// square with five vertical bars of varying height in the same palette `preview.rs` draws its
/// own waveform cells with — literally a tiny frozen oscilloscope grid, so the icon reads as
/// "this app" at a glance rather than a generic placeholder.
fn generate_icon() -> egui::IconData {
    const SIZE: usize = 64;
    const CORNER_RADIUS: f32 = 14.0;
    let bars = [
        Color32::from_rgb(90, 170, 255),
        Color32::from_rgb(255, 140, 90),
        Color32::from_rgb(120, 220, 140),
        Color32::from_rgb(240, 210, 90),
        Color32::from_rgb(170, 150, 255),
    ];
    let bar_heights = [0.45f32, 0.75, 0.30, 0.90, 0.55]; // fraction of the icon's own height

    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let idx = (y * SIZE + x) * 4;
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // Distance outside the rounded-rect boundary, in pixels (0 = on the edge) — used
            // both to cut the corners and to antialias them a little rather than leaving jaggies.
            let dx = (CORNER_RADIUS - fx.min(SIZE as f32 - fx)).max(0.0);
            let dy = (CORNER_RADIUS - fy.min(SIZE as f32 - fy)).max(0.0);
            let corner_dist = (dx * dx + dy * dy).sqrt();
            let coverage = (CORNER_RADIUS - corner_dist + 1.0).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                continue; // fully outside the rounded corner: stays transparent
            }

            let bar_count = bars.len();
            let margin = SIZE as f32 * 0.14;
            let usable = SIZE as f32 - margin * 2.0;
            let gap = usable * 0.12 / (bar_count - 1) as f32;
            let bar_w = (usable - gap * (bar_count - 1) as f32) / bar_count as f32;

            let mut pixel = theme::BG;
            for (i, &color) in bars.iter().enumerate() {
                let bar_x0 = margin + i as f32 * (bar_w + gap);
                let bar_x1 = bar_x0 + bar_w;
                if fx < bar_x0 || fx >= bar_x1 {
                    continue;
                }
                let bar_h = (SIZE as f32 - margin * 2.0) * bar_heights[i];
                let bar_y0 = SIZE as f32 - margin - bar_h;
                let bar_y1 = SIZE as f32 - margin;
                if fy >= bar_y0 && fy < bar_y1 {
                    pixel = color;
                }
            }

            let alpha = (255.0 * coverage) as u8;
            rgba[idx] = pixel.r();
            rgba[idx + 1] = pixel.g();
            rgba[idx + 2] = pixel.b();
            rgba[idx + 3] = alpha;
        }
    }
    egui::IconData { rgba, width: SIZE as u32, height: SIZE as u32 }
}

/// Sets the whole app's visual style once at startup — dark background/panel colors, rounded
/// corners, and an accent-blue selection/hover color, applied globally so every widget (this
/// app hand-styles very few individually) picks it up automatically.
fn setup_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = theme::BG;
    visuals.window_fill = theme::BG;
    visuals.extreme_bg_color = theme::CONSOLE_BG;
    visuals.override_text_color = Some(theme::TEXT);
    visuals.widgets.noninteractive.bg_fill = theme::PANEL;
    visuals.widgets.inactive.bg_fill = theme::PANEL;
    visuals.widgets.inactive.weak_bg_fill = theme::PANEL;
    visuals.widgets.hovered.bg_fill = theme::PANEL_BORDER;
    visuals.widgets.active.bg_fill = theme::ACCENT;
    visuals.selection.bg_fill = theme::ACCENT.gamma_multiply(0.35);
    visuals.selection.stroke = Stroke::new(1.0, theme::ACCENT);
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
    ] {
        widget.rounding = Rounding::same(6.0);
    }
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    ctx.set_style(style);
}

#[derive(Clone, Copy, PartialEq)]
enum StatusKind {
    Idle,
    Running,
    Success,
    Error,
}

struct GuiApp {
    file_path: Option<PathBuf>,
    status: String,
    status_kind: StatusKind,
    busy: bool,
    result_rx: Option<Receiver<ActionResult>>,
}

impl Default for GuiApp {
    fn default() -> Self {
        GuiApp { file_path: None, status: String::new(), status_kind: StatusKind::Idle, busy: false, result_rx: None }
    }
}

impl GuiApp {
    /// Starts a background action, wiring up the channel this frame (and every frame after,
    /// until it reports back) polls for completion.
    fn run(&mut self, action: &str, args: Vec<String>) {
        let (tx, rx) = std::sync::mpsc::channel();
        spawn_action(action, args, tx);
        self.result_rx = Some(rx);
        self.busy = true;
        self.status = format!("Running {action}…");
        self.status_kind = StatusKind::Running;
    }

    fn run_preview(&mut self, file: &Path, record_to: Option<&Path>) {
        let (tx, rx) = std::sync::mpsc::channel();
        spawn_preview(file, record_to, tx);
        self.result_rx = Some(rx);
        self.busy = true;
        self.status = if record_to.is_some() { "Rendering video…".to_string() } else { "Opening preview window…".to_string() };
        self.status_kind = StatusKind::Running;
    }

    fn open_file_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Tracker / VGM", &["mod", "MOD", "xm", "XM", "s3m", "S3M", "vgm", "VGM", "vgz", "VGZ"])
            .pick_file()
        {
            self.load_file(path);
        }
    }

    fn load_file(&mut self, path: PathBuf) {
        self.file_path = Some(path);
        self.status.clear();
        self.status_kind = StatusKind::Idle;
    }

    /// A save dialog pre-populated with `<input file's own stem><suffix>` next to the input
    /// file — a starting point, not a hard default; the dialog lets the user go anywhere.
    /// Directory and file name are set as two separate rfd calls deliberately: `set_file_name`
    /// expects a bare name, not a path — passing it a full path once produced a mangled
    /// save-file result (macOS's NSSavePanel silently rewrites '/' inside what it treats as a
    /// single file-name field to ':', a legacy classic-Mac-path artifact), confirmed against a
    /// real run before this split.
    fn save_dialog(&self, suffix: &str) -> rfd::FileDialog {
        let path = self.file_path.as_ref().expect("only called once a file is loaded");
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
        let mut dialog = rfd::FileDialog::new().set_file_name(format!("{stem}{suffix}"));
        if let Some(dir) = path.parent() {
            dialog = dialog.set_directory(dir);
        }
        dialog
    }

    /// A full-width action button with a short label and a smaller, muted description below
    /// it — reads as a proper menu item rather than a bare `ui.button`, and gives every action
    /// enough room to explain itself without needing a separate tooltip.
    fn action_button(&self, ui: &mut egui::Ui, label: &str, description: &str) -> bool {
        let frame = egui::Frame::none()
            .fill(theme::PANEL)
            .stroke(Stroke::new(1.0, theme::PANEL_BORDER))
            .rounding(Rounding::same(6.0))
            .inner_margin(egui::Margin::symmetric(12.0, 8.0));
        // The frame's own rect — not `ui.min_rect()` after the fact, which returns the
        // *cumulative* bounding box of everything drawn in this `ui` so far (this function
        // is called once per action, all sharing one outer `ui`). Using that cumulative rect
        // as the click hitbox made every button's clickable area also cover every button
        // drawn above it, so clicking the first button in a section could also register as a
        // click on a later one — confirmed live: clicking "Convert to Ableton Live Set…" was
        // also firing "Extract MIDI…"'s own action.
        let frame_response = frame
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.vertical(|ui| {
                    ui.label(RichText::new(label).color(theme::TEXT).strong());
                    ui.label(RichText::new(description).color(theme::TEXT_MUTED).size(11.5));
                });
            })
            .response;
        let rect = frame_response.rect;
        let response = ui.interact(rect, ui.id().with(label), egui::Sense::click());
        if response.hovered() {
            ui.painter().rect_stroke(rect, Rounding::same(6.0), Stroke::new(1.0, theme::ACCENT));
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        response.clicked()
    }

    fn section_label(&self, ui: &mut egui::Ui, text: &str) {
        ui.add_space(10.0);
        ui.label(RichText::new(text.to_uppercase()).color(theme::TEXT_MUTED).size(11.0).strong());
        ui.add_space(2.0);
    }
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(rx) = &self.result_rx {
            if let Ok(result) = rx.try_recv() {
                self.busy = false;
                self.result_rx = None;
                match result {
                    ActionResult::Success { action, output } => {
                        let trimmed = output.trim();
                        self.status = if trimmed.is_empty() { format!("{action}: done.") } else { format!("{action}: done.\n{trimmed}") };
                        self.status_kind = StatusKind::Success;
                    }
                    ActionResult::Failure { action, message } => {
                        self.status = format!("{action} failed:\n{}", message.trim());
                        self.status_kind = StatusKind::Error;
                    }
                }
            } else {
                // Still running — repaint next frame too, so the status line's own "Running…"
                // doesn't sit stale until some unrelated input event happens to wake the UI.
                ctx.request_repaint();
            }
        }

        // Drag-and-drop: dropping a supported file anywhere in the window loads it, same as
        // using the Open dialog — a standard desktop-app convenience, and a natural fit next
        // to a file-picker-centric workflow like this one.
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if let Some(path) = dropped.into_iter().find_map(|f| f.path).filter(|p| is_supported_path(p)) {
            if !self.busy {
                self.load_file(path);
            }
        }
        let hovering_file = ctx.input(|i| !i.raw.hovered_files.is_empty());

        egui::TopBottomPanel::top("menu_bar").frame(egui::Frame::none().fill(theme::PANEL).inner_margin(egui::Margin::symmetric(8.0, 4.0))).show(
            ctx,
            |ui| {
                egui::menu::bar(ui, |ui| {
                    ui.menu_button("File", |ui| {
                        if ui.add_enabled(!self.busy, egui::Button::new("Open…")).clicked() {
                            self.open_file_dialog();
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Quit").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                });
            },
        );

        egui::CentralPanel::default().frame(egui::Frame::none().fill(theme::BG).inner_margin(egui::Margin::same(20.0))).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("ablemod").color(theme::TEXT).size(22.0).strong());
                ui.add_space(6.0);
                ui.label(RichText::new("tracker & chiptune -> Ableton Live").color(theme::TEXT_MUTED).italics());
            });
            ui.add_space(14.0);

            let Some(path) = self.file_path.clone() else {
                // Empty state: a big dashed drop zone doubling as an "Open file" button — makes
                // drag-and-drop discoverable instead of a hidden feature only power users find.
                let zone_h = 160.0;
                let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), zone_h), egui::Sense::click());
                let stroke_color = if hovering_file { theme::ACCENT } else { theme::PANEL_BORDER };
                ui.painter().rect_stroke(
                    rect,
                    Rounding::same(10.0),
                    Stroke::new(if hovering_file { 2.0 } else { 1.5 }, stroke_color),
                );
                let text_color = if hovering_file { theme::ACCENT } else { theme::TEXT_MUTED };
                ui.painter().text(
                    rect.center() - egui::vec2(0.0, 10.0),
                    egui::Align2::CENTER_CENTER,
                    "Drop a file here, or click to open",
                    egui::FontId::proportional(15.0),
                    text_color,
                );
                ui.painter().text(
                    rect.center() + egui::vec2(0.0, 12.0),
                    egui::Align2::CENTER_CENTER,
                    ".mod / .xm / .s3m tracker modules · .vgm / .vgz chiptune rips",
                    egui::FontId::proportional(11.5),
                    theme::TEXT_MUTED,
                );
                if response.clicked() {
                    self.open_file_dialog();
                }
                if response.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                return;
            };

            // File card: name + a colored type badge, so the loaded file and what ablemod will
            // do with it are both legible at a glance rather than a bare path string.
            let vgm = is_vgm_path(&path);
            let (badge_text, badge_color) =
                if vgm { ("VGM / VGZ · chip-emulated audio", theme::VGM_BADGE) } else { ("Tracker module", theme::TRACKER_BADGE) };
            egui::Frame::none()
                .fill(theme::PANEL)
                .stroke(Stroke::new(1.0, theme::PANEL_BORDER))
                .rounding(Rounding::same(8.0))
                .inner_margin(egui::Margin::symmetric(14.0, 10.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(path.file_name().and_then(|n| n.to_str()).unwrap_or("?")).color(theme::TEXT).strong().size(14.0));
                            ui.label(RichText::new(path.display().to_string()).color(theme::TEXT_MUTED).size(11.0));
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            egui::Frame::none().fill(badge_color.gamma_multiply(0.18)).rounding(Rounding::same(10.0)).inner_margin(
                                egui::Margin::symmetric(9.0, 3.0),
                            ).show(ui, |ui| {
                                ui.label(RichText::new(badge_text).color(badge_color).size(11.5).strong());
                            });
                            if ui.add_enabled(!self.busy, egui::Button::new("Change…")).clicked() {
                                self.open_file_dialog();
                            }
                        });
                    });
                });

            ui.add_enabled_ui(!self.busy, |ui| {
                self.section_label(ui, "Convert");
                if self.action_button(ui, "Convert to Ableton Live Set…", "One .als project — Sampler tracks for a module, rendered-audio tracks for a chip rip.")
                {
                    if let Some(out) = self.save_dialog(".als").save_file() {
                        self.run("Convert to ALS", vec!["convert-als".into(), path.display().to_string(), "-o".into(), out.display().to_string()]);
                    }
                }

                if vgm {
                    self.section_label(ui, "Extract");
                    if self.action_button(ui, "Extract mixed track (WAV)…", "The full chip-emulated mix, rendered down to a single stereo file.") {
                        if let Some(out) = self.save_dialog("_mix.wav").save_file() {
                            self.run(
                                "Extract mixed track",
                                vec!["extract-mixed-tracks".into(), path.display().to_string(), "-o".into(), out.display().to_string()],
                            );
                        }
                    }
                    if self.action_button(ui, "Extract separated tracks (WAV)…", "One WAV per chip channel, isolated by muting every other channel.") {
                        if let Some(out) = rfd::FileDialog::new().pick_folder() {
                            self.run(
                                "Extract separated tracks",
                                vec!["extract-separated-tracks-wav".into(), path.display().to_string(), "-o".into(), out.display().to_string()],
                            );
                        }
                    }

                    self.section_label(ui, "Preview");
                    if self.action_button(ui, "Preview (live)", "Opens a live, audio-driven window with one scrolling waveform cell per channel.") {
                        self.run_preview(&path, None);
                    }
                    if self.action_button(ui, "Export video…", "Renders one playthrough of that same preview to an .mp4 file via ffmpeg.") {
                        if let Some(out) = self.save_dialog(".mp4").save_file() {
                            self.run_preview(&path, Some(&out));
                        }
                    }
                } else {
                    self.section_label(ui, "Extract");
                    if self.action_button(ui, "Extract samples…", "Every stored instrument sample, dumped as its own WAV file.") {
                        if let Some(out) = rfd::FileDialog::new().pick_folder() {
                            self.run("Extract samples", vec!["extract-samples".into(), path.display().to_string(), "-o".into(), out.display().to_string()]);
                        }
                    }
                    if self.action_button(ui, "Extract MIDI…", "The module's patterns transcribed to a .mid file, one track per sample.") {
                        if let Some(out) = self.save_dialog(".mid").save_file() {
                            self.run("Extract MIDI", vec!["extract-midi".into(), path.display().to_string(), "-o".into(), out.display().to_string()]);
                        }
                    }
                }
            });

            ui.add_space(12.0);
            let status_color = match self.status_kind {
                StatusKind::Idle => theme::TEXT_MUTED,
                StatusKind::Running => theme::ACCENT,
                StatusKind::Success => theme::SUCCESS,
                StatusKind::Error => theme::ERROR,
            };
            egui::Frame::none()
                .fill(theme::CONSOLE_BG)
                .stroke(Stroke::new(1.0, theme::PANEL_BORDER))
                .rounding(Rounding::same(6.0))
                .inner_margin(egui::Margin::same(10.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        // A drawn dot rather than a Unicode glyph (●/✓/✗): egui's bundled
                        // default font doesn't cover those code points and silently falls back
                        // to a tofu box, confirmed by seeing exactly that during testing.
                        let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 18.0), egui::Sense::hover());
                        if !matches!(self.status_kind, StatusKind::Idle) {
                            ui.painter().circle_filled(dot_rect.center(), 4.0, status_color);
                        }
                        if self.busy {
                            ui.add(egui::Spinner::new().color(theme::ACCENT).size(14.0));
                        }
                        egui::ScrollArea::vertical().max_height(150.0).show(ui, |ui| {
                            if self.status.is_empty() {
                                ui.label(RichText::new("Ready.").color(theme::TEXT_MUTED).monospace());
                            } else {
                                ui.label(RichText::new(&self.status).color(status_color).monospace());
                            }
                        });
                    });
                });
        });
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([620.0, 560.0])
            .with_min_inner_size([480.0, 420.0])
            .with_icon(std::sync::Arc::new(generate_icon())),
        ..Default::default()
    };
    eframe::run_native(
        "ablemod",
        options,
        Box::new(|cc| {
            setup_style(&cc.egui_ctx);
            Ok(Box::new(GuiApp::default()))
        }),
    )
}
