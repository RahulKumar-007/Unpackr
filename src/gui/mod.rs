use anyhow::Result;
use std::path::PathBuf;

#[cfg(not(feature = "gui"))]
pub fn run_gui(_initial_archive: Option<PathBuf>) -> Result<()> {
    anyhow::bail!("GUI support requires compiling with `--features gui`");
}

#[cfg(feature = "gui")]
pub fn run_gui(initial_archive: Option<PathBuf>) -> Result<()> {
    gui_impl::launch(initial_archive)
}

#[cfg(feature = "gui")]
mod gui_impl {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use anyhow::Result;
    use eframe::egui::{
        self, Align, Align2, Color32, FontFamily, FontId, Frame, Layout, Margin, RichText, Rounding,
        ScrollArea, Stroke, TextEdit, Vec2,
    };

    use crate::archive::{CompressionMethod, EntryState, ZipArchiveInspection, ZipInspector};
    use crate::cli::bench::{compute_benchmark_report, BenchmarkReport};
    use crate::cli::inspect::format_bytes;
    use crate::extraction::{
        CollisionPolicy, ExtractionEngine, ExtractionOptions, ExtractionProgress,
        ExtractionSummary, ResumeOptions, DEFAULT_MAX_ENTRIES,
    };
    use crate::state::job::{find_manifest_file, global_jobs_dir};
    use crate::state::manifest::ExtractionManifest;
    use crate::state::tracker::{StateTracker, VerificationFailure};

    // Design System Spacing Scale
    const SPACE_XS: f32 = 4.0;
    const SPACE_SM: f32 = 8.0;
    const SPACE_MD: f32 = 14.0;
    const SPACE_LG: f32 = 20.0;

    // Design System Typography Scale
    const FONT_LABEL: f32 = 11.5;
    const FONT_BODY: f32 = 13.0;
    const FONT_TITLE: f32 = 15.0;
    const FONT_METRIC: f32 = 16.5;

    // Minimalist Palette Constants (Dark Mode Defaults)
    const BG_DARK: Color32 = Color32::from_rgb(13, 15, 19);
    const CARD_BG: Color32 = Color32::from_rgb(20, 23, 29);
    const CARD_ELEVATED: Color32 = Color32::from_rgb(27, 31, 40);
    const BORDER_SUBTLE: Color32 = Color32::from_rgb(44, 48, 60);
    const TEXT_PRIMARY: Color32 = Color32::from_rgb(243, 244, 246);
    const TEXT_MUTED: Color32 = Color32::from_rgb(148, 163, 184);
    const TEXT_DIM: Color32 = Color32::from_rgb(100, 116, 139);

    const ACCENT_EMERALD: Color32 = Color32::from_rgb(16, 185, 129);
    const ACCENT_INDIGO: Color32 = Color32::from_rgb(99, 102, 241);
    const ACCENT_CYAN: Color32 = Color32::from_rgb(6, 182, 212);
    const ACCENT_AMBER: Color32 = Color32::from_rgb(245, 158, 11);
    const ACCENT_ROSE: Color32 = Color32::from_rgb(239, 68, 68);

    #[derive(Debug, Clone, Copy)]
    pub struct ThemePalette {
        pub bg_dark: Color32,
        pub card_bg: Color32,
        pub card_elevated: Color32,
        pub border_subtle: Color32,
        pub text_primary: Color32,
        pub text_muted: Color32,
        pub text_dim: Color32,
    }

    impl ThemePalette {
        pub fn dark() -> Self {
            Self {
                bg_dark: Color32::from_rgb(13, 15, 19),
                card_bg: Color32::from_rgb(20, 23, 29),
                card_elevated: Color32::from_rgb(27, 31, 40),
                border_subtle: Color32::from_rgb(44, 48, 60),
                text_primary: Color32::from_rgb(243, 244, 246),
                text_muted: Color32::from_rgb(148, 163, 184),
                text_dim: Color32::from_rgb(100, 116, 139),
            }
        }

        pub fn light() -> Self {
            Self {
                bg_dark: Color32::from_rgb(245, 247, 250),
                card_bg: Color32::from_rgb(255, 255, 255),
                card_elevated: Color32::from_rgb(240, 243, 248),
                border_subtle: Color32::from_rgb(222, 226, 233),
                text_primary: Color32::from_rgb(15, 23, 42),
                text_muted: Color32::from_rgb(71, 85, 105),
                text_dim: Color32::from_rgb(148, 163, 184),
            }
        }
    }

    fn recent_archives_path() -> Option<PathBuf> {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|h| h.join(".unpackr").join("recent.json"))
    }

    fn load_recent_archives() -> Vec<PathBuf> {
        if let Some(path) = recent_archives_path() {
            if path.is_file() {
                if let Ok(data) = fs::read_to_string(&path) {
                    if let Ok(list) = serde_json::from_str::<Vec<PathBuf>>(&data) {
                        return list.into_iter().filter(|p| p.is_file()).take(8).collect();
                    }
                }
            }
        }
        Vec::new()
    }

    fn save_recent_archives(list: &[PathBuf]) {
        if let Some(path) = recent_archives_path() {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string_pretty(&list) {
                let _ = fs::write(path, json);
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ActiveTab {
        Extract,
        Jobs,
        Benchmark,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum EntryFilter {
        All,
        FilesOnly,
        DirsOnly,
        SymlinksOnly,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum EntrySortKey {
        Index,
        Path,
        Method,
        Compressed,
        Uncompressed,
        Ratio,
        Crc32,
        Offset,
    }

    #[derive(Debug, Clone)]
    enum BackgroundEvent {
        Idle,
        Extracting(Option<ExtractionProgress>),
        ExtractDone(Result<ExtractionSummary, String>),
        Benchmarking,
        BenchmarkDone(Result<BenchmarkReport, String>),
        Verifying,
        VerifyDone {
            job_id: String,
            failures: Vec<VerificationFailure>,
        },
    }

    #[derive(Debug, Clone)]
    struct JobEntryItem {
        manifest_path: PathBuf,
        manifest: ExtractionManifest,
    }

    pub struct UnpackrApp {
        active_tab: ActiveTab,

        // Extract & Inspect state
        archive_path_input: String,
        dest_path_input: String,
        inspection: Option<ZipArchiveInspection>,
        inspection_error: Option<String>,

        // Extraction Options
        reclaim_archive: bool,
        enable_sparse: bool,
        collision_policy: CollisionPolicy,
        max_compression_ratio: f64,
        max_entries_limit: usize,
        max_total_size_mb: u64, // 0 means unlimited
        max_file_size_mb: u64,  // 0 means unlimited

        // Entry table filters & sorting
        entry_search: String,
        entry_filter: EntryFilter,
        sort_key: EntrySortKey,
        sort_ascending: bool,

        // Background task state
        bg_state: Arc<Mutex<BackgroundEvent>>,
        last_summary: Option<ExtractionSummary>,
        status_banner: Option<(String, Color32)>,
        cancel_token: Option<Arc<AtomicBool>>,

        // History & Theming
        recent_archives: Vec<PathBuf>,
        is_dark_mode: bool,

        // Jobs tab state
        jobs: Vec<JobEntryItem>,
        resume_retry_failed: bool,
        resume_verify_existing: bool,
        resume_reclaim: bool,
        last_verification: Option<(String, Vec<VerificationFailure>)>,

        // Benchmark tab state
        bench_custom_archive: String,
        bench_entries: usize,
        bench_size_mb: usize,
        last_bench_report: Option<BenchmarkReport>,
        bench_history: Vec<BenchmarkReport>,
    }

    impl UnpackrApp {
        pub fn new(cc: &eframe::CreationContext<'_>, initial_archive: Option<PathBuf>) -> Self {
            configure_minimalist_theme(&cc.egui_ctx, true);

            let mut app = Self {
                active_tab: ActiveTab::Extract,
                archive_path_input: String::new(),
                dest_path_input: String::new(),
                inspection: None,
                inspection_error: None,
                reclaim_archive: false,
                enable_sparse: true,
                collision_policy: CollisionPolicy::Fail,
                max_compression_ratio: 100.0,
                max_entries_limit: DEFAULT_MAX_ENTRIES,
                max_total_size_mb: 0,
                max_file_size_mb: 0,
                entry_search: String::new(),
                entry_filter: EntryFilter::All,
                sort_key: EntrySortKey::Index,
                sort_ascending: true,
                bg_state: Arc::new(Mutex::new(BackgroundEvent::Idle)),
                last_summary: None,
                status_banner: None,
                cancel_token: None,
                recent_archives: load_recent_archives(),
                is_dark_mode: true,
                jobs: Vec::new(),
                resume_retry_failed: true,
                resume_verify_existing: false,
                resume_reclaim: false,
                last_verification: None,
                bench_custom_archive: String::new(),
                bench_entries: 6,
                bench_size_mb: 3,
                last_bench_report: None,
                bench_history: Vec::new(),
            };

            if let Some(path) = initial_archive {
                app.load_archive(path);
            }
            app.refresh_jobs();
            app
        }

        fn theme(&self) -> ThemePalette {
            if self.is_dark_mode {
                ThemePalette::dark()
            } else {
                ThemePalette::light()
            }
        }

        fn card_frame(&self) -> Frame {
            let p = self.theme();
            Frame::none()
                .fill(p.card_bg)
                .stroke(Stroke::new(1.0_f32, p.border_subtle))
                .rounding(Rounding::same(8.0))
                .inner_margin(Margin::symmetric(18.0, 14.0))
        }

        fn metric_tile(
            &self,
            ui: &mut egui::Ui,
            label: &str,
            value: &str,
            value_color: Color32,
        ) {
            let p = self.theme();
            let resp = Frame::none()
                .fill(p.card_elevated)
                .rounding(Rounding::same(6.0))
                .inner_margin(Margin::symmetric(12.0, 9.0))
                .stroke(Stroke::new(1.0_f32, p.border_subtle))
                .show(ui, |ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(label).size(FONT_LABEL).color(p.text_muted));
                        ui.add_space(2.0);
                        ui.label(RichText::new(value).size(FONT_METRIC).strong().color(value_color));
                    });
                });
            if resp.response.interact(egui::Sense::click()).on_hover_text(format!("Click to copy \"{}\"", value)).clicked() {
                ui.output_mut(|o| o.copied_text = value.to_string());
            }
        }

        fn is_busy(&self) -> bool {
            if let Ok(guard) = self.bg_state.lock() {
                matches!(
                    *guard,
                    BackgroundEvent::Extracting(_)
                        | BackgroundEvent::Benchmarking
                        | BackgroundEvent::Verifying
                )
            } else {
                false
            }
        }

        fn load_archive(&mut self, path: PathBuf) {
            self.archive_path_input = path.display().to_string();
            // Auto-suggest destination folder next to the archive if empty or previous default
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("extracted_output");
            let parent = path.parent().unwrap_or_else(|| Path::new("."));
            self.dest_path_input = parent.join(stem).display().to_string();

            // Record in recent archives history
            self.recent_archives.retain(|p| p != &path);
            self.recent_archives.insert(0, path.clone());
            self.recent_archives.truncate(8);
            save_recent_archives(&self.recent_archives);

            match ZipInspector::inspect(&path) {
                Ok(insp) => {
                    self.inspection = Some(insp);
                    self.inspection_error = None;
                    self.status_banner = None;
                }
                Err(e) => {
                    self.inspection = None;
                    self.inspection_error = Some(format!("Failed to inspect archive: {}", e));
                }
            }
        }

        fn refresh_jobs(&mut self) {
            let mut discovered = Vec::new();
            let mut seen_job_ids = std::collections::HashSet::new();

            // 1. Check current destination if set
            if !self.dest_path_input.trim().is_empty() {
                let dest_manifest = PathBuf::from(self.dest_path_input.trim())
                    .join(".unpackr")
                    .join("manifest.json");
                if dest_manifest.is_file() {
                    if let Ok(manifest) = ExtractionManifest::load(&dest_manifest) {
                        seen_job_ids.insert(manifest.job_id.clone());
                        discovered.push(JobEntryItem {
                            manifest_path: dest_manifest,
                            manifest,
                        });
                    }
                }
            }

            // 2. Scan global jobs directory (~/.unpackr/jobs)
            if let Some(global_dir) = global_jobs_dir() {
                if global_dir.is_dir() {
                    if let Ok(entries) = fs::read_dir(global_dir) {
                        for entry in entries.flatten() {
                            let job_dir = entry.path();
                            let global_manifest_path = job_dir.join("manifest.json");
                            if !global_manifest_path.is_file() {
                                continue;
                            }
                            if let Ok(global_manifest) =
                                ExtractionManifest::load(&global_manifest_path)
                            {
                                let dest_manifest = global_manifest
                                    .destination
                                    .join(".unpackr")
                                    .join("manifest.json");

                                // If the destination's .unpackr/manifest.json no longer exists on disk
                                // (e.g. ephemeral cargo test / benchmark tempdir or deleted output folder),
                                // prune the orphaned global job record automatically.
                                if !dest_manifest.is_file() {
                                    let _ = fs::remove_dir_all(&job_dir);
                                    continue;
                                }

                                if seen_job_ids.contains(&global_manifest.job_id) {
                                    continue;
                                }

                                // Load the authoritative manifest from the destination folder
                                if let Ok(authoritative) = ExtractionManifest::load(&dest_manifest)
                                {
                                    seen_job_ids.insert(authoritative.job_id.clone());
                                    discovered.push(JobEntryItem {
                                        manifest_path: dest_manifest,
                                        manifest: authoritative,
                                    });
                                }
                            }
                        }
                    }
                }
            }

            // Sort newest updated first
            discovered.sort_by_key(|a| std::cmp::Reverse(a.manifest.updated_at_unix));
            self.jobs = discovered;
        }

        fn start_extraction(&mut self) {
            if self.is_busy() {
                return;
            }
            let archive_path = PathBuf::from(self.archive_path_input.trim());
            let destination = PathBuf::from(self.dest_path_input.trim());
            if archive_path.as_os_str().is_empty() || destination.as_os_str().is_empty() {
                self.status_banner = Some((
                    "Please specify both an archive file and a destination directory.".to_string(),
                    ACCENT_AMBER,
                ));
                return;
            }

            let options = ExtractionOptions {
                destination,
                collision_policy: self.collision_policy,
                enable_sparse: self.enable_sparse,
                max_compression_ratio: self.max_compression_ratio,
                reclaim_archive: self.reclaim_archive,
                state_dir: None,
                verbose: false,
                quiet: true,
                max_total_size: if self.max_total_size_mb > 0 {
                    Some(self.max_total_size_mb * 1024 * 1024)
                } else {
                    None
                },
                max_file_size: if self.max_file_size_mb > 0 {
                    Some(self.max_file_size_mb * 1024 * 1024)
                } else {
                    None
                },
                max_entries: Some(self.max_entries_limit.max(1)),
            };

            let cancel_token = Arc::new(AtomicBool::new(false));
            self.cancel_token = Some(Arc::clone(&cancel_token));

            let bg_state = Arc::clone(&self.bg_state);
            if let Ok(mut guard) = bg_state.lock() {
                *guard = BackgroundEvent::Extracting(None);
            }
            self.last_summary = None;
            self.status_banner = None;

            thread::spawn(move || {
                let progress_ref = Arc::clone(&bg_state);
                let result =
                    ExtractionEngine::extract_with_progress_and_cancel(&archive_path, &options, move |prog| {
                        if let Ok(mut guard) = progress_ref.lock() {
                            *guard = BackgroundEvent::Extracting(Some(prog));
                        }
                    }, Some(cancel_token));

                if let Ok(mut guard) = bg_state.lock() {
                    *guard = BackgroundEvent::ExtractDone(result.map_err(|e| e.to_string()));
                }
            });
        }

        fn start_resume(&mut self, target: String) {
            if self.is_busy() {
                return;
            }
            let options = ResumeOptions {
                destination_override: None,
                archive_override: None,
                retry_failed: self.resume_retry_failed,
                verify_existing: self.resume_verify_existing,
                collision_policy: Some(self.collision_policy),
                enable_sparse: self.enable_sparse,
                max_compression_ratio: self.max_compression_ratio,
                reclaim_archive: self.resume_reclaim,
                verbose: false,
                quiet: true,
                max_total_size: None,
                max_file_size: None,
                max_entries: Some(self.max_entries_limit.max(1)),
            };

            let cancel_token = Arc::new(AtomicBool::new(false));
            self.cancel_token = Some(Arc::clone(&cancel_token));

            let bg_state = Arc::clone(&self.bg_state);
            if let Ok(mut guard) = bg_state.lock() {
                *guard = BackgroundEvent::Extracting(None);
            }
            self.status_banner = None;

            thread::spawn(move || {
                let progress_ref = Arc::clone(&bg_state);
                let result =
                    ExtractionEngine::resume_with_progress_and_cancel(&target, &options, move |prog| {
                        if let Ok(mut guard) = progress_ref.lock() {
                            *guard = BackgroundEvent::Extracting(Some(prog));
                        }
                    }, Some(cancel_token));

                if let Ok(mut guard) = bg_state.lock() {
                    *guard = BackgroundEvent::ExtractDone(result.map_err(|e| e.to_string()));
                }
            });
        }

        fn start_verify(&mut self, manifest_path: PathBuf, job_id: String) {
            if self.is_busy() {
                return;
            }
            let bg_state = Arc::clone(&self.bg_state);
            if let Ok(mut guard) = bg_state.lock() {
                *guard = BackgroundEvent::Verifying;
            }

            thread::spawn(move || {
                let failures = match StateTracker::load_from_file(&manifest_path) {
                    Ok(tracker) => tracker.verify_extracted_output(),
                    Err(e) => vec![VerificationFailure {
                        entry_name: "manifest.json".to_string(),
                        path: manifest_path,
                        reason: format!("Failed to load manifest: {}", e),
                    }],
                };
                if let Ok(mut guard) = bg_state.lock() {
                    *guard = BackgroundEvent::VerifyDone { job_id, failures };
                }
            });
        }

        fn start_benchmark(&mut self) {
            if self.is_busy() {
                return;
            }
            let custom_archive = if self.bench_custom_archive.trim().is_empty() {
                None
            } else {
                Some(PathBuf::from(self.bench_custom_archive.trim()))
            };
            let entries = self.bench_entries;
            let size_mb = self.bench_size_mb;

            let bg_state = Arc::clone(&self.bg_state);
            if let Ok(mut guard) = bg_state.lock() {
                *guard = BackgroundEvent::Benchmarking;
            }
            self.status_banner = None;

            thread::spawn(move || {
                let res = compute_benchmark_report(custom_archive, entries, size_mb, true, false);
                if let Ok(mut guard) = bg_state.lock() {
                    *guard = BackgroundEvent::BenchmarkDone(res.map_err(|e| e.to_string()));
                }
            });
        }

        fn poll_background_events(&mut self, ctx: &egui::Context) {
            let mut next_state = None;
            if let Ok(guard) = self.bg_state.lock() {
                match &*guard {
                    BackgroundEvent::Extracting(_)
                    | BackgroundEvent::Benchmarking
                    | BackgroundEvent::Verifying => {
                        ctx.request_repaint_after(Duration::from_millis(30));
                    }
                    BackgroundEvent::ExtractDone(res) => {
                        self.cancel_token = None;
                        match res {
                            Ok(summary) => {
                                self.status_banner = Some((
                                    format!(
                                        "Extraction complete: {} files verified ({:.1} MB/s, peak footprint: {})",
                                        summary.extracted_files,
                                        summary.throughput_mb_per_sec,
                                        format_bytes(summary.peak_disk_footprint_bytes)
                                    ),
                                    ACCENT_EMERALD,
                                ));
                                self.last_summary = Some(summary.clone());
                            }
                            Err(err) => {
                                if err.to_lowercase().contains("cancelled") {
                                    self.status_banner = Some((
                                        "Extraction cancelled by user. State is saved for atomic resume.".to_string(),
                                        ACCENT_AMBER,
                                    ));
                                } else {
                                    self.status_banner =
                                        Some((format!("Extraction error: {}", err), ACCENT_ROSE));
                                }
                            }
                        }
                        next_state = Some(BackgroundEvent::Idle);
                    }
                    BackgroundEvent::BenchmarkDone(res) => {
                        match res {
                            Ok(report) => {
                                self.bench_history.push(report.clone());
                                self.status_banner = Some((
                                    format!(
                                        "Benchmark complete: saved {:.1}% peak disk space ({})",
                                        report.peak_space_saved_percent,
                                        format_bytes(report.peak_space_saved_bytes)
                                    ),
                                    ACCENT_EMERALD,
                                ));
                                self.last_bench_report = Some(report.clone());
                            }
                            Err(err) => {
                                self.status_banner =
                                    Some((format!("Benchmark failed: {}", err), ACCENT_ROSE));
                            }
                        }
                        next_state = Some(BackgroundEvent::Idle);
                    }
                    BackgroundEvent::VerifyDone { job_id, failures } => {
                        if failures.is_empty() {
                            self.status_banner = Some((
                                format!(
                                    "Integrity verification PASSED for job '{}': 100% CRC-32 match.",
                                    job_id
                                ),
                                ACCENT_EMERALD,
                            ));
                        } else {
                            self.status_banner = Some((
                                format!(
                                    "Integrity verification FAILED for '{}': {} corrupted/missing entries.",
                                    job_id,
                                    failures.len()
                                ),
                                ACCENT_ROSE,
                            ));
                        }
                        self.last_verification = Some((job_id.clone(), failures.clone()));
                        next_state = Some(BackgroundEvent::Idle);
                    }
                    BackgroundEvent::Idle => {}
                }
            }

            if let Some(new_state) = next_state {
                if let Ok(mut guard) = self.bg_state.lock() {
                    *guard = new_state;
                }
                self.refresh_jobs();
            }
        }
    }

    impl eframe::App for UnpackrApp {
        fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            self.poll_background_events(ctx);

            // Handle drag-and-drop archive files anywhere on the window
            let dropped_files = ctx.input(|i| i.raw.dropped_files.clone());
            if let Some(first) = dropped_files.first() {
                if let Some(path) = &first.path {
                    self.active_tab = ActiveTab::Extract;
                    self.load_archive(path.clone());
                }
            }
            let is_hovering_files = ctx.input(|i| !i.raw.hovered_files.is_empty());

            // Global Keyboard Shortcut: Ctrl+Enter (or Cmd+Enter) to Extract on Extract Tab
            if self.active_tab == ActiveTab::Extract && !self.is_busy() {
                let enter_pressed = ctx.input(|i| i.key_pressed(egui::Key::Enter));
                let modifier_down = ctx.input(|i| i.modifiers.command || i.modifiers.ctrl);
                if enter_pressed && modifier_down {
                    self.start_extraction();
                }
            }

            let palette = self.theme();

            // Top Minimalist Brand Header
            egui::TopBottomPanel::top("header_bar")
                .frame(
                    Frame::none()
                        .fill(palette.card_bg)
                        .inner_margin(Margin::symmetric(20.0, 10.0))
                        .stroke(Stroke::new(1.0_f32, palette.border_subtle)),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        // Brand mark
                        ui.label(
                            RichText::new("🗜 UNPACKR")
                                .font(FontId::new(16.5, FontFamily::Proportional))
                                .strong()
                                .color(palette.text_primary),
                        );
                        ui.add_space(SPACE_XS);
                        badge_pill(
                            ui,
                            concat!("v", env!("CARGO_PKG_VERSION")),
                            palette.card_elevated,
                            palette.text_muted,
                        );
                        ui.add_space(SPACE_SM);
                        ui.label(
                            RichText::new("Low-Disk-Space Archive Engine")
                                .size(FONT_LABEL)
                                .color(palette.text_dim),
                        );

                        // Right-aligned live status indicator and theme toggle
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let theme_label = if self.is_dark_mode { "☀ Light" } else { "🌙 Dark" };
                            let theme_btn = egui::Button::new(
                                RichText::new(theme_label).size(FONT_LABEL).strong().color(palette.text_primary),
                            )
                            .fill(palette.card_elevated)
                            .stroke(Stroke::new(1.0_f32, palette.border_subtle))
                            .rounding(Rounding::same(5.0));

                            if ui.add(theme_btn).on_hover_text("Toggle Theme (Dark / Light)").clicked() {
                                self.is_dark_mode = !self.is_dark_mode;
                                configure_minimalist_theme(ctx, self.is_dark_mode);
                            }
                            ui.add_space(SPACE_SM);

                            if self.is_busy() {
                                badge_pill(
                                    ui,
                                    "● WORKING",
                                    ACCENT_INDIGO.gamma_multiply(0.25),
                                    ACCENT_INDIGO,
                                );
                            } else {
                                badge_pill(
                                    ui,
                                    "● READY",
                                    ACCENT_EMERALD.gamma_multiply(0.18),
                                    ACCENT_EMERALD,
                                );
                            }
                        });
                    });
                });

            // Dedicated Modern Navigation Tab Bar
            egui::TopBottomPanel::top("nav_tab_bar")
                .frame(
                    Frame::none()
                        .fill(palette.card_bg)
                        .inner_margin(Margin::symmetric(20.0, 7.0))
                        .stroke(Stroke::new(1.0_f32, palette.border_subtle)),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        nav_tab_button(
                            ui,
                            &palette,
                            "📦",
                            "Extract & Inspect",
                            self.active_tab == ActiveTab::Extract,
                            || {
                                self.active_tab = ActiveTab::Extract;
                            },
                        );
                        ui.add_space(SPACE_XS);
                        nav_tab_button(
                            ui,
                            &palette,
                            "🗂",
                            &format!("Jobs & Recovery ({})", self.jobs.len()),
                            self.active_tab == ActiveTab::Jobs,
                            || {
                                self.refresh_jobs();
                                self.active_tab = ActiveTab::Jobs;
                            },
                        );
                        ui.add_space(SPACE_XS);
                        nav_tab_button(
                            ui,
                            &palette,
                            "📊",
                            "Storage Benchmark",
                            self.active_tab == ActiveTab::Benchmark,
                            || {
                                self.active_tab = ActiveTab::Benchmark;
                            },
                        );
                    });
                });

            // Persistent Bottom Status Footer
            egui::TopBottomPanel::bottom("bottom_status_bar")
                .frame(
                    Frame::none()
                        .fill(palette.card_bg)
                        .inner_margin(Margin::symmetric(20.0, 7.0))
                        .stroke(Stroke::new(1.0_f32, palette.border_subtle)),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        let (dot_color, status_text) = if self.is_busy() {
                            (ACCENT_INDIGO, "Operation in progress...")
                        } else if let Some((msg, col)) = &self.status_banner {
                            (*col, msg.as_str())
                        } else {
                            (ACCENT_EMERALD, "Ready for archive operations")
                        };

                        ui.label(RichText::new("●").color(dot_color).size(12.0));
                        ui.label(
                            RichText::new(status_text)
                                .size(FONT_LABEL)
                                .color(palette.text_muted),
                        );

                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                RichText::new(concat!(
                                    "Ctrl+Enter: Extract  •  Drop ZIP anywhere  •  v",
                                    env!("CARGO_PKG_VERSION")
                                ))
                                .size(FONT_LABEL)
                                .color(palette.text_dim),
                            );
                        });
                    });
                });

            // Main Content Canvas
            egui::CentralPanel::default()
                .frame(
                    Frame::none()
                        .fill(palette.bg_dark)
                        .inner_margin(Margin::symmetric(24.0, 16.0)),
                )
                .show(ctx, |ui| {
                    // Status notification banner if present
                    let mut dismiss_banner = false;
                    if let Some((msg, color)) = &self.status_banner {
                        Frame::none()
                            .fill(color.gamma_multiply(0.12))
                            .stroke(Stroke::new(1.0_f32, color.gamma_multiply(0.45)))
                            .rounding(Rounding::same(6.0))
                            .inner_margin(Margin::symmetric(14.0, 10.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(msg).color(*color).strong());
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if ui
                                            .small_button(RichText::new("✕").color(TEXT_MUTED))
                                            .clicked()
                                        {
                                            dismiss_banner = true;
                                        }
                                    });
                                });
                            });
                        ui.add_space(SPACE_MD);
                    }
                    if dismiss_banner {
                        self.status_banner = None;
                    }

                    // Active Progress Card if extracting
                    self.render_live_progress_card(ui);

                    // Render active tab content
                    ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| match self.active_tab {
                            ActiveTab::Extract => self.render_extract_tab(ui),
                            ActiveTab::Jobs => self.render_jobs_tab(ui),
                            ActiveTab::Benchmark => self.render_benchmark_tab(ui),
                        });
                });

            // Drag-and-drop Visual Overlay
            if is_hovering_files {
                let screen_rect = ctx.screen_rect();
                let painter = ctx.layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("drag_overlay_layer"),
                ));
                painter.rect_filled(
                    screen_rect,
                    Rounding::ZERO,
                    Color32::from_black_alpha(195),
                );
                let border_rect = screen_rect.shrink(32.0);
                painter.rect_stroke(
                    border_rect,
                    Rounding::same(12.0),
                    Stroke::new(2.5_f32, ACCENT_INDIGO),
                );
                painter.text(
                    screen_rect.center(),
                    Align2::CENTER_CENTER,
                    "📥  Drop ZIP Archive to Inspect & Extract",
                    FontId::new(22.0, FontFamily::Proportional),
                    Color32::WHITE,
                );
            }
        }
    }

    impl UnpackrApp {
        fn render_live_progress_card(&self, ui: &mut egui::Ui) {
            let maybe_progress = if let Ok(guard) = self.bg_state.lock() {
                match &*guard {
                    BackgroundEvent::Extracting(prog) => Some(prog.clone()),
                    _ => None,
                }
            } else {
                None
            };

            if let Some(opt_prog) = maybe_progress {
                self.card_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new("Extracting Archive...")
                                .size(FONT_TITLE)
                                .strong()
                                .color(self.theme().text_primary),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if let Some(token) = &self.cancel_token {
                                let is_cancelling = token.load(Ordering::Relaxed);
                                let cancel_btn = egui::Button::new(
                                    RichText::new(if is_cancelling { "⏳ Cancelling..." } else { "⏹ Cancel" })
                                        .size(FONT_LABEL)
                                        .color(ACCENT_ROSE)
                                        .strong(),
                                )
                                .fill(ACCENT_ROSE.gamma_multiply(0.12))
                                .stroke(Stroke::new(1.0_f32, ACCENT_ROSE.gamma_multiply(0.5)));
                                if ui.add_enabled(!is_cancelling, cancel_btn).clicked() {
                                    token.store(true, Ordering::Relaxed);
                                }
                                ui.add_space(SPACE_SM);
                            }
                            if let Some(prog) = &opt_prog {
                                ui.label(
                                    RichText::new(format!(
                                        "{:.1} MB/s  •  Peak: {}",
                                        prog.throughput_mb_per_sec,
                                        format_bytes(prog.peak_disk_footprint_bytes)
                                    ))
                                    .size(FONT_BODY)
                                    .color(ACCENT_CYAN)
                                    .strong(),
                                );
                            }
                        });
                    });

                    if let Some(prog) = opt_prog {
                        ui.add_space(SPACE_SM);

                        // Current active entry name row
                        let display_name = if prog.current_entry_name.len() > 64 {
                            format!("…{}", &prog.current_entry_name[prog.current_entry_name.len() - 60..])
                        } else {
                            prog.current_entry_name.clone()
                        };
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("📄 Current Entry:").size(FONT_LABEL).color(self.theme().text_muted));
                            ui.label(RichText::new(display_name).size(FONT_LABEL).color(self.theme().text_primary).monospace());
                        });

                        ui.add_space(SPACE_XS);
                        let fraction = if prog.total_uncompressed_bytes > 0 {
                            (prog.bytes_extracted as f32 / prog.total_uncompressed_bytes as f32)
                                .clamp(0.0, 1.0)
                        } else if prog.total_entries > 0 {
                            (prog.current_entry_index as f32 / prog.total_entries as f32)
                                .clamp(0.0, 1.0)
                        } else {
                            0.0
                        };

                        ui.add(
                            egui::ProgressBar::new(fraction)
                                .text(format!(
                                    "[{}/{}] {:.1}%",
                                    prog.current_entry_index,
                                    prog.total_entries,
                                    fraction * 100.0,
                                ))
                                .fill(ACCENT_INDIGO),
                        );

                        ui.add_space(SPACE_SM);
                        ui.columns(4, |cols| {
                            self.metric_tile(
                                &mut cols[0],
                                "Extracted Data",
                                &format!("{}/{}", format_bytes(prog.bytes_extracted), format_bytes(prog.total_uncompressed_bytes)),
                                self.theme().text_primary,
                            );
                            self.metric_tile(
                                &mut cols[1],
                                "Archive Reclaimed",
                                &format_bytes(prog.reclaimed_archive_bytes),
                                if prog.reclaimed_archive_bytes > 0 { ACCENT_EMERALD } else { self.theme().text_muted },
                            );
                            self.metric_tile(
                                &mut cols[2],
                                "Sparse Zero-Blocks",
                                &format_bytes(prog.sparse_bytes_saved),
                                if prog.sparse_bytes_saved > 0 { ACCENT_CYAN } else { self.theme().text_muted },
                            );
                            self.metric_tile(
                                &mut cols[3],
                                "Entries Remaining",
                                &format!("{}", prog.total_entries.saturating_sub(prog.current_entry_index)),
                                self.theme().text_muted,
                            );
                        });
                    }
                });
                ui.add_space(SPACE_MD);
            }
        }

        fn render_extract_tab(&mut self, ui: &mut egui::Ui) {
            // 1. Archive & Destination Selection Card
            self.card_frame().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("ARCHIVE & DESTINATION")
                            .size(FONT_LABEL)
                            .strong()
                            .color(self.theme().text_muted),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new("Tip: Drag & drop any .zip file onto this window")
                                .size(FONT_LABEL)
                                .color(self.theme().text_dim),
                        );
                    });
                });
                ui.add_space(SPACE_SM);

                let archive_trimmed = self.archive_path_input.trim().to_string();
                let archive_exists = !archive_trimmed.is_empty() && Path::new(&archive_trimmed).is_file();

                // Source Archive Row
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [100.0, 28.0],
                        egui::Label::new(RichText::new("📦 Source ZIP").size(FONT_BODY).color(self.theme().text_muted)),
                    );
                    let resp = ui.add_sized(
                        [ui.available_width() - 260.0, 28.0],
                        TextEdit::singleline(&mut self.archive_path_input)
                            .hint_text("/path/to/archive.zip"),
                    );
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        let p = PathBuf::from(self.archive_path_input.trim());
                        if !p.as_os_str().is_empty() {
                            self.load_archive(p);
                        }
                    }

                    if !archive_trimmed.is_empty() {
                        if archive_exists {
                            badge_pill(ui, "✓ Ready", ACCENT_EMERALD.gamma_multiply(0.2), ACCENT_EMERALD);
                        } else {
                            badge_pill(ui, "✗ Not Found", ACCENT_ROSE.gamma_multiply(0.2), ACCENT_ROSE);
                        }
                    }

                    if ui.button("📂 Browse...").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("ZIP Archives", &["zip", "ZIP"])
                            .pick_file()
                        {
                            self.load_archive(path);
                        }
                    }
                    if ui.button("🔍 Inspect").clicked() {
                        let p = PathBuf::from(self.archive_path_input.trim());
                        if !p.as_os_str().is_empty() {
                            self.load_archive(p);
                        }
                    }
                });

                if !self.recent_archives.is_empty() {
                    ui.add_space(SPACE_XS);
                    let mut archive_to_load = None;
                    let mut clear_recents = false;
                    let palette = self.theme();
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Recent:").size(FONT_LABEL).color(palette.text_dim));
                        for p in &self.recent_archives {
                            let fname = p.file_name().and_then(|n| n.to_str()).unwrap_or("archive.zip");
                            let btn = egui::Button::new(
                                RichText::new(format!("📦 {}", fname)).size(FONT_LABEL).color(palette.text_primary),
                            )
                            .fill(palette.card_elevated)
                            .stroke(Stroke::new(1.0_f32, palette.border_subtle))
                            .rounding(Rounding::same(4.0));
                            if ui.add(btn).on_hover_text(format!("Click to inspect:\n{}", p.display())).clicked() {
                                archive_to_load = Some(p.clone());
                            }
                        }
                        if ui.small_button(RichText::new("✕ Clear").size(10.0).color(palette.text_dim)).on_hover_text("Clear recent archive history").clicked() {
                            clear_recents = true;
                        }
                    });
                    if clear_recents {
                        self.recent_archives.clear();
                        save_recent_archives(&[]);
                    }
                    if let Some(p) = archive_to_load {
                        self.load_archive(p);
                    }
                }

                ui.add_space(SPACE_XS);

                let dest_trimmed = self.dest_path_input.trim().to_string();
                let dest_exists = !dest_trimmed.is_empty() && Path::new(&dest_trimmed).is_dir();

                // Destination Row
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [100.0, 28.0],
                        egui::Label::new(RichText::new("📁 Destination").size(FONT_BODY).color(TEXT_MUTED)),
                    );
                    let resp = ui.add_sized(
                        [ui.available_width() - 200.0, 28.0],
                        TextEdit::singleline(&mut self.dest_path_input)
                            .hint_text("/path/to/output_folder"),
                    );
                    let _ = resp;

                    if !dest_trimmed.is_empty() {
                        if dest_exists {
                            badge_pill(ui, "✓ Existing", ACCENT_CYAN.gamma_multiply(0.2), ACCENT_CYAN);
                        } else {
                            badge_pill(ui, "✦ New Folder", CARD_ELEVATED, TEXT_MUTED);
                        }
                    }

                    if ui.button("📂 Folder...").clicked() {
                        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                            self.dest_path_input = folder.display().to_string();
                        }
                    }

                    if dest_exists && ui.button("↗ Open").on_hover_text("Open folder in OS file manager").clicked() {
                        open_in_file_manager(Path::new(&dest_trimmed));
                    }
                });

                ui.add_space(SPACE_MD);
                ui.separator();
                ui.add_space(SPACE_SM);

                // Extraction Mode & Controls Row
                ui.horizontal_wrapped(|ui| {
                    let reclaim_active = self.reclaim_archive;
                    ui.checkbox(
                        &mut self.reclaim_archive,
                        RichText::new("In-Place Archive Reclamation (--reclaim-archive)")
                            .color(if reclaim_active {
                                ACCENT_EMERALD
                            } else {
                                TEXT_PRIMARY
                            })
                            .strong(),
                    )
                    .on_hover_text(
                        "Punches 4KB-aligned holes (FALLOC_FL_PUNCH_HOLE) in the source ZIP as each entry is verified, freeing disk space immediately.",
                    );

                    ui.add_space(SPACE_MD);
                    ui.checkbox(
                        &mut self.enable_sparse,
                        RichText::new("Sparse Zero-Block Optimization").color(TEXT_PRIMARY),
                    )
                    .on_hover_text("Skips allocating physical disk blocks for 4KB runs of zeroes.");

                    ui.add_space(SPACE_MD);
                    ui.label(RichText::new("On Collision:").color(TEXT_MUTED));
                    egui::ComboBox::from_id_source("collision_combo")
                        .selected_text(match self.collision_policy {
                            CollisionPolicy::Fail => "Fail",
                            CollisionPolicy::Skip => "Skip",
                            CollisionPolicy::Overwrite => "Overwrite",
                            CollisionPolicy::Rename => "Rename",
                        })
                        .width(100.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.collision_policy,
                                CollisionPolicy::Fail,
                                "Fail",
                            )
                            .on_hover_text("Fail: Abort extraction immediately if a file already exists at the destination.");
                            ui.selectable_value(
                                &mut self.collision_policy,
                                CollisionPolicy::Skip,
                                "Skip",
                            )
                            .on_hover_text("Skip: Preserve existing files and skip writing duplicate entries.");
                            ui.selectable_value(
                                &mut self.collision_policy,
                                CollisionPolicy::Overwrite,
                                "Overwrite",
                            )
                            .on_hover_text("Overwrite: Atomically replace existing destination files.");
                            ui.selectable_value(
                                &mut self.collision_policy,
                                CollisionPolicy::Rename,
                                "Rename",
                            )
                            .on_hover_text("Rename: Save duplicates with numbered suffixes (e.g. file_1.ext).");
                        });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let can_extract = !self.is_busy()
                            && !self.archive_path_input.trim().is_empty()
                            && !self.dest_path_input.trim().is_empty();
                        let extract_btn = egui::Button::new(
                            RichText::new("  Extract Archive [Ctrl+Enter]  ")
                                .size(FONT_BODY)
                                .strong()
                                .color(Color32::WHITE),
                        )
                        .fill(if can_extract {
                            ACCENT_INDIGO
                        } else {
                            CARD_ELEVATED
                        })
                        .rounding(Rounding::same(6.0));

                        if ui.add_enabled(can_extract, extract_btn).clicked() {
                            self.start_extraction();
                        }
                    });
                });

                // Collapsible Security & Resource Guardrails
                ui.add_space(SPACE_SM);
                ui.collapsing(
                    RichText::new("🔒 Security & Resource Guardrails").size(FONT_LABEL).color(TEXT_MUTED),
                    |ui| {
                        Frame::none()
                            .fill(BG_DARK)
                            .rounding(Rounding::same(6.0))
                            .inner_margin(Margin::symmetric(14.0, 10.0))
                            .stroke(Stroke::new(1.0_f32, BORDER_SUBTLE))
                            .show(ui, |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(RichText::new("Max Ratio:").size(FONT_LABEL).color(TEXT_MUTED));
                                    ui.add(
                                        egui::DragValue::new(&mut self.max_compression_ratio)
                                            .speed(5.0)
                                            .clamp_range(1.0..=10000.0)
                                            .suffix(":1"),
                                    ).on_hover_text("Safety threshold to abort extraction on zip bombs.");

                                    ui.add_space(SPACE_MD);
                                    ui.label(RichText::new("Max Entries:").size(FONT_LABEL).color(TEXT_MUTED));
                                    ui.add(
                                        egui::DragValue::new(&mut self.max_entries_limit)
                                            .speed(100.0)
                                            .clamp_range(1..=5_000_000),
                                    ).on_hover_text("Maximum total entries permitted.");

                                    ui.add_space(SPACE_MD);
                                    ui.label(
                                        RichText::new("Max Total Size (0=∞):")
                                            .size(FONT_LABEL)
                                            .color(TEXT_MUTED),
                                    );
                                    ui.add(
                                        egui::DragValue::new(&mut self.max_total_size_mb)
                                            .speed(10.0)
                                            .suffix(" MB"),
                                    ).on_hover_text("Maximum total uncompressed bytes.");

                                    ui.add_space(SPACE_MD);
                                    ui.label(
                                        RichText::new("Max Single File (0=∞):")
                                            .size(FONT_LABEL)
                                            .color(TEXT_MUTED),
                                    );
                                    ui.add(
                                        egui::DragValue::new(&mut self.max_file_size_mb)
                                            .speed(10.0)
                                            .suffix(" MB"),
                                    ).on_hover_text("Maximum size for any single uncompressed entry.");
                                });
                            });
                    },
                );
            });

            if let Some(err) = &self.inspection_error {
                ui.add_space(SPACE_MD);
                Frame::none()
                    .fill(ACCENT_ROSE.gamma_multiply(0.12))
                    .stroke(Stroke::new(1.0_f32, ACCENT_ROSE))
                    .rounding(Rounding::same(6.0))
                    .inner_margin(Margin::symmetric(14.0, 10.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("⚠ {}", err)).color(ACCENT_ROSE).strong());
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.button("📋 Copy Error").clicked() {
                                    ui.output_mut(|o| o.copied_text = err.clone());
                                }
                            });
                        });
                    });
            }

            // 2. Last Extraction Summary Card (if just completed)
            if let Some(summary) = &self.last_summary {
                ui.add_space(SPACE_MD);
                self.card_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("LAST EXTRACTION SUMMARY")
                                .size(FONT_LABEL)
                                .strong()
                                .color(ACCENT_EMERALD),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.small_button("📋 Copy ID").clicked() {
                                ui.output_mut(|o| o.copied_text = summary.job_id.clone());
                            }
                            ui.add_space(SPACE_XS);
                            badge_pill(
                                ui,
                                &format!("Job: {}", summary.job_id),
                                CARD_ELEVATED,
                                TEXT_MUTED,
                            );
                        });
                    });
                    ui.add_space(SPACE_SM);
                    ui.columns(5, |cols| {
                        let verified_detail = if summary.skipped_files > 0 {
                            format!("{} ({} dirs, {} skipped)", summary.extracted_files, summary.created_directories, summary.skipped_files)
                        } else {
                            format!("{} ({} dirs)", summary.extracted_files, summary.created_directories)
                        };
                        self.metric_tile(
                            &mut cols[0],
                            "Files Verified",
                            &verified_detail,
                            self.theme().text_primary,
                        );
                        self.metric_tile(
                            &mut cols[1],
                            "Data Written",
                            &format_bytes(summary.total_uncompressed_bytes),
                            self.theme().text_primary,
                        );
                        self.metric_tile(
                            &mut cols[2],
                            "Archive Reclaimed",
                            &format_bytes(summary.reclaimed_archive_bytes),
                            if summary.reclaimed_archive_bytes > 0 { ACCENT_EMERALD } else { self.theme().text_muted },
                        );
                        self.metric_tile(
                            &mut cols[3],
                            "Peak Disk Footprint",
                            &format_bytes(summary.peak_disk_footprint_bytes),
                            ACCENT_CYAN,
                        );
                        self.metric_tile(
                            &mut cols[4],
                            "Throughput",
                            &format!(
                                "{:.1} MB/s ({:.2?})",
                                summary.throughput_mb_per_sec, summary.duration
                            ),
                            self.theme().text_primary,
                        );
                    });
                });
            }

            // 3. Archive Inspection & Peak Footprint Simulator
            if let Some(insp) = &self.inspection {
                ui.add_space(14.0);
                self.card_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("ARCHIVE TELEMETRY & FOOTPRINT PROJECTION")
                                .size(11.5)
                                .strong()
                                .color(TEXT_MUTED),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if insp.has_overlapping_entries {
                                badge_pill(
                                    ui,
                                    "⚠ Overlapping Streams (Reclaim Blocked)",
                                    ACCENT_ROSE.gamma_multiply(0.2),
                                    ACCENT_ROSE,
                                );
                            } else {
                                badge_pill(
                                    ui,
                                    "✓ Safe for In-Place Reclamation",
                                    ACCENT_EMERALD.gamma_multiply(0.18),
                                    ACCENT_EMERALD,
                                );
                            }
                            if insp.is_zip64 {
                                ui.add_space(6.0);
                                badge_pill(ui, "ZIP64", ACCENT_INDIGO.gamma_multiply(0.25), ACCENT_INDIGO);
                            }
                        });
                    });

                    ui.add_space(10.0);

                    // 4 Key Metric Tiles
                    let max_entry_uncompressed = insp
                        .entries
                        .iter()
                        .map(|e| e.uncompressed_size)
                        .max()
                        .unwrap_or(0);
                    let classic_peak = insp.file_size.saturating_add(insp.total_uncompressed_size);
                    // With progressive reclamation, peak is roughly initial archive + largest single entry (plus non-reclaimable sub-4KB overhead)
                    let estimated_reclaim_peak = insp
                        .file_size
                        .max(insp.total_uncompressed_size)
                        .saturating_add(max_entry_uncompressed);
                    let estimated_reclaim_peak = estimated_reclaim_peak.min(classic_peak);

                    ui.columns(4, |cols| {
                        self.metric_tile(
                            &mut cols[0],
                            "Archive File Size",
                            &format_bytes(insp.file_size),
                            self.theme().text_primary,
                        );
                        self.metric_tile(
                            &mut cols[1],
                            "Uncompressed Total",
                            &format_bytes(insp.total_uncompressed_size),
                            self.theme().text_primary,
                        );
                        self.metric_tile(
                            &mut cols[2],
                            "Standard Peak Needed",
                            &format_bytes(classic_peak),
                            ACCENT_AMBER,
                        );
                        self.metric_tile(
                            &mut cols[3],
                            "Reclaim Peak Needed",
                            &format_bytes(estimated_reclaim_peak),
                            ACCENT_EMERALD,
                        );
                    });

                    ui.add_space(10.0);

                    // Visual Footprint Comparison Bar
                    if classic_peak > 0 {
                        let reclaim_ratio =
                            (estimated_reclaim_peak as f32 / classic_peak as f32).clamp(0.05, 1.0);
                        let saved_bytes = classic_peak.saturating_sub(estimated_reclaim_peak);
                        let saved_pct = (1.0 - reclaim_ratio) * 100.0;

                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!(
                                    "Projected Peak Disk Savings with Reclamation: {:.1}% ({} less disk space required)",
                                    saved_pct,
                                    format_bytes(saved_bytes)
                                ))
                                .size(FONT_LABEL)
                                .color(ACCENT_EMERALD),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let short_hash = format!("Identity: {}…", &insp.identity[..insp.identity.len().min(14)]);
                                if ui
                                    .button(
                                        RichText::new(short_hash)
                                            .size(FONT_LABEL)
                                            .monospace()
                                            .color(ACCENT_CYAN),
                                    )
                                    .on_hover_text(format!(
                                        "BLAKE3 Identity: {}\nClick to copy full hash to clipboard",
                                        insp.identity
                                    ))
                                    .clicked()
                                {
                                    ui.output_mut(|o| o.copied_text = insp.identity.clone());
                                }
                            });
                        });
                    }
                });

                // 4. Entry Explorer Table
                ui.add_space(SPACE_MD);
                self.card_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("ARCHIVE ENTRIES ({})", insp.entries.len()))
                                .size(FONT_LABEL)
                                .strong()
                                .color(TEXT_MUTED),
                        );

                        ui.add_space(SPACE_MD);
                        ui.add_sized(
                            [200.0, 24.0],
                            TextEdit::singleline(&mut self.entry_search)
                                .hint_text("Filter entries by name..."),
                        );

                        ui.add_space(SPACE_SM);
                        ui.selectable_value(&mut self.entry_filter, EntryFilter::All, "All");
                        ui.selectable_value(
                            &mut self.entry_filter,
                            EntryFilter::FilesOnly,
                            "Files",
                        );
                        ui.selectable_value(&mut self.entry_filter, EntryFilter::DirsOnly, "Dirs");
                        ui.selectable_value(
                            &mut self.entry_filter,
                            EntryFilter::SymlinksOnly,
                            "Symlinks",
                        );

                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .button("📄 Export CSV")
                                .on_hover_text("Export all archive entries to a CSV spreadsheet")
                                .clicked()
                            {
                                if let Some(path) = rfd::FileDialog::new()
                                    .add_filter("CSV Spreadsheet", &["csv"])
                                    .set_file_name("entries.csv")
                                    .save_file()
                                {
                                    let mut csv = String::from("index,name,is_dir,is_symlink,method,compressed_bytes,uncompressed_bytes,ratio,crc32,offset\n");
                                    for e in &insp.entries {
                                        csv.push_str(&format!(
                                            "{},\"{}\",{},{},{},{},{},{:.1},0x{:08X},0x{:08X}\n",
                                            e.index,
                                            e.name.replace('"', "\"\""),
                                            e.is_dir,
                                            e.is_symlink,
                                            e.compression_method.as_str(),
                                            e.compressed_size,
                                            e.uncompressed_size,
                                            e.savings_ratio(),
                                            e.crc32,
                                            e.data_offset,
                                        ));
                                    }
                                    if let Err(err) = fs::write(&path, csv) {
                                        self.status_banner = Some((format!("Failed to write CSV: {}", err), ACCENT_ROSE));
                                    } else {
                                        self.status_banner = Some((format!("Exported {} entries to {:?}", insp.entries.len(), path), ACCENT_EMERALD));
                                    }
                                }
                            }
                        });
                    });

                    ui.add_space(SPACE_SM);
                    ui.separator();
                    ui.add_space(SPACE_XS);

                    let avail_width = ui.available_width();
                    let path_col_width = (avail_width - 530.0).max(200.0);

                    // Table Header with Interactive Sort Buttons
                    ui.horizontal(|ui| {
                        sort_header_btn(ui, 45.0, "#", EntrySortKey::Index, &mut self.sort_key, &mut self.sort_ascending);
                        sort_header_btn(ui, path_col_width, "PATH", EntrySortKey::Path, &mut self.sort_key, &mut self.sort_ascending);
                        sort_header_btn(ui, 75.0, "METHOD", EntrySortKey::Method, &mut self.sort_key, &mut self.sort_ascending);
                        sort_header_btn(ui, 100.0, "COMPRESSED", EntrySortKey::Compressed, &mut self.sort_key, &mut self.sort_ascending);
                        sort_header_btn(ui, 105.0, "UNCOMPRESSED", EntrySortKey::Uncompressed, &mut self.sort_key, &mut self.sort_ascending);
                        sort_header_btn(ui, 65.0, "RATIO", EntrySortKey::Ratio, &mut self.sort_key, &mut self.sort_ascending);
                        sort_header_btn(ui, 80.0, "CRC-32", EntrySortKey::Crc32, &mut self.sort_key, &mut self.sort_ascending);
                        sort_header_btn(ui, 60.0, "OFFSET", EntrySortKey::Offset, &mut self.sort_key, &mut self.sort_ascending);
                    });
                    ui.separator();

                    let search_lower = self.entry_search.trim().to_lowercase();
                    let mut filtered: Vec<_> = insp
                        .entries
                        .iter()
                        .filter(|e| {
                            if !search_lower.is_empty()
                                && !e.name.to_lowercase().contains(&search_lower)
                            {
                                return false;
                            }
                            match self.entry_filter {
                                EntryFilter::All => true,
                                EntryFilter::FilesOnly => !e.is_dir && !e.is_symlink,
                                EntryFilter::DirsOnly => e.is_dir,
                                EntryFilter::SymlinksOnly => e.is_symlink,
                            }
                        })
                        .collect();

                    match self.sort_key {
                        EntrySortKey::Index => filtered.sort_by_key(|e| e.index),
                        EntrySortKey::Path => filtered.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                        EntrySortKey::Method => filtered.sort_by_key(|e| e.compression_method.as_str()),
                        EntrySortKey::Compressed => filtered.sort_by_key(|e| e.compressed_size),
                        EntrySortKey::Uncompressed => filtered.sort_by_key(|e| e.uncompressed_size),
                        EntrySortKey::Ratio => filtered.sort_by(|a, b| a.savings_ratio().partial_cmp(&b.savings_ratio()).unwrap_or(std::cmp::Ordering::Equal)),
                        EntrySortKey::Crc32 => filtered.sort_by_key(|e| e.crc32),
                        EntrySortKey::Offset => filtered.sort_by_key(|e| e.data_offset),
                    }
                    if !self.sort_ascending {
                        filtered.reverse();
                    }

                    let row_height = 22.0;
                    ScrollArea::vertical()
                        .id_source("entries_scroll")
                        .max_height(320.0)
                        .show_rows(ui, row_height, filtered.len(), |ui, row_range| {
                            for idx in row_range {
                                let entry = filtered[idx];
                                let icon = get_entry_icon(&entry.name, entry.is_dir, entry.is_symlink);
                                ui.horizontal(|ui| {
                                    let row_rect = egui::Rect::from_min_size(
                                        ui.cursor().min,
                                        Vec2::new(ui.available_width(), row_height),
                                    );
                                    if ui.rect_contains_pointer(row_rect) {
                                        ui.painter().rect_filled(row_rect, Rounding::same(3.0), CARD_ELEVATED);
                                    }

                                    ui.add_sized(
                                        [45.0, row_height],
                                        egui::Label::new(
                                            RichText::new(format!("{}", entry.index))
                                                .size(FONT_LABEL)
                                                .monospace()
                                                .color(TEXT_DIM),
                                        ),
                                    );
                                    let max_chars = ((path_col_width / 7.2) as usize).max(18);
                                    let display_name = if entry.name.len() > max_chars {
                                        format!("{} …{}", icon, &entry.name[entry.name.len().saturating_sub(max_chars - 3)..])
                                    } else {
                                        format!("{} {}", icon, entry.name)
                                    };
                                    ui.add_sized(
                                        [path_col_width, row_height],
                                        egui::Label::new(
                                            RichText::new(display_name).size(FONT_LABEL).color(
                                                if entry.is_dir {
                                                    ACCENT_CYAN
                                                } else {
                                                    TEXT_PRIMARY
                                                },
                                            ),
                                        ),
                                    )
                                    .on_hover_text(&entry.name);

                                    let method_color = match entry.compression_method {
                                        CompressionMethod::Stored => ACCENT_EMERALD,
                                        CompressionMethod::Deflated => ACCENT_INDIGO,
                                        CompressionMethod::Unsupported(_) => ACCENT_ROSE,
                                    };
                                    ui.add_sized(
                                        [75.0, row_height],
                                        egui::Label::new(
                                            RichText::new(entry.compression_method.as_str())
                                                .size(FONT_LABEL)
                                                .color(method_color),
                                        ),
                                    );
                                    ui.add_sized(
                                        [100.0, row_height],
                                        egui::Label::new(
                                            RichText::new(short_bytes(entry.compressed_size))
                                                .size(FONT_LABEL)
                                                .monospace()
                                                .color(TEXT_MUTED),
                                        ),
                                    );
                                    ui.add_sized(
                                        [105.0, row_height],
                                        egui::Label::new(
                                            RichText::new(short_bytes(entry.uncompressed_size))
                                                .size(FONT_LABEL)
                                                .monospace()
                                                .color(TEXT_PRIMARY),
                                        ),
                                    );
                                    ui.add_sized(
                                        [65.0, row_height],
                                        egui::Label::new(
                                            RichText::new(format!("{:.0}%", entry.savings_ratio()))
                                                .size(FONT_LABEL)
                                                .monospace()
                                                .color(TEXT_MUTED),
                                        ),
                                    );
                                    ui.add_sized(
                                        [80.0, row_height],
                                        egui::Label::new(
                                            RichText::new(format!("0x{:08X}", entry.crc32))
                                                .size(FONT_LABEL)
                                                .monospace()
                                                .color(TEXT_DIM),
                                        ),
                                    );
                                    ui.add_sized(
                                        [60.0, row_height],
                                        egui::Label::new(
                                            RichText::new(format!("0x{:08X}", entry.data_offset))
                                                .size(FONT_LABEL)
                                                .monospace()
                                                .color(TEXT_DIM),
                                        ),
                                    );
                                });
                            }
                        });
                });
            } else if self.inspection_error.is_none() {
                // Drop-Zone Hero when no archive is loaded yet
                ui.add_space(SPACE_LG);
                let hero_card = Frame::none()
                    .fill(CARD_BG)
                    .stroke(Stroke::new(1.5_f32, ACCENT_INDIGO.gamma_multiply(0.45)))
                    .rounding(Rounding::same(12.0))
                    .inner_margin(Margin::symmetric(36.0, 44.0))
                    .show(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.label(RichText::new("🗜").size(48.0));
                            ui.add_space(SPACE_SM);
                            ui.label(
                                RichText::new("Drop a ZIP Archive Here")
                                    .size(19.0)
                                    .strong()
                                    .color(TEXT_PRIMARY),
                            );
                            ui.add_space(SPACE_XS);
                            ui.label(
                                RichText::new(
                                    "or click anywhere in this card to select an archive from your computer",
                                )
                                .size(FONT_BODY)
                                .color(TEXT_MUTED),
                            );
                            ui.add_space(SPACE_MD);

                            ui.horizontal_wrapped(|ui| {
                                ui.centered_and_justified(|ui| {
                                    ui.horizontal(|ui| {
                                        badge_pill(
                                            ui,
                                            "⚡ In-Place Block Reclamation",
                                            CARD_ELEVATED,
                                            ACCENT_EMERALD,
                                        );
                                        ui.add_space(SPACE_XS);
                                        badge_pill(
                                            ui,
                                            "🧩 Sparse Zero-Blocks",
                                            CARD_ELEVATED,
                                            ACCENT_CYAN,
                                        );
                                        ui.add_space(SPACE_XS);
                                        badge_pill(
                                            ui,
                                            "🛡 Crash Resilient",
                                            CARD_ELEVATED,
                                            ACCENT_AMBER,
                                        );
                                        ui.add_space(SPACE_XS);
                                        badge_pill(
                                            ui,
                                            "📦 ZIP64 Supported",
                                            CARD_ELEVATED,
                                            ACCENT_INDIGO,
                                        );
                                    });
                                });
                            });

                            ui.add_space(SPACE_LG);
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("  📂 Select Archive (.zip)  ")
                                            .size(FONT_BODY)
                                            .strong()
                                            .color(Color32::WHITE),
                                    )
                                    .fill(ACCENT_INDIGO)
                                    .rounding(Rounding::same(6.0)),
                                )
                                .clicked()
                            {
                                if let Some(path) = rfd::FileDialog::new()
                                    .add_filter("ZIP Archives", &["zip", "ZIP"])
                                    .pick_file()
                                {
                                    self.load_archive(path);
                                }
                            }
                        });
                    });

                if hero_card.response.interact(egui::Sense::click()).clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("ZIP Archives", &["zip", "ZIP"])
                        .pick_file()
                    {
                        self.load_archive(path);
                    }
                }
            }
        }

        fn render_jobs_tab(&mut self, ui: &mut egui::Ui) {
            self.card_frame().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("CRASH RECOVERY & STATEFUL JOBS")
                            .size(11.5)
                            .strong()
                            .color(self.theme().text_muted),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Refresh Jobs").clicked() {
                            self.refresh_jobs();
                        }
                        if ui.button("Open Destination Folder...").clicked() {
                            if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                                if let Some(manifest_path) =
                                    find_manifest_file(&folder.display().to_string())
                                {
                                    if let Ok(manifest) = ExtractionManifest::load(&manifest_path) {
                                        self.jobs.insert(
                                            0,
                                            JobEntryItem {
                                                manifest_path,
                                                manifest,
                                            },
                                        );
                                    }
                                } else {
                                    self.status_banner = Some((
                                        format!("No .unpackr/manifest.json found in {:?}", folder),
                                        ACCENT_AMBER,
                                    ));
                                }
                            }
                        }
                    });
                });

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.resume_retry_failed, "Retry Failed Entries");
                    ui.add_space(12.0);
                    ui.checkbox(
                        &mut self.resume_verify_existing,
                        "Re-verify Existing Files Before Resume",
                    );
                    ui.add_space(12.0);
                    ui.checkbox(
                        &mut self.resume_reclaim,
                        RichText::new("Reclaim Archive on Resume").color(ACCENT_EMERALD),
                    );
                });
            });

            ui.add_space(14.0);

            if self.jobs.is_empty() {
                self.card_frame().show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(SPACE_LG);
                        ui.label(RichText::new("🗂").size(40.0));
                        ui.add_space(SPACE_XS);
                        ui.label(
                            RichText::new("No active or past extraction jobs found")
                                .size(FONT_TITLE)
                                .strong()
                                .color(self.theme().text_primary),
                        );
                        ui.add_space(SPACE_XS);
                        ui.label(
                            RichText::new(
                                "Jobs are automatically journaled to ~/.unpackr/jobs/ and <destination>/.unpackr/manifest.json for atomic crash recovery.",
                            )
                            .size(FONT_BODY)
                            .color(self.theme().text_muted),
                        );
                        ui.add_space(SPACE_LG);
                    });
                });
                return;
            }

            // Render list of jobs
            let jobs_snapshot = self.jobs.clone();
            for item in jobs_snapshot {
                let m = &item.manifest;
                let total = m.entries.len();
                let verified = m.verified_count();
                let reclaimed = m
                    .entries
                    .values()
                    .filter(|r| matches!(r.state, EntryState::Reclaimed))
                    .count();
                let failed = m.failed_count();
                let pending = m.pending_count();
                let pct = m.progress_percent() as f32 / 100.0;
                let archive_title = m.archive.path.file_name().and_then(|n| n.to_str()).unwrap_or(&m.job_id);

                self.card_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("📦 {}", archive_title))
                                .size(FONT_TITLE)
                                .strong()
                                .color(TEXT_PRIMARY),
                        );
                        ui.add_space(SPACE_XS);
                        let short_id = if m.job_id.len() > 8 { &m.job_id[..8] } else { &m.job_id };
                        if ui
                            .small_button(RichText::new(format!("#{}", short_id)).monospace().color(TEXT_MUTED))
                            .on_hover_text(format!("Job ID: {}\nClick to copy full ID", m.job_id))
                            .clicked()
                        {
                            ui.output_mut(|o| o.copied_text = m.job_id.clone());
                        }

                        ui.add_space(SPACE_XS);
                        badge_pill(
                            ui,
                            &humanize_time_ago(m.updated_at_unix),
                            CARD_ELEVATED,
                            TEXT_DIM,
                        );

                        ui.add_space(SPACE_XS);
                        if failed > 0 {
                            badge_pill(
                                ui,
                                &format!("FAILED ({})", failed),
                                ACCENT_ROSE.gamma_multiply(0.2),
                                ACCENT_ROSE,
                            );
                        } else if pending > 0 {
                            badge_pill(
                                ui,
                                &format!("INTERRUPTED ({} pending)", pending),
                                ACCENT_AMBER.gamma_multiply(0.2),
                                ACCENT_AMBER,
                            );
                        } else {
                            badge_pill(
                                ui,
                                "VERIFIED COMPLETE",
                                ACCENT_EMERALD.gamma_multiply(0.2),
                                ACCENT_EMERALD,
                            );
                        }

                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let busy = self.is_busy();
                            let is_complete = pending == 0 && failed == 0;

                            if is_complete {
                                if ui
                                    .add_enabled(
                                        !busy,
                                        egui::Button::new(
                                            RichText::new("Dismiss Record").color(TEXT_MUTED),
                                        ),
                                    )
                                    .on_hover_text(
                                        "Removes the job tracking manifest (.unpackr) while keeping all extracted files intact.",
                                    )
                                    .clicked()
                                {
                                    if let Some(parent) = item.manifest_path.parent() {
                                        let _ = fs::remove_dir_all(parent);
                                    }
                                    if let Some(global_dir) = global_jobs_dir() {
                                        let _ = fs::remove_dir_all(global_dir.join(&m.job_id));
                                    }
                                    self.status_banner = Some((
                                        format!(
                                            "Cleared job record '{}' (extracted files kept intact)",
                                            m.job_id
                                        ),
                                        ACCENT_EMERALD,
                                    ));
                                    self.refresh_jobs();
                                }
                            } else if ui
                                .add_enabled(
                                    !busy,
                                    egui::Button::new(
                                        RichText::new("Cancel & Clean").color(ACCENT_ROSE),
                                    ),
                                )
                                .on_hover_text(
                                    "Cancels the incomplete job and deletes partial extracted files.",
                                )
                                .clicked()
                            {
                                let _ = crate::cli::cancel::run_cancel(
                                    &item.manifest_path.display().to_string(),
                                    true,
                                    true,
                                );
                                self.status_banner = Some((
                                    format!("Cancelled and cleaned incomplete job '{}'", m.job_id),
                                    ACCENT_AMBER,
                                ));
                                self.refresh_jobs();
                            }

                            if ui
                                .add_enabled(!busy, egui::Button::new("Verify CRC-32"))
                                .clicked()
                            {
                                self.start_verify(item.manifest_path.clone(), m.job_id.clone());
                            }

                            if !is_complete
                                && ui
                                    .add_enabled(
                                        !busy,
                                        egui::Button::new(
                                            RichText::new("Resume Job")
                                                .strong()
                                                .color(Color32::WHITE),
                                        )
                                        .fill(ACCENT_INDIGO),
                                    )
                                    .clicked()
                            {
                                self.start_resume(item.manifest_path.display().to_string());
                            }
                        });
                    });

                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!(
                            "Archive: {}   →   Destination: {}",
                            m.archive.path.display(),
                            m.destination.display()
                        ))
                        .size(12.0)
                        .color(TEXT_MUTED),
                    );

                    ui.add_space(6.0);
                    ui.add(
                        egui::ProgressBar::new(pct)
                            .text(format!(
                                "{}/{} entries verified ({:.1}%)  •  {} reclaimed  •  {} / {}",
                                verified,
                                total,
                                pct * 100.0,
                                reclaimed,
                                format_bytes(m.verified_uncompressed_bytes()),
                                format_bytes(m.total_uncompressed_bytes())
                            ))
                            .fill(if failed > 0 {
                                ACCENT_ROSE
                            } else {
                                ACCENT_EMERALD
                            }),
                    );
                });
                ui.add_space(10.0);
            }

            // Show detailed verification report if one was run
            if let Some((job_id, failures)) = &self.last_verification {
                if !failures.is_empty() {
                    ui.add_space(6.0);
                    self.card_frame().show(ui, |ui| {
                        ui.label(
                            RichText::new(format!(
                                "VERIFICATION FAILURES FOR '{}' ({})",
                                job_id,
                                failures.len()
                            ))
                            .size(12.0)
                            .strong()
                            .color(ACCENT_ROSE),
                        );
                        ui.add_space(6.0);
                        for f in failures {
                            ui.label(
                                RichText::new(format!("• {} — {}", f.entry_name, f.reason))
                                    .size(12.0)
                                    .color(self.theme().text_primary),
                            );
                        }
                    });
                }
            }
        }

        fn render_benchmark_tab(&mut self, ui: &mut egui::Ui) {
            self.card_frame().show(ui, |ui| {
                ui.label(
                    RichText::new("PEAK DISK FOOTPRINT BENCHMARK")
                        .size(11.5)
                        .strong()
                        .color(self.theme().text_muted),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Compare standard read-only extraction against Unpackr's progressive in-place hole punching (FALLOC_FL_PUNCH_HOLE).",
                    )
                    .size(12.5)
                    .color(TEXT_MUTED),
                );
                ui.add_space(12.0);

                ui.horizontal(|ui| {
                    ui.label(RichText::new("Synthetic Entries:").color(TEXT_PRIMARY));
                    ui.add(egui::Slider::new(&mut self.bench_entries, 2..=25));
                    ui.add_space(16.0);
                    ui.label(RichText::new("Entry Size (MB):").color(TEXT_PRIMARY));
                    ui.add(egui::Slider::new(&mut self.bench_size_mb, 1..=10).suffix(" MB"));

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let busy = self.is_busy();
                        let btn = egui::Button::new(
                            RichText::new(if busy {
                                "  Running Benchmark...  "
                            } else {
                                "  Run Benchmark  "
                            })
                            .strong()
                            .color(Color32::WHITE),
                        )
                        .fill(if busy { CARD_ELEVATED } else { ACCENT_INDIGO })
                        .rounding(Rounding::same(6.0));

                        if ui.add_enabled(!busy, btn).clicked() {
                            self.start_benchmark();
                        }
                    });
                });

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Or Custom Archive (optional):")
                            .size(12.0)
                            .color(TEXT_MUTED),
                    );
                    ui.add_sized(
                        [ui.available_width() - 100.0, 26.0],
                        TextEdit::singleline(&mut self.bench_custom_archive)
                            .hint_text("Leave empty to generate synthetic stored workload"),
                    );
                    if ui.button("Browse...").clicked() {
                        if let Some(p) = rfd::FileDialog::new()
                            .add_filter("ZIP Archives", &["zip", "ZIP"])
                            .pick_file()
                        {
                            self.bench_custom_archive = p.display().to_string();
                        }
                    }
                });
            });

            if let Some(report) = &self.last_bench_report {
                ui.add_space(14.0);
                self.card_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(&report.workload)
                                .size(14.0)
                                .strong()
                                .color(self.theme().text_primary),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if report.integrity_verified {
                                badge_pill(
                                    ui,
                                    "✓ 100% Byte-for-Byte Verified",
                                    ACCENT_EMERALD.gamma_multiply(0.2),
                                    ACCENT_EMERALD,
                                );
                            } else {
                                badge_pill(
                                    ui,
                                    "✕ Integrity Mismatch",
                                    ACCENT_ROSE.gamma_multiply(0.2),
                                    ACCENT_ROSE,
                                );
                            }
                        });
                    });

                    ui.add_space(14.0);

                    ui.columns(4, |cols| {
                        self.metric_tile(
                            &mut cols[0],
                            "Standard Peak Footprint",
                            &format_bytes(report.standard_peak_bytes),
                            ACCENT_AMBER,
                        );
                        self.metric_tile(
                            &mut cols[1],
                            "Reclaim Peak Footprint",
                            &format_bytes(report.reclaim_peak_bytes),
                            ACCENT_EMERALD,
                        );
                        self.metric_tile(
                            &mut cols[2],
                            "Peak Disk Space Saved",
                            &format!(
                                "-{:.1}% ({})",
                                report.peak_space_saved_percent,
                                format_bytes(report.peak_space_saved_bytes)
                            ),
                            ACCENT_CYAN,
                        );
                        self.metric_tile(
                            &mut cols[3],
                            "Archive Bytes Reclaimed",
                            &format_bytes(report.archive_bytes_reclaimed),
                            ACCENT_EMERALD,
                        );
                    });

                    ui.add_space(SPACE_MD);
                    ui.separator();
                    ui.add_space(SPACE_SM);

                    // High-impact savings highlight card
                    Frame::none()
                        .fill(ACCENT_EMERALD.gamma_multiply(0.12))
                        .stroke(Stroke::new(1.0_f32, ACCENT_EMERALD.gamma_multiply(0.45)))
                        .rounding(Rounding::same(8.0))
                        .inner_margin(Margin::symmetric(16.0, 10.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("⚡ DISK FOOTPRINT REDUCTION:").size(FONT_BODY).strong().color(ACCENT_EMERALD));
                                ui.label(
                                    RichText::new(format!(
                                        "-{:.1}% Peak Disk Storage Needed  (Saved {})",
                                        report.peak_space_saved_percent,
                                        format_bytes(report.peak_space_saved_bytes)
                                    ))
                                    .size(FONT_TITLE)
                                    .strong()
                                    .color(self.theme().text_primary),
                                );
                            });
                        });

                    ui.add_space(SPACE_MD);

                    // Visual Bar Comparison
                    let max_bytes = report
                        .standard_peak_bytes
                        .max(report.reclaim_peak_bytes)
                        .max(1);
                    let std_frac =
                        (report.standard_peak_bytes as f32 / max_bytes as f32).clamp(0.0, 1.0);
                    let rec_frac =
                        (report.reclaim_peak_bytes as f32 / max_bytes as f32).clamp(0.0, 1.0);

                    ui.label(
                        RichText::new(format!(
                            "Standard Mode  —  {} peak  ({:.1} MB/s, {:.2}s)",
                            format_bytes(report.standard_peak_bytes),
                            report.standard_throughput_mb_s,
                            report.standard_duration_secs
                        ))
                        .size(FONT_BODY)
                        .color(self.theme().text_muted),
                    );
                    ui.add(
                        egui::ProgressBar::new(std_frac)
                            .text(format_bytes(report.standard_peak_bytes))
                            .fill(ACCENT_AMBER.gamma_multiply(0.75)),
                    );

                    ui.add_space(SPACE_SM);

                    ui.label(
                        RichText::new(format!(
                            "Unpackr Reclaim Mode  —  {} peak  ({:.1} MB/s, {:.2}s)",
                            format_bytes(report.reclaim_peak_bytes),
                            report.reclaim_throughput_mb_s,
                            report.reclaim_duration_secs
                        ))
                        .size(FONT_BODY)
                        .color(ACCENT_EMERALD)
                        .strong(),
                    );
                    ui.add(
                        egui::ProgressBar::new(rec_frac)
                            .text(format_bytes(report.reclaim_peak_bytes))
                            .fill(ACCENT_EMERALD),
                    );
                });
            }

            if !self.bench_history.is_empty() {
                ui.add_space(SPACE_MD);
                self.card_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("BENCHMARK RUN HISTORY")
                                .size(FONT_LABEL)
                                .strong()
                                .color(self.theme().text_muted),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button("Clear History").clicked() {
                                self.bench_history.clear();
                            }
                            ui.label(
                                RichText::new(format!("{} runs recorded", self.bench_history.len()))
                                    .size(FONT_LABEL)
                                    .color(self.theme().text_dim),
                            );
                        });
                    });

                    ui.add_space(SPACE_SM);
                    ui.separator();
                    ui.add_space(SPACE_XS);

                    // Table headers
                    ui.horizontal(|ui| {
                        ui.add_sized([40.0, 18.0], egui::Label::new(RichText::new("RUN").size(FONT_LABEL).strong().color(self.theme().text_dim)));
                        ui.add_sized([65.0, 18.0], egui::Label::new(RichText::new("ENTRIES").size(FONT_LABEL).strong().color(self.theme().text_dim)));
                        ui.add_sized([80.0, 18.0], egui::Label::new(RichText::new("TOTAL SIZE").size(FONT_LABEL).strong().color(self.theme().text_dim)));
                        ui.add_sized([110.0, 18.0], egui::Label::new(RichText::new("STD PEAK").size(FONT_LABEL).strong().color(self.theme().text_dim)));
                        ui.add_sized([110.0, 18.0], egui::Label::new(RichText::new("RECLAIM PEAK").size(FONT_LABEL).strong().color(self.theme().text_dim)));
                        ui.add_sized([100.0, 18.0], egui::Label::new(RichText::new("SPACE SAVED").size(FONT_LABEL).strong().color(self.theme().text_dim)));
                        ui.add_sized([110.0, 18.0], egui::Label::new(RichText::new("RECLAIM SPEED").size(FONT_LABEL).strong().color(self.theme().text_dim)));
                    });
                    ui.separator();

                    for (idx, h) in self.bench_history.iter().enumerate().rev() {
                        ui.horizontal(|ui| {
                            ui.add_sized([40.0, 20.0], egui::Label::new(RichText::new(format!("#{}", idx + 1)).size(FONT_LABEL).monospace().color(self.theme().text_muted)));
                            ui.add_sized([65.0, 20.0], egui::Label::new(RichText::new(format!("{}", h.total_entries)).size(FONT_LABEL).color(self.theme().text_primary)));
                            ui.add_sized([80.0, 20.0], egui::Label::new(RichText::new(format_bytes(h.uncompressed_bytes)).size(FONT_LABEL).monospace().color(self.theme().text_muted)));
                            ui.add_sized([110.0, 20.0], egui::Label::new(RichText::new(format_bytes(h.standard_peak_bytes)).size(FONT_LABEL).monospace().color(ACCENT_AMBER)));
                            ui.add_sized([110.0, 20.0], egui::Label::new(RichText::new(format_bytes(h.reclaim_peak_bytes)).size(FONT_LABEL).monospace().color(ACCENT_EMERALD)));
                            ui.add_sized([100.0, 20.0], egui::Label::new(RichText::new(format!("-{:.1}%", h.peak_space_saved_percent)).size(FONT_LABEL).strong().color(ACCENT_CYAN)));
                            ui.add_sized([110.0, 20.0], egui::Label::new(RichText::new(format!("{:.1} MB/s", h.reclaim_throughput_mb_s)).size(FONT_LABEL).color(self.theme().text_muted)));
                        });
                    }
                });
            }
        }
    }

    fn configure_minimalist_theme(ctx: &egui::Context, is_dark: bool) {
        let palette = if is_dark {
            ThemePalette::dark()
        } else {
            ThemePalette::light()
        };

        let mut visuals = if is_dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        visuals.panel_fill = palette.bg_dark;
        visuals.window_fill = palette.card_bg;
        visuals.extreme_bg_color = if is_dark {
            Color32::from_rgb(10, 12, 15)
        } else {
            Color32::from_rgb(255, 255, 255)
        };
        visuals.faint_bg_color = palette.card_elevated;

        visuals.widgets.noninteractive.bg_fill = palette.card_bg;
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, palette.border_subtle);
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, palette.text_primary);
        visuals.widgets.noninteractive.rounding = Rounding::same(6.0);

        visuals.widgets.inactive.bg_fill = palette.card_elevated;
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, palette.border_subtle);
        visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, palette.text_primary);
        visuals.widgets.inactive.rounding = Rounding::same(6.0);

        visuals.widgets.hovered.bg_fill = if is_dark {
            Color32::from_rgb(38, 44, 56)
        } else {
            Color32::from_rgb(230, 235, 245)
        };
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT_INDIGO);
        visuals.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, if is_dark { Color32::WHITE } else { palette.text_primary });
        visuals.widgets.hovered.rounding = Rounding::same(6.0);

        visuals.widgets.active.bg_fill = ACCENT_INDIGO;
        visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, ACCENT_INDIGO);
        visuals.widgets.active.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
        visuals.widgets.active.rounding = Rounding::same(6.0);

        visuals.selection.bg_fill = ACCENT_INDIGO.gamma_multiply(0.35);
        visuals.selection.stroke = Stroke::new(1.0_f32, ACCENT_INDIGO);

        ctx.set_visuals(visuals);

        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = Vec2::new(8.0, 8.0);
        style.spacing.button_padding = Vec2::new(12.0, 6.0);
        ctx.set_style(style);
    }

    fn open_in_file_manager(path: &Path) {
        #[cfg(target_os = "linux")]
        {
            let _ = std::process::Command::new("xdg-open").arg(path).spawn();
        }
        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("open").arg(path).spawn();
        }
        #[cfg(target_os = "windows")]
        {
            let _ = std::process::Command::new("explorer").arg(path).spawn();
        }
    }

    fn nav_tab_button(
        ui: &mut egui::Ui,
        palette: &ThemePalette,
        icon: &str,
        label: &str,
        active: bool,
        on_click: impl FnOnce(),
    ) {
        let display = format!("{}  {}", icon, label);
        let btn = egui::Button::new(
            RichText::new(display)
                .size(FONT_BODY)
                .strong()
                .color(if active {
                    Color32::WHITE
                } else {
                    palette.text_muted
                }),
        )
        .fill(if active {
            ACCENT_INDIGO
        } else {
            Color32::TRANSPARENT
        })
        .stroke(if active {
            Stroke::new(1.0_f32, ACCENT_INDIGO)
        } else {
            Stroke::new(1.0_f32, palette.border_subtle)
        })
        .rounding(Rounding::same(6.0));

        if ui.add(btn).clicked() {
            on_click();
        }
    }

    fn badge_pill(ui: &mut egui::Ui, text: &str, bg: Color32, fg: Color32) {
        Frame::none()
            .fill(bg)
            .rounding(Rounding::same(4.0))
            .inner_margin(Margin::symmetric(8.0, 3.0))
            .show(ui, |ui| {
                ui.label(RichText::new(text).size(11.0).strong().color(fg));
            });
    }

    fn humanize_time_ago(unix_timestamp: u64) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(unix_timestamp);
        let diff = now.saturating_sub(unix_timestamp);
        if diff < 60 {
            format!("{}s ago", diff.max(1))
        } else if diff < 3600 {
            format!("{}m ago", diff / 60)
        } else if diff < 86400 {
            format!("{}h ago", diff / 3600)
        } else {
            format!("{}d ago", diff / 86400)
        }
    }

    fn get_entry_icon(name: &str, is_dir: bool, is_symlink: bool) -> &'static str {
        if is_dir {
            "📁"
        } else if is_symlink {
            "🔗"
        } else {
            let lower = name.to_lowercase();
            if lower.ends_with(".png")
                || lower.ends_with(".jpg")
                || lower.ends_with(".jpeg")
                || lower.ends_with(".gif")
                || lower.ends_with(".svg")
                || lower.ends_with(".webp")
                || lower.ends_with(".ico")
                || lower.ends_with(".bmp")
            {
                "🖼"
            } else if lower.ends_with(".zip")
                || lower.ends_with(".tar")
                || lower.ends_with(".gz")
                || lower.ends_with(".xz")
                || lower.ends_with(".7z")
                || lower.ends_with(".rar")
                || lower.ends_with(".bz2")
            {
                "📦"
            } else if lower.ends_with(".exe")
                || lower.ends_with(".bin")
                || lower.ends_with(".so")
                || lower.ends_with(".dll")
                || lower.ends_with(".dylib")
                || lower.ends_with(".elf")
            {
                "⚙"
            } else {
                "📄"
            }
        }
    }

    fn sort_header_btn(
        ui: &mut egui::Ui,
        width: f32,
        title: &str,
        key: EntrySortKey,
        current_key: &mut EntrySortKey,
        ascending: &mut bool,
    ) {
        let is_current = *current_key == key;
        let arrow = if is_current {
            if *ascending { " ▲" } else { " ▼" }
        } else {
            ""
        };
        let label_text = format!("{}{}", title, arrow);
        let color = if is_current { ACCENT_CYAN } else { TEXT_DIM };
        let resp = ui.add_sized(
            [width, 20.0],
            egui::Button::new(RichText::new(label_text).size(FONT_LABEL).strong().color(color))
                .fill(Color32::TRANSPARENT)
                .stroke(Stroke::NONE),
        );
        if resp.clicked() {
            if is_current {
                *ascending = !*ascending;
            } else {
                *current_key = key;
                *ascending = true;
            }
        }
    }

    fn short_bytes(bytes: u64) -> String {
        const KB: u64 = 1024;
        const MB: u64 = KB * 1024;
        const GB: u64 = MB * 1024;
        if bytes >= GB {
            format!("{:.2} GB", bytes as f64 / GB as f64)
        } else if bytes >= MB {
            format!("{:.2} MB", bytes as f64 / MB as f64)
        } else if bytes >= KB {
            format!("{:.1} KB", bytes as f64 / KB as f64)
        } else {
            format!("{} B", bytes)
        }
    }

    /// Ensures X11 / xkbcommon uses a UTF-8 locale when the OS sets a bare locale like `en_IN`
    /// (which `/usr/share/X11/locale/locale.alias` maps to `iso8859-1/Compose`).
    fn ensure_utf8_xkb_locale() {
        for var in ["LC_ALL", "LC_CTYPE", "LANG"] {
            if let Ok(val) = std::env::var(var) {
                let lower = val.to_ascii_lowercase();
                if !val.is_empty() && !lower.contains("utf-8") && !lower.contains("utf8") {
                    let base = val.split('.').next().unwrap_or("en_US");
                    let utf8_val = if base == "C" || base == "POSIX" {
                        "C.UTF-8".to_string()
                    } else {
                        format!("{}.UTF-8", base)
                    };
                    std::env::set_var(var, utf8_val);
                }
            }
        }
    }

    pub fn launch(initial_archive: Option<PathBuf>) -> Result<()> {
        ensure_utf8_xkb_locale();

        let native_options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("Unpackr — Low-Disk-Space Archive Extraction Engine")
                .with_inner_size([1020.0, 720.0])
                .with_min_inner_size([780.0, 520.0])
                .with_drag_and_drop(true),
            ..Default::default()
        };

        eframe::run_native(
            "Unpackr",
            native_options,
            Box::new(move |cc| Box::new(UnpackrApp::new(cc, initial_archive))),
        )
        .map_err(|e| anyhow::anyhow!("Failed to launch native GUI: {}", e))
    }
}
