use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use cssimpler::app::{App, Invalidation, latest_runtime_stats};
use cssimpler::core::{Color, ElementInteractionState, ElementPath, Node, RenderNode, Style};
use cssimpler::renderer::{
    FrameInfo, RedrawSchedule, SceneProvider, ViewportSize, WindowConfig, latest_frame_timing_stats,
};
use cssimpler::style::{Stylesheet, parse_stylesheet};
use cssimpler::ui;

use crate::dialogs;
use crate::page_counter::{self, PageCounterOptions};
use crate::pdf;
use crate::registry::{self, ToolDefinition, ToolId, ToolStatus};
use crate::results::{ResultLevel, ToolResult, display_path, display_paths};
use crate::tray::{self, TrayEvent};
use crate::usb::{self, DriveInfo};

const MAX_USB_DRIVE_BUTTONS: usize = 16;
const PROFILE_ENV_VAR: &str = "PRINTLTOOLS_PROFILE";
const PROFILE_PATH_ENV_VAR: &str = "PRINTLTOOLS_PROFILE_PATH";
const WINDOW_WIDTH: usize = 520;
const WINDOW_HEIGHT: usize = 680;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Launcher,
    Settings,
    PageCounterPrompt,
    UsbDriveSelect,
    Processing,
    Result,
}

#[derive(Debug, Clone)]
struct PageCounterPrompt {
    folder: PathBuf,
    include_subfolders: bool,
}

struct PrintLTools {
    view: View,
    include_subfolders: bool,
    remember_last_folders: bool,
    open_launcher_on_tray_click: bool,
    powerpoint_slides_per_page: u32,
    page_counter_prompt: Option<PageCounterPrompt>,
    processing_title: Option<String>,
    processing_detail: Option<String>,
    usb_drives: Vec<DriveInfo>,
    last_result: Option<ToolResult>,
    background_sender: Sender<Message>,
    background_receiver: Receiver<Message>,
    tray_events: Option<Receiver<TrayEvent>>,
}

#[derive(Debug, Clone)]
enum Message {
    Tray(TrayEvent),
    OpenLauncher,
    OpenSettings,
    Minimize,
    ToolPressed(ToolId),
    FolderPicked(Option<PathBuf>),
    PdfFilesPicked(Option<Vec<PathBuf>>),
    PdfOutputPicked {
        files: Vec<PathBuf>,
        output: Option<PathBuf>,
    },
    PdfMergeFinished(ToolResult),
    UsbDrivesLoaded(Result<Vec<DriveInfo>, String>),
    UsbDriveSelected(usize),
    UsbEjectFinished(ToolResult),
    ToggleIncludeSubfolders,
    PowerPointSlidesPerPageChanged(u32),
    RunPendingPageCounter,
    PageCounterFinished(ToolResult),
    CancelPendingTool,
    ToggleRememberLastFolders,
    ToggleOpenOnTrayClick,
    DismissResult,
}

#[derive(Debug, Clone)]
enum UiCommand {
    OpenLauncher,
    OpenSettings,
    Minimize,
    ToolPressed(ToolId),
    UsbDriveSelected(usize),
    ToggleIncludeSubfolders,
    PowerPointSlidesPerPageChanged(u32),
    RunPendingPageCounter,
    CancelPendingTool,
    ToggleRememberLastFolders,
    ToggleOpenOnTrayClick,
    DismissResult,
}

static UI_COMMANDS: OnceLock<Mutex<Vec<UiCommand>>> = OnceLock::new();
static FULL_REPAINT_REQUEST: AtomicU8 = AtomicU8::new(0);

pub(crate) fn request_full_window_repaint() {
    FULL_REPAINT_REQUEST.store(1, Ordering::Release);
}

pub fn run() -> cssimpler::renderer::Result<()> {
    let app = PollingSceneProvider {
        inner: App::new(PrintLTools::new(), stylesheet(), update, view),
        profiler: FrameProfiler::from_env(),
        window_positioned: false,
        full_repaint_frames: 0,
    };

    cssimpler::renderer::run_with_scene_provider(
        WindowConfig {
            clear_color: Color::rgb(54, 67, 78),
            frame_time: Duration::from_millis(16),
            ..WindowConfig::new("PrintLTools", WINDOW_WIDTH, WINDOW_HEIGHT)
                .with_glass_capable(true)
                .with_decorations(false)
        },
        app,
    )
}

struct PollingSceneProvider<P> {
    inner: P,
    profiler: FrameProfiler,
    window_positioned: bool,
    full_repaint_frames: u8,
}

impl<P> SceneProvider for PollingSceneProvider<P>
where
    P: SceneProvider,
{
    fn update(&mut self, frame: FrameInfo) {
        self.full_repaint_frames = self.full_repaint_frames.saturating_sub(1);
        self.full_repaint_frames = self
            .full_repaint_frames
            .max(FULL_REPAINT_REQUEST.swap(0, Ordering::AcqRel));
        self.profiler.record_frame(frame);
        self.inner.update(frame);
    }

    fn scene(&self) -> &[RenderNode] {
        self.inner.scene()
    }

    fn capture_scene(&mut self) -> Vec<RenderNode> {
        let mut scene = self.inner.capture_scene();
        if self.full_repaint_frames > 0 {
            apply_full_repaint_marker(&mut scene);
        }
        scene
    }

    fn set_viewport(&mut self, viewport: ViewportSize) {
        self.inner.set_viewport(viewport);
        if !self.window_positioned {
            self.window_positioned = true;
            if let Err(error) = crate::window_control::position_bottom_right() {
                eprintln!("PrintLTools window positioning failed: {error}");
            }
        }
    }

    fn set_element_interaction(&mut self, interaction: ElementInteractionState) -> bool {
        self.profiler.record_interaction(&interaction);
        self.inner.set_element_interaction(interaction)
    }

    fn redraw_schedule(&self) -> RedrawSchedule {
        RedrawSchedule::EveryFrame
    }

    fn needs_redraw(&self) -> bool {
        self.inner.needs_redraw()
    }
}

fn apply_full_repaint_marker(scene: &mut [RenderNode]) {
    let Some(root) = scene.first_mut() else {
        return;
    };

    let background = root.style.background.unwrap_or(Color::rgb(224, 239, 247));
    root.style.background = Some(Color {
        b: background.b ^ 1,
        ..background
    });
}

struct FrameProfiler {
    writer: Option<BufWriter<File>>,
    started_at: Instant,
    sample_index: u64,
    current_interaction: ElementInteractionState,
    interaction_changed: bool,
}

impl FrameProfiler {
    fn from_env() -> Self {
        let writer = std::env::var_os(PROFILE_ENV_VAR).and_then(|_| {
            let path = std::env::var_os(PROFILE_PATH_ENV_VAR)
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    std::env::temp_dir()
                        .join(format!("printltools-frame-profile-{}.csv", process::id()))
                });
            let file = File::create(&path);
            match file {
                Ok(file) => {
                    eprintln!("PrintLTools frame profile: {}", path.display());
                    let mut writer = BufWriter::new(file);
                    if let Err(error) = writeln!(
                        writer,
                        "sample_index,elapsed_ms,frame_index,frame_delta_us,hovered,active,interaction_changed,runtime_view_us,runtime_render_tree_us,runtime_scene_swap_us,runtime_transition_us,runtime_structural_update_us,runtime_interaction_us,runtime_style_resolution_us,runtime_layout_sync_us,runtime_render_extraction_us,runtime_rerendered,runtime_transition_active,frame_update_us,frame_scene_prep_us,frame_paint_us,frame_present_us,frame_total_us,render_workers,dirty_regions,dirty_jobs,damage_pixels,painted_pixels,scene_passes,paint_mode,paint_reason"
                    ) {
                        eprintln!("PrintLTools frame profile header failed: {error}");
                        None
                    } else if let Err(error) = writer.flush() {
                        eprintln!("PrintLTools frame profile header flush failed: {error}");
                        None
                    } else {
                        Some(writer)
                    }
                }
                Err(error) => {
                    eprintln!(
                        "PrintLTools frame profile could not create {}: {error}",
                        path.display()
                    );
                    None
                }
            }
        });

        Self {
            writer,
            started_at: Instant::now(),
            sample_index: 0,
            current_interaction: ElementInteractionState::default(),
            interaction_changed: false,
        }
    }

    fn record_interaction(&mut self, interaction: &ElementInteractionState) {
        if self.current_interaction == *interaction {
            return;
        }

        self.current_interaction = interaction.clone();
        self.interaction_changed = true;
    }

    fn record_frame(&mut self, frame: FrameInfo) {
        let Some(writer) = self.writer.as_mut() else {
            return;
        };

        let runtime = latest_runtime_stats();
        let timing = latest_frame_timing_stats();
        let elapsed_ms = self.started_at.elapsed().as_millis();
        let interaction_changed = self.interaction_changed;
        self.interaction_changed = false;

        let result = writeln!(
            writer,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{:?},{:?}",
            self.sample_index,
            elapsed_ms,
            frame.frame_index,
            frame.delta.as_micros(),
            format_element_path(&self.current_interaction.hovered),
            format_element_path(&self.current_interaction.active),
            interaction_changed,
            runtime.view_us,
            runtime.render_tree_us,
            runtime.scene_swap_us,
            runtime.transition_us,
            runtime.structural_update_us,
            runtime.interaction_us,
            runtime.style_resolution_us,
            runtime.layout_sync_us,
            runtime.render_extraction_us,
            runtime.rerendered,
            runtime.transition_active,
            timing.update_us,
            timing.scene_prep_us,
            timing.paint_us,
            timing.present_us,
            timing.total_us,
            timing.render_workers,
            timing.dirty_regions,
            timing.dirty_jobs,
            timing.damage_pixels,
            timing.painted_pixels,
            timing.scene_passes,
            timing.paint_mode,
            timing.paint_reason
        );

        if let Err(error) = result {
            eprintln!("PrintLTools frame profile write failed: {error}");
            self.writer = None;
            return;
        }

        self.sample_index = self.sample_index.saturating_add(1);
        if self.sample_index % 60 == 0
            && let Some(writer) = self.writer.as_mut()
            && let Err(error) = writer.flush()
        {
            eprintln!("PrintLTools frame profile flush failed: {error}");
            self.writer = None;
        }
    }
}

impl Drop for FrameProfiler {
    fn drop(&mut self) {
        if let Some(writer) = self.writer.as_mut() {
            let _ = writer.flush();
        }
    }
}

fn format_element_path(path: &Option<ElementPath>) -> String {
    let Some(path) = path else {
        return String::new();
    };

    let mut value = path.root.to_string();
    for child in &path.children {
        value.push('.');
        value.push_str(&child.to_string());
    }
    value
}

impl PrintLTools {
    fn new() -> Self {
        let (background_sender, background_receiver) = mpsc::channel();
        let mut state = Self {
            view: View::Launcher,
            include_subfolders: false,
            remember_last_folders: true,
            open_launcher_on_tray_click: true,
            powerpoint_slides_per_page: 4,
            page_counter_prompt: None,
            processing_title: None,
            processing_detail: None,
            usb_drives: Vec::new(),
            last_result: None,
            background_sender,
            background_receiver,
            tray_events: None,
        };

        match tray::spawn_events() {
            Ok(receiver) => state.tray_events = Some(receiver),
            Err(error) => {
                state.last_result = Some(ToolResult::warning(
                    "Tray integration",
                    "The app started, but the Windows tray icon could not be initialized.",
                    vec![error],
                ));
                state.view = View::Result;
            }
        }

        state
    }
}

impl From<UiCommand> for Message {
    fn from(value: UiCommand) -> Self {
        match value {
            UiCommand::OpenLauncher => Self::OpenLauncher,
            UiCommand::OpenSettings => Self::OpenSettings,
            UiCommand::Minimize => Self::Minimize,
            UiCommand::ToolPressed(id) => Self::ToolPressed(id),
            UiCommand::UsbDriveSelected(index) => Self::UsbDriveSelected(index),
            UiCommand::ToggleIncludeSubfolders => Self::ToggleIncludeSubfolders,
            UiCommand::PowerPointSlidesPerPageChanged(value) => {
                Self::PowerPointSlidesPerPageChanged(value)
            }
            UiCommand::RunPendingPageCounter => Self::RunPendingPageCounter,
            UiCommand::CancelPendingTool => Self::CancelPendingTool,
            UiCommand::ToggleRememberLastFolders => Self::ToggleRememberLastFolders,
            UiCommand::ToggleOpenOnTrayClick => Self::ToggleOpenOnTrayClick,
            UiCommand::DismissResult => Self::DismissResult,
        }
    }
}

fn update(state: &mut PrintLTools, _frame: FrameInfo) -> Invalidation {
    let mut messages = Vec::new();

    if let Some(receiver) = &state.tray_events {
        while let Ok(event) = receiver.try_recv() {
            messages.push(Message::Tray(event));
        }
    }

    while let Ok(message) = state.background_receiver.try_recv() {
        messages.push(message);
    }

    messages.extend(take_ui_commands().into_iter().map(Message::from));

    let mut changed = false;
    for message in messages {
        changed |= handle_message(state, message);
    }

    if changed {
        Invalidation::Layout
    } else {
        Invalidation::Clean
    }
}

fn handle_message(state: &mut PrintLTools, message: Message) -> bool {
    match message {
        Message::Tray(event) => handle_tray_event(state, event),
        Message::OpenLauncher => {
            state.view = View::Launcher;
            true
        }
        Message::OpenSettings => {
            state.view = View::Settings;
            true
        }
        Message::Minimize => minimize_to_tray(state),
        Message::ToolPressed(id) => start_tool(state, id),
        Message::FolderPicked(folder) => handle_folder_picked(state, folder),
        Message::PdfFilesPicked(files) => handle_pdf_files_picked(state, files),
        Message::PdfOutputPicked { files, output } => {
            handle_pdf_output_picked(state, files, output)
        }
        Message::PdfMergeFinished(result) => record_result(state, result),
        Message::UsbDrivesLoaded(result) => handle_usb_drives_loaded(state, result),
        Message::UsbDriveSelected(index) => {
            let Some(drive) = state.usb_drives.get(index).cloned() else {
                return record_result(
                    state,
                    ToolResult::warning(
                        "USB safe eject",
                        "The selected drive is no longer available.",
                        Vec::new(),
                    ),
                );
            };

            handle_usb_drive_selected(state, drive)
        }
        Message::UsbEjectFinished(result) => record_result(state, result),
        Message::ToggleIncludeSubfolders => {
            state.include_subfolders = !state.include_subfolders;
            true
        }
        Message::PowerPointSlidesPerPageChanged(value) => {
            state.powerpoint_slides_per_page = value;
            true
        }
        Message::RunPendingPageCounter => run_pending_page_counter(state),
        Message::PageCounterFinished(result) => record_result(state, result),
        Message::CancelPendingTool => {
            state.page_counter_prompt = None;
            state.processing_title = None;
            state.processing_detail = None;
            state.usb_drives.clear();
            state.view = View::Launcher;
            true
        }
        Message::ToggleRememberLastFolders => {
            state.remember_last_folders = !state.remember_last_folders;
            true
        }
        Message::ToggleOpenOnTrayClick => {
            state.open_launcher_on_tray_click = !state.open_launcher_on_tray_click;
            true
        }
        Message::DismissResult => {
            state.last_result = None;
            state.view = View::Launcher;
            true
        }
    }
}

fn handle_tray_event(state: &mut PrintLTools, event: TrayEvent) -> bool {
    match event {
        TrayEvent::HideLauncher => minimize_to_tray(state),
        TrayEvent::OpenLauncher => {
            if state.open_launcher_on_tray_click {
                state.view = View::Launcher;
                true
            } else {
                false
            }
        }
        TrayEvent::OpenSettings => {
            state.view = View::Settings;
            true
        }
        TrayEvent::Error(error) => record_result(
            state,
            ToolResult::error(
                "Tray integration",
                "The app is running, but the Windows tray icon could not be initialized.",
                vec![error],
            ),
        ),
    }
}

fn minimize_to_tray(state: &mut PrintLTools) -> bool {
    if state.tray_events.is_none() {
        return record_result(
            state,
            ToolResult::warning(
                "Minimize to tray",
                "The app could not be minimized because tray integration is unavailable.",
                Vec::new(),
            ),
        );
    }

    match crate::window_control::hide() {
        Ok(()) => false,
        Err(error) => record_result(
            state,
            ToolResult::warning(
                "Minimize to tray",
                "The app could not hide its window.",
                vec![error],
            ),
        ),
    }
}

fn start_tool(state: &mut PrintLTools, id: ToolId) -> bool {
    match id {
        ToolId::FolderPageCounter => spawn_or_record(state, "printltools-dialog", |sender| {
            move || {
                let folder = dialogs::pick_folder("Select folder for page counting");
                let _ = sender.send(Message::FolderPicked(folder));
            }
        }),
        ToolId::UsbSafeEject => {
            state.processing_title = Some("Loading drives".to_string());
            state.processing_detail = Some("Scanning removable and external drives.".to_string());
            state.view = View::Processing;

            spawn_or_record(state, "printltools-worker", |sender| {
                move || {
                    let result = usb::list_drives();
                    let _ = sender.send(Message::UsbDrivesLoaded(result));
                }
            });

            true
        }
        ToolId::PdfJoiner => spawn_or_record(state, "printltools-dialog", |sender| {
            move || {
                let files = dialogs::pick_pdf_files("Select PDF files to join");
                let _ = sender.send(Message::PdfFilesPicked(files));
            }
        }),
    }
}

fn handle_folder_picked(state: &mut PrintLTools, folder: Option<PathBuf>) -> bool {
    let Some(folder) = folder else {
        return record_result(
            state,
            ToolResult::info(
                "Folder page counter",
                "Folder selection was canceled.",
                Vec::new(),
            ),
        );
    };

    state.page_counter_prompt = Some(PageCounterPrompt {
        folder,
        include_subfolders: state.include_subfolders,
    });
    state.view = View::PageCounterPrompt;
    true
}

fn handle_pdf_files_picked(state: &mut PrintLTools, files: Option<Vec<PathBuf>>) -> bool {
    let Some(files) = files else {
        return record_result(
            state,
            ToolResult::info("PDF joiner", "PDF selection was canceled.", Vec::new()),
        );
    };

    if files.is_empty() {
        return record_result(
            state,
            ToolResult::warning("PDF joiner", "No PDF files were selected.", Vec::new()),
        );
    }

    spawn_or_record(state, "printltools-dialog", |sender| {
        move || {
            let output = dialogs::save_pdf_file("Save joined PDF as", "joined.pdf");
            let _ = sender.send(Message::PdfOutputPicked { files, output });
        }
    })
}

fn handle_pdf_output_picked(
    state: &mut PrintLTools,
    files: Vec<PathBuf>,
    output: Option<PathBuf>,
) -> bool {
    let Some(output) = output else {
        return record_result(
            state,
            ToolResult::info("PDF joiner", "Output selection was canceled.", Vec::new()),
        );
    };

    state.processing_title = Some("Joining PDFs".to_string());
    state.processing_detail = Some(format!("Merging {} selected PDF files.", files.len()));
    state.view = View::Processing;

    spawn_or_record(state, "printltools-worker", |sender| {
        move || {
            let result = merge_pdfs_result(files, output);
            let _ = sender.send(Message::PdfMergeFinished(result));
        }
    });

    true
}

fn handle_usb_drives_loaded(
    state: &mut PrintLTools,
    result: Result<Vec<DriveInfo>, String>,
) -> bool {
    match result {
        Ok(drives) if drives.is_empty() => record_result(
            state,
            ToolResult::warning(
                "USB safe eject",
                "No eligible removable or external drives were found.",
                Vec::new(),
            ),
        ),
        Ok(drives) => {
            state.usb_drives = drives;
            state.processing_title = None;
            state.processing_detail = None;
            state.view = View::UsbDriveSelect;
            true
        }
        Err(error) => record_result(
            state,
            ToolResult::error(
                "USB safe eject",
                "Drive scan failed.",
                vec![format!("Error: {error}")],
            ),
        ),
    }
}

fn handle_usb_drive_selected(state: &mut PrintLTools, drive: DriveInfo) -> bool {
    state.processing_title = Some("Preparing USB eject".to_string());
    state.processing_detail = Some(format!("Closing processes and ejecting {}.", drive.letter));
    state.view = View::Processing;

    spawn_or_record(state, "printltools-worker", |sender| {
        move || {
            let result = usb_eject_result(drive);
            let _ = sender.send(Message::UsbEjectFinished(result));
        }
    });

    true
}

fn run_pending_page_counter(state: &mut PrintLTools) -> bool {
    let Some(prompt) = state.page_counter_prompt.clone() else {
        return false;
    };

    let options = PageCounterOptions {
        folder: prompt.folder,
        include_subfolders: prompt.include_subfolders,
        powerpoint_slides_per_page: state.powerpoint_slides_per_page,
    };

    state.page_counter_prompt = None;
    state.processing_title = Some("Counting pages".to_string());
    state.processing_detail =
        Some("Counting PDF and document pages in the selected folder.".to_string());
    state.view = View::Processing;

    spawn_or_record(state, "printltools-worker", |sender| {
        move || {
            let result = run_page_counter(options);
            let _ = sender.send(Message::PageCounterFinished(result));
        }
    });

    true
}

fn spawn_or_record<F, G>(state: &mut PrintLTools, name: &'static str, build: F) -> bool
where
    F: FnOnce(Sender<Message>) -> G,
    G: FnOnce() + Send + 'static,
{
    let sender = state.background_sender.clone();
    let task = build(sender);
    match thread::Builder::new().name(name.to_string()).spawn(task) {
        Ok(_) => false,
        Err(error) => record_result(
            state,
            ToolResult::error(
                "Background task",
                "The operation could not start.",
                vec![format!("Error: {error}")],
            ),
        ),
    }
}

fn merge_pdfs_result(files: Vec<PathBuf>, output: PathBuf) -> ToolResult {
    let mut details = vec![
        format!("Output: {}", display_path(&output)),
        format!("Input files: {}", files.len()),
    ];
    details.extend(display_paths(&files));

    match pdf::merge_pdfs(&files, &output) {
        Ok(summary) => ToolResult::info(
            "PDF joiner",
            "PDF files were joined successfully.",
            vec![
                format!("Output: {}", display_path(&summary.output_path)),
                format!("Input files: {}", summary.input_count),
                format!("Total pages: {}", summary.total_pages),
            ],
        ),
        Err(error) => {
            details.push(format!("Error: {error}"));
            ToolResult::error("PDF joiner", "PDF merge failed.", details)
        }
    }
}

fn usb_eject_result(drive: DriveInfo) -> ToolResult {
    match usb::safe_eject(drive) {
        Ok(summary) => {
            let mut details = vec![
                format!("Drive: {}", summary.drive.display_name()),
                format!(
                    "Restart Manager resources registered: {}",
                    summary.resources_registered
                ),
                format!(
                    "Restart Manager processes affected: {}",
                    summary.restart_manager_processes.len()
                ),
                format!(
                    "Process scan actions: {}",
                    summary.process_scan_actions.len()
                ),
                format!(
                    "Administrator rights required: {}",
                    if summary.admin_required { "yes" } else { "no" }
                ),
                format!("Eject method: {}", eject_method_label(summary.eject_method)),
            ];

            if summary.resource_scan_limited {
                details.push(
                    "Resource scan hit its limit before every file on the drive was registered."
                        .to_string(),
                );
            }

            for process in &summary.restart_manager_processes {
                details.push(format_process_action("Restart Manager", process));
            }

            for process in &summary.process_scan_actions {
                details.push(format_process_action("Process scan", process));
            }

            for note in &summary.notes {
                details.push(format!("Note: {note}"));
            }

            if summary.admin_required {
                ToolResult::warning(
                    "USB safe eject",
                    "Administrator rights are required before this drive can be safely inspected.",
                    details,
                )
            } else if summary.eject_method == usb::EjectMethod::Mountvol {
                ToolResult::warning(
                    "USB safe eject",
                    "The volume was dismounted with mountvol, but Windows Shell safe eject was not confirmed.",
                    details,
                )
            } else if summary.ejected {
                ToolResult::info(
                    "USB safe eject",
                    "Windows accepted the eject request.",
                    details,
                )
            } else {
                ToolResult::warning(
                    "USB safe eject",
                    "Processes were handled, but Windows still reports the drive as mounted.",
                    details,
                )
            }
        }
        Err(error) => ToolResult::error(
            "USB safe eject",
            "USB eject could not start.",
            vec![format!("Error: {error}")],
        ),
    }
}

fn format_process_action(source: &str, process: &usb::ProcessAction) -> String {
    let path = process.path.as_deref().unwrap_or("path unavailable");

    format!(
        "{source}: PID {} {} - {} ({})",
        process.pid, process.name, process.action, path
    )
}

fn eject_method_label(method: usb::EjectMethod) -> &'static str {
    match method {
        usb::EjectMethod::None => "none",
        usb::EjectMethod::Shell => "Windows Shell eject",
        usb::EjectMethod::Mountvol => "mountvol /p dismount",
    }
}

fn run_page_counter(options: PageCounterOptions) -> ToolResult {
    match page_counter::count_folder(&options) {
        Ok(summary) => {
            let mut details = vec![
                format!("Folder: {}", display_path(&summary.folder)),
                format!(
                    "Include subfolders: {}",
                    if summary.include_subfolders {
                        "yes"
                    } else {
                        "no"
                    }
                ),
                format!(
                    "PowerPoint slides per printed page: {}",
                    summary.powerpoint_slides_per_page
                ),
                format!("Counted files: {}", summary.counted_files),
                format!("PDF files discovered: {}", summary.pdf_files),
                format!("Document files discovered: {}", summary.document_files),
                format!(
                    "Microsoft Word files counted: {}",
                    summary.word_counted_files
                ),
                format!(
                    "LibreOffice fallback files counted: {}",
                    summary.libreoffice_fallback_files
                ),
                format!("Skipped files: {}", summary.skipped_files.len()),
                format!("Failed files: {}", summary.failed_files.len()),
            ];

            for skipped in &summary.skipped_files {
                details.push(format!(
                    "Skipped: {} ({})",
                    display_path(&skipped.path),
                    skipped.reason
                ));
            }

            for failed in &summary.failed_files {
                details.push(format!(
                    "Failed: {} ({})",
                    display_path(&failed.path),
                    failed.reason
                ));
            }

            ToolResult::info(
                "Folder page counter",
                format!("Total counted pages: {}", summary.total_pages),
                details,
            )
        }
        Err(error) => ToolResult::error(
            "Folder page counter",
            "Page counting could not start.",
            vec![error],
        ),
    }
}

fn record_result(state: &mut PrintLTools, result: ToolResult) -> bool {
    state.last_result = Some(result);
    state.processing_title = None;
    state.processing_detail = None;
    state.usb_drives.clear();
    state.view = View::Result;
    true
}

fn view(state: &PrintLTools) -> Node {
    let body = match state.view {
        View::Launcher => launcher_view(state),
        View::Settings => settings_view(state),
        View::PageCounterPrompt => page_counter_prompt_view(state),
        View::UsbDriveSelect => usb_drive_select_view(state),
        View::Processing => processing_view(state),
        View::Result => result_view(state),
    };

    ui! {
        <div id="app">
            <section class="shell">
                <header class="topbar">
                    <div class="brand">
                        <span class="brand-mark">
                            {icon_app_brand()}
                        </span>
                        <div class="brand-copy">
                            <h1>PrintLTools</h1>
                            <p>Print and file utilities</p>
                        </div>
                    </div>
                    {top_actions(state.view)}
                </header>
                <main class="content">
                    {body}
                </main>
            </section>
        </div>
    }
}

fn top_actions(current_view: View) -> Node {
    ui! {
        <div class="top-actions">
            {nav_button("Launcher", icon_launch(), command_open_launcher, current_view == View::Launcher)}
            {nav_button("Settings", icon_sliders(), command_open_settings, current_view == View::Settings)}
            {add_class(nav_button("Minimize", icon_minimize(), command_minimize, false), "minimize")}
        </div>
    }
}

fn launcher_view(state: &PrintLTools) -> Node {
    let mut tools = Node::element("div").with_class("tool-list");
    for tool in registry::all_tools() {
        tools = tools.with_child(tool_button(tool));
    }

    ui! {
        <div class="view">
            <section class="panel">
                <div class="section-heading">
                    <h2>Tools</h2>
                    <p>Choose a utility to start.</p>
                </div>
                {Node::from(tools)}
            </section>
            <section class="panel">
                <div class="section-heading">
                    <h2>Folder options</h2>
                    <p>Used by the folder page counter.</p>
                </div>
                {toggle_button(
                    "Include subfolders",
                    state.include_subfolders,
                    command_toggle_include_subfolders,
                )}
            </section>
            <section class="panel">
                <div class="section-heading">
                    <h2>Latest result</h2>
                    <p>Most recent completed operation.</p>
                </div>
                {latest_result_panel(state.last_result.as_ref())}
            </section>
        </div>
    }
}

fn settings_view(state: &PrintLTools) -> Node {
    ui! {
        <div class="view">
            <section class="panel">
                <div class="section-heading">
                    <h2>Settings</h2>
                    <p>Local application preferences.</p>
                </div>
                <div class="settings-list">
                    {toggle_button(
                        "Open launcher when restoring from tray",
                        state.open_launcher_on_tray_click,
                        command_toggle_open_on_tray_click,
                    )}
                    {toggle_button(
                        "Remember last folders",
                        state.remember_last_folders,
                        command_toggle_remember_last_folders,
                    )}
                </div>
                <p class="muted-line">Start with Windows and persisted settings are planned for Epic F.</p>
            </section>
        </div>
    }
}

fn result_view(state: &PrintLTools) -> Node {
    let Some(result) = &state.last_result else {
        return ui! {
            <div class="view">
                <section class="panel">
                    <h2>No result</h2>
                    {action_button("Back to launcher", command_dismiss_result)}
                </section>
            </div>
        };
    };

    ui! {
        <div class="view">
            <section class={result_panel_class(result.level)}>
                <p class="eyebrow">{result_level_label(result.level)}</p>
                <h2>{&result.title}</h2>
                <p class="result-summary">{&result.summary}</p>
                {result_detail_list(result)}
                <div class="button-row">
                    {action_button("Back to launcher", command_dismiss_result)}
                    {action_button("Keep result", command_open_launcher)}
                </div>
            </section>
        </div>
    }
}

fn page_counter_prompt_view(state: &PrintLTools) -> Node {
    let Some(prompt) = &state.page_counter_prompt else {
        return ui! {
            <div class="view">
                <section class="panel">
                    <h2>No folder selected</h2>
                    {action_button("Back to launcher", command_cancel_pending_tool)}
                </section>
            </div>
        };
    };

    ui! {
        <div class="view">
            <section class="panel">
                <div class="section-heading">
                    <h2>Folder page counter</h2>
                    <p>Confirm options before counting pages.</p>
                </div>
                <div class="fact-list">
                    <p>{format!("Folder: {}", display_path(&prompt.folder))}</p>
                    <p>{format!(
                        "Include subfolders: {}",
                        if prompt.include_subfolders { "yes" } else { "no" }
                    )}</p>
                </div>
                <h3>PowerPoint slides per printed page</h3>
                {slide_options(state.powerpoint_slides_per_page)}
                <div class="button-row">
                    {action_button("Count pages", command_run_pending_page_counter)}
                    {action_button("Cancel", command_cancel_pending_tool)}
                </div>
            </section>
        </div>
    }
}

fn usb_drive_select_view(state: &PrintLTools) -> Node {
    let mut drives = Node::element("div").with_class("drive-list");

    for (index, drive) in state.usb_drives.iter().enumerate() {
        let Some(handler) = drive_select_handler(index) else {
            continue;
        };

        drives = drives.with_child(drive_button(drive, handler));
    }

    if state.usb_drives.len() > MAX_USB_DRIVE_BUTTONS {
        drives = drives.with_child(ui! {
            <p class="muted-line">
                Only the first 16 detected drives are shown.
            </p>
        });
    }

    ui! {
        <div class="view">
            <section class="panel">
                <div class="section-heading">
                    <h2>USB safe eject</h2>
                    <p>Select the drive to close locking processes and request safe eject.</p>
                </div>
                {Node::from(drives)}
                <div class="button-row">
                    {action_button("Cancel", command_cancel_pending_tool)}
                </div>
            </section>
        </div>
    }
}

fn processing_view(state: &PrintLTools) -> Node {
    ui! {
        <div class="view">
            <section class="panel processing-panel">
                <p class="eyebrow">Working</p>
                <h2>{state.processing_title.as_deref().unwrap_or("Processing")}</h2>
                <p>{state.processing_detail.as_deref().unwrap_or("The selected operation is still running.")}</p>
                <p class="muted-line">You can leave this window open while the background worker finishes.</p>
            </section>
        </div>
    }
}

fn tool_button(tool: &'static ToolDefinition) -> Node {
    let status = match tool.status {
        ToolStatus::Ready => "Ready",
        ToolStatus::Planned => "Planned",
    };

    let button = match tool.status {
        ToolStatus::Ready => ui! {
            <button class="tool-button" type="button" onclick={tool_handler(tool.id)}>
                <span class="tool-accent"></span>
                <span class="tool-surface"></span>
                {tool_icon(tool.id)}
                <span class="tool-copy">
                    <span class="tool-title">{tool.name}</span>
                    <span class="tool-description">{tool.description}</span>
                    <span class="tool-meta">{format!("{} - {}", tool.short_name, status)}</span>
                </span>
                <span class="tool-chevron">
                    {icon_chevron()}
                </span>
            </button>
        },
        ToolStatus::Planned => ui! {
            <button class="tool-button" type="button">
                <span class="tool-accent"></span>
                <span class="tool-surface"></span>
                {tool_icon(tool.id)}
                <span class="tool-copy">
                    <span class="tool-title">{tool.name}</span>
                    <span class="tool-description">{tool.description}</span>
                    <span class="tool-meta">{format!("{} - {}", tool.short_name, status)}</span>
                </span>
                <span class="tool-chevron">
                    {icon_chevron()}
                </span>
            </button>
        },
    };

    let button = add_class(button, tool_button_variant_class(tool.id));
    let button = add_style(button, tool_accent_style(tool.id));
    if tool.status == ToolStatus::Planned {
        add_class(button, "disabled")
    } else {
        button
    }
}

fn latest_result_panel(result: Option<&ToolResult>) -> Node {
    let Some(result) = result else {
        return ui! {
            <div class="empty-state empty-result">
                {icon_history()}
            </div>
        };
    };

    ui! {
        <div class={result_panel_class(result.level)}>
            <p class="eyebrow">{result_level_label(result.level)}</p>
            <h3>{&result.title}</h3>
            <p>{&result.summary}</p>
            {action_button("Dismiss", command_dismiss_result)}
        </div>
    }
}

fn result_detail_list(result: &ToolResult) -> Node {
    if result.details.is_empty() {
        return ui! {
            <div class="detail-list">
                <p class="muted-line">No additional details.</p>
            </div>
        };
    }

    let mut details = Node::element("div").with_class("detail-list");
    for detail in &result.details {
        details = details.with_child(ui! {
            <p class="detail-line">{detail}</p>
        });
    }
    details.into()
}

fn slide_options(selected: u32) -> Node {
    let mut options = Node::element("div").with_class("segmented");

    for value in [1_u32, 2, 3, 4, 6, 9] {
        let label = value.to_string();
        let handler = slide_handler(value);
        let mut button = ui! {
            <button class="segment-button" type="button" onclick={handler}>
                <span class="button-text">{label}</span>
            </button>
        };

        if selected == value {
            button = add_class(button, "selected");
        }

        options = options.with_child(button);
    }

    options.into()
}

fn drive_button(drive: &DriveInfo, handler: fn()) -> Node {
    ui! {
        <button class="drive-button" type="button" onclick={handler}>
            <span class="drive-icon">
                {icon_usb()}
            </span>
            <span class="drive-copy">
                <span class="drive-title">{drive.display_name()}</span>
                <span class="drive-root">{format!("Root: {}", display_path(&drive.root))}</span>
            </span>
            <span class="tool-chevron">
                {icon_chevron()}
            </span>
        </button>
    }
}

fn nav_button(label: &'static str, icon: Node, handler: fn(), selected: bool) -> Node {
    let button = ui! {
        <button class="nav-button" type="button" onclick={handler}>
            <span class="button-icon">
                {icon}
            </span>
            <span class="button-text">{label}</span>
        </button>
    };

    if selected {
        add_class(button, "selected")
    } else {
        button
    }
}

fn action_button(label: &'static str, handler: fn()) -> Node {
    ui! {
        <button class="action-button" type="button" onclick={handler}>
            <span class="button-text">{label}</span>
        </button>
    }
}

fn toggle_button(label: &'static str, selected: bool, handler: fn()) -> Node {
    let button = ui! {
        <button class="toggle-button" type="button" onclick={handler}>
            <span class="toggle-track">
                <span class="toggle-knob"></span>
                <span class="toggle-state">{if selected { "ON" } else { "OFF" }}</span>
            </span>
            <span class="toggle-label">{label}</span>
        </button>
    };

    if selected {
        add_class(button, "selected")
    } else {
        button
    }
}

fn add_class(node: Node, class_name: &'static str) -> Node {
    match node {
        Node::Element(element) => element.with_class(class_name).into(),
        Node::Text(_) => node,
    }
}

fn add_style(node: Node, style: Style) -> Node {
    match node {
        Node::Element(element) => element.with_style(style).into(),
        Node::Text(_) => node,
    }
}

fn tool_button_variant_class(id: ToolId) -> &'static str {
    match id {
        ToolId::FolderPageCounter => "tool-folder",
        ToolId::UsbSafeEject => "tool-usb",
        ToolId::PdfJoiner => "tool-pdf",
    }
}

#[derive(Clone, Copy)]
struct AccentGradient {
    shadow: Color,
    body: Color,
    saturated: Color,
    shifted: Color,
    end: Color,
}

fn tool_accent_style(id: ToolId) -> Style {
    let gradient = generated_accent_gradient(tool_accent_base(id));
    let mut style = Style::default();
    style
        .custom_properties
        .set("--tool-accent-shadow", color_css(gradient.shadow));
    style
        .custom_properties
        .set("--tool-accent-body", color_css(gradient.body));
    style
        .custom_properties
        .set("--tool-accent-saturated", color_css(gradient.saturated));
    style
        .custom_properties
        .set("--tool-accent-shifted", color_css(gradient.shifted));
    style
        .custom_properties
        .set("--tool-accent-end", color_css(gradient.end));
    style
}

fn tool_accent_base(id: ToolId) -> Color {
    match id {
        ToolId::FolderPageCounter => Color::rgb(34, 135, 214),
        ToolId::UsbSafeEject => Color::rgb(30, 180, 132),
        ToolId::PdfJoiner => Color::rgb(117, 86, 206),
    }
}

fn generated_accent_gradient(base_color: Color) -> AccentGradient {
    let base = HsvColor::from_color(base_color);
    let shifted_hue = accent_target_hue(base.hue);

    AccentGradient {
        shadow: HsvColor {
            hue: lerp_hue(base.hue, shifted_hue, 0.04),
            saturation: base.saturation.max(0.76),
            value: 0.52,
        }
        .to_color(),
        body: base_color,
        saturated: HsvColor {
            hue: lerp_hue(base.hue, shifted_hue, 0.34),
            saturation: 1.0,
            value: 1.0,
        }
        .to_color(),
        shifted: HsvColor {
            hue: lerp_hue(base.hue, shifted_hue, 0.72),
            saturation: 0.86,
            value: 1.0,
        }
        .to_color(),
        end: HsvColor {
            hue: shifted_hue,
            saturation: 0.56,
            value: 1.0,
        }
        .to_color(),
    }
}

fn generated_svg_accent_gradient(base_color: Color) -> AccentGradient {
    let base = HsvColor::from_color(base_color);
    let highlight = HsvColor {
        hue: base.hue,
        saturation: base.saturation,
        value: (base.value + 0.06).min(0.88),
    }
    .to_color();

    AccentGradient {
        shadow: HsvColor {
            hue: base.hue,
            saturation: base.saturation,
            value: 0.52,
        }
        .to_color(),
        body: base_color,
        saturated: highlight,
        shifted: highlight,
        end: highlight,
    }
}

#[derive(Clone, Copy)]
struct HsvColor {
    hue: f32,
    saturation: f32,
    value: f32,
}

impl HsvColor {
    fn from_color(color: Color) -> Self {
        let r = color.r as f32 / 255.0;
        let g = color.g as f32 / 255.0;
        let b = color.b as f32 / 255.0;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;

        let hue = if delta <= f32::EPSILON {
            0.0
        } else if max == r {
            60.0 * ((g - b) / delta).rem_euclid(6.0)
        } else if max == g {
            60.0 * (((b - r) / delta) + 2.0)
        } else {
            60.0 * (((r - g) / delta) + 4.0)
        };

        Self {
            hue,
            saturation: if max <= f32::EPSILON {
                0.0
            } else {
                delta / max
            },
            value: max,
        }
    }

    fn to_color(self) -> Color {
        let hue = self.hue.rem_euclid(360.0) / 60.0;
        let saturation = self.saturation.clamp(0.0, 1.0);
        let value = self.value.clamp(0.0, 1.0);
        let chroma = value * saturation;
        let x = chroma * (1.0 - (hue.rem_euclid(2.0) - 1.0).abs());
        let m = value - chroma;

        let (r, g, b) = if hue < 1.0 {
            (chroma, x, 0.0)
        } else if hue < 2.0 {
            (x, chroma, 0.0)
        } else if hue < 3.0 {
            (0.0, chroma, x)
        } else if hue < 4.0 {
            (0.0, x, chroma)
        } else if hue < 5.0 {
            (x, 0.0, chroma)
        } else {
            (chroma, 0.0, x)
        };

        Color::rgb(
            color_channel(r + m),
            color_channel(g + m),
            color_channel(b + m),
        )
    }
}

fn accent_target_hue(hue: f32) -> f32 {
    if hue >= 245.0 {
        hue + 42.0
    } else if hue >= 185.0 {
        hue - 30.0
    } else {
        hue + 24.0
    }
}

fn lerp_hue(from: f32, to: f32, amount: f32) -> f32 {
    let delta = (to - from + 540.0).rem_euclid(360.0) - 180.0;
    (from + delta * amount).rem_euclid(360.0)
}

fn color_channel(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn color_css(color: Color) -> String {
    format!("rgb({}, {}, {})", color.r, color.g, color.b)
}

fn tool_icon(id: ToolId) -> Node {
    let tile = Node::element("span")
        .with_class("tool-icon")
        .with_class(match id {
            ToolId::FolderPageCounter => "folder",
            ToolId::UsbSafeEject => "usb",
            ToolId::PdfJoiner => "pdf",
        })
        .with_child(match id {
            ToolId::FolderPageCounter => icon_folder(),
            ToolId::UsbSafeEject => icon_usb(),
            ToolId::PdfJoiner => icon_pdf_document(),
        });

    tile.into()
}

fn icon_app_brand() -> Node {
    Node::element("svg")
        .with_class("svg-icon")
        .with_class("icon-app-brand")
        .with_attribute("xmlns", "http://www.w3.org/2000/svg")
        .with_attribute("viewBox", "0 0 64 64")
        .with_child(
            Node::element("path")
                .with_attribute(
                    "d",
                    "M14 4 H50 C55.52 4 60 8.48 60 14 V50 C60 55.52 55.52 60 50 60 H14 C8.48 60 4 55.52 4 50 V14 C4 8.48 8.48 4 14 4 Z",
                )
                .with_attribute("fill", "rgb(20, 106, 158)")
                .with_attribute("stroke", "none")
                .into(),
        )
        .with_child(
            Node::element("path")
                .with_attribute(
                    "d",
                    "M24 16.5 H36.5 C37.2 16.5 37.8 16.8 38.3 17.3 L46.7 25.7 C47.2 26.2 47.5 26.8 47.5 27.5 V46 C47.5 47.1 46.6 48 45.5 48 H24 C22.9 48 22 47.1 22 46 V18.5 C22 17.4 22.9 16.5 24 16.5 Z",
                )
                .with_attribute("fill", "none")
                .with_attribute("stroke", "rgb(255, 255, 255)")
                .with_attribute("stroke-width", "2.5")
                .with_attribute("stroke-linecap", "round")
                .with_attribute("stroke-linejoin", "round")
                .into(),
        )
        .with_child(
            Node::element("path")
                .with_attribute("d", "M38 18 V25 C38 26.1 38.9 27 40 27 H46")
                .with_attribute("fill", "none")
                .with_attribute("stroke", "rgb(255, 255, 255)")
                .with_attribute("stroke-width", "2.5")
                .with_attribute("stroke-linecap", "round")
                .with_attribute("stroke-linejoin", "round")
                .into(),
        )
        .with_child(
            Node::element("path")
                .with_attribute("d", "M28.5 33.5 H37.5 M28.5 39 H41.5")
                .with_attribute("fill", "none")
                .with_attribute("stroke", "rgb(255, 255, 255)")
                .with_attribute("stroke-width", "2")
                .with_attribute("stroke-linecap", "round")
                .with_attribute("stroke-linejoin", "round")
                .into(),
        )
        .into()
}

fn icon_pdf_document() -> Node {
    let gradient = generated_svg_accent_gradient(tool_accent_base(ToolId::PdfJoiner));
    let stroke_width = "1.35";

    Node::element("svg")
        .with_class("svg-icon")
        .with_class("icon-pdf-document")
        .with_attribute("xmlns", "http://www.w3.org/2000/svg")
        .with_attribute("viewBox", "0 0 24 24")
        .with_child(
            Node::element("defs")
                .with_child(
                    Node::element("linearGradient")
                        .with_id("pdf-accent-gradient")
                        .with_attribute("x1", "0")
                        .with_attribute("y1", "24")
                        .with_attribute("x2", "0")
                        .with_attribute("y2", "0")
                        .with_attribute("gradientUnits", "userSpaceOnUse")
                        .with_child(svg_gradient_stop("0%", gradient.shadow).into())
                        .with_child(svg_gradient_stop("14%", gradient.body).into())
                        .with_child(svg_gradient_stop("54%", gradient.body).into())
                        .with_child(svg_gradient_stop("82%", gradient.saturated).into())
                        .with_child(svg_gradient_stop("93%", gradient.shifted).into())
                        .with_child(svg_gradient_stop("97%", gradient.end).into())
                        .into(),
                )
                .into(),
        )
        .with_child(
            Node::element("path")
                .with_attribute(
                    "d",
                    "M6.45 12.45 V3.9 C6.45 3.22 7 2.68 7.68 2.68 H14.35 C14.72 2.68 15.08 2.83 15.34 3.09 L18.96 6.71 C19.22 6.97 19.37 7.33 19.37 7.7 V20.18 C19.37 20.86 18.82 21.4 18.14 21.4 H16.6",
                )
                .with_attribute("fill", "none")
                .with_attribute("stroke", "url(#pdf-accent-gradient)")
                .with_attribute("stroke-width", stroke_width)
                .with_attribute("stroke-linecap", "round")
                .with_attribute("stroke-linejoin", "round")
                .into(),
        )
        .with_child(
            Node::element("path")
                .with_attribute("d", "M14.35 2.85 V6.65 C14.35 7.08 14.7 7.43 15.13 7.43 H19.18")
                .with_attribute("fill", "none")
                .with_attribute("stroke", "url(#pdf-accent-gradient)")
                .with_attribute("stroke-width", stroke_width)
                .with_attribute("stroke-linecap", "round")
                .with_attribute("stroke-linejoin", "round")
                .into(),
        )
        .with_child(
            Node::element("path")
                .with_attribute(
                    "d",
                    "M2.85 13.55 H15.05 C15.5 13.55 15.86 13.91 15.86 14.36 V19.42 C15.86 19.87 15.5 20.23 15.05 20.23 H2.85 C2.4 20.23 2.04 19.87 2.04 19.42 V14.36 C2.04 13.91 2.4 13.55 2.85 13.55 Z M3.35 15.05 V18.85 H4.42 V17.55 H5.64 C6.6 17.55 7.18 17.08 7.18 16.28 C7.18 15.48 6.6 15.05 5.64 15.05 H3.35 Z M4.42 15.88 H5.5 C5.92 15.88 6.14 16.03 6.14 16.29 C6.14 16.56 5.92 16.72 5.5 16.72 H4.42 Z M7.75 15.05 V18.85 H9.28 C10.58 18.85 11.42 18.1 11.42 16.95 C11.42 15.8 10.58 15.05 9.28 15.05 H7.75 Z M8.82 15.9 H9.2 C9.9 15.9 10.34 16.3 10.34 16.95 C10.34 17.6 9.9 18 9.2 18 H8.82 Z M12 15.05 V18.85 H13.07 V17.48 H14.7 V16.62 H13.07 V15.93 H15.08 V15.05 H12 Z",
                )
                .with_attribute("fill", "url(#pdf-accent-gradient)")
                .with_attribute("stroke", "none")
                .into(),
        )
        .into()
}

const FOLDER_ICON_STROKE_WIDTH: &str = "1.15";

fn icon_folder() -> Node {
    let gradient = generated_svg_accent_gradient(tool_accent_base(ToolId::FolderPageCounter));
    let stroke_width = FOLDER_ICON_STROKE_WIDTH;

    Node::element("svg")
        .with_class("svg-icon")
        .with_class("icon-folder")
        .with_attribute("xmlns", "http://www.w3.org/2000/svg")
        .with_attribute("viewBox", "0 0 24 24")
        .with_child(
            Node::element("defs")
                .with_child(
                    Node::element("linearGradient")
                        .with_id("folder-accent-gradient")
                        .with_attribute("x1", "0")
                        .with_attribute("y1", "24")
                        .with_attribute("x2", "0")
                        .with_attribute("y2", "0")
                        .with_attribute("gradientUnits", "userSpaceOnUse")
                        .with_child(svg_gradient_stop("0%", gradient.shadow).into())
                        .with_child(svg_gradient_stop("14%", gradient.body).into())
                        .with_child(svg_gradient_stop("54%", gradient.body).into())
                        .with_child(svg_gradient_stop("82%", gradient.saturated).into())
                        .with_child(svg_gradient_stop("93%", gradient.shifted).into())
                        .with_child(svg_gradient_stop("97%", gradient.end).into())
                        .into(),
                )
                .into(),
        )
        .with_child(
            Node::element("path")
                .with_attribute(
                    "d",
                    "M20 9 V6.47214 C20 6.16165 19.92771 5.85542 19.78885 5.57771 L19 4 H14 L13 6 H3 C2.4477 6 2 6.44772 2 7 V9 V18 C2 19.1046 2.8954 20 4 20 H6",
                )
                .with_attribute("fill", "none")
                .with_attribute("stroke", "url(#folder-accent-gradient)")
                .with_attribute("stroke-width", stroke_width)
                .with_attribute("stroke-linecap", "round")
                .with_attribute("stroke-linejoin", "round")
                .into(),
        )
        .with_child(
            Node::element("path")
                .with_attribute(
                    "d",
                    "M6.7638 9 H21.69075 C22.35012 9 22.82901 9.62698 22.65551 10.2631 L20.40194 18.5262 C20.16463 19.3964 19.37431 20 18.47241 20 H4.3092 C3.6499 20 3.171 19.373 3.3445 18.7369 L5.799 9.73688 C5.9177 9.30182 6.3128 9 6.7638 9 Z",
                )
                .with_attribute("fill", "none")
                .with_attribute("stroke", "url(#folder-accent-gradient)")
                .with_attribute("stroke-width", stroke_width)
                .with_attribute("stroke-linecap", "round")
                .with_attribute("stroke-linejoin", "round")
                .into(),
        )
        .into()
}

fn svg_gradient_stop(offset: &'static str, color: Color) -> Node {
    Node::element("stop")
        .with_attribute("offset", offset)
        .with_attribute("stop-color", color_css(color))
        .into()
}

fn icon_usb() -> Node {
    let gradient = generated_svg_accent_gradient(tool_accent_base(ToolId::UsbSafeEject));

    Node::element("svg")
        .with_class("svg-icon")
        .with_class("icon-usb")
        .with_attribute("xmlns", "http://www.w3.org/2000/svg")
        .with_attribute("viewBox", "0 0 2796 2796")
        .with_child(
            Node::element("defs")
                .with_child(
                    Node::element("linearGradient")
                        .with_id("usb-accent-gradient")
                        .with_attribute("x1", "0")
                        .with_attribute("y1", "2796")
                        .with_attribute("x2", "0")
                        .with_attribute("y2", "0")
                        .with_attribute("gradientUnits", "userSpaceOnUse")
                        .with_child(svg_gradient_stop("0%", gradient.shadow).into())
                        .with_child(svg_gradient_stop("14%", gradient.body).into())
                        .with_child(svg_gradient_stop("54%", gradient.body).into())
                        .with_child(svg_gradient_stop("82%", gradient.saturated).into())
                        .with_child(svg_gradient_stop("93%", gradient.shifted).into())
                        .with_child(svg_gradient_stop("97%", gradient.end).into())
                        .into(),
                )
                .into(),
        )
        .with_child(svg_filled_gradient_path(
            "usb-accent-gradient",
            "M1520.24,1431.93 H1562.96 V1517.79 C1562.96,1542.67 1552.38,1566.52 1533.93,1583.22 L1415.64,1690.29 V1222.38 H1467.58 L1398.24,1102.28 L1328.9,1222.38 H1380.84 V1840.52 L1262.55,1733.45 C1244.1,1716.75 1233.52,1692.9 1233.52,1668.02 V1580.06 C1258.49,1572.58 1276.7,1549.45 1276.7,1522.04 C1276.7,1488.58 1249.58,1461.46 1216.12,1461.46 C1182.66,1461.46 1155.54,1488.58 1155.54,1522.04 C1155.54,1549.45 1173.75,1572.58 1198.72,1580.06 V1668.02 C1198.72,1702.71 1213.47,1735.96 1239.19,1759.24 L1380.83,1887.45 V1985.03 C1333.59,1993.27 1297.67,2034.46 1297.67,2084.06 C1297.67,2139.6 1342.69,2184.62 1398.23,2184.62 C1453.77,2184.62 1498.79,2139.6 1498.79,2084.06 C1498.79,2034.46 1462.87,1993.28 1415.63,1985.03 V1737.22 L1557.27,1609.01 C1582.99,1585.73 1597.74,1552.48 1597.74,1517.78 V1431.92 H1640.45 V1311.7 H1520.23 V1431.92 Z",
        ).into())
        .with_child(svg_filled_gradient_path(
            "usb-accent-gradient",
            "M1853.45,818.95 H1772.5 V372.55 C1772.5,347.97 1752.57,328.04 1727.99,328.04 H1067.99 C1043.41,328.04 1023.48,347.97 1023.48,372.55 V818.95 H942.53 C917.95,818.95 898.02,838.88 898.02,863.46 V2213.46 C898.02,2281.44 924.49,2345.35 972.56,2393.42 C1020.63,2441.49 1084.54,2467.96 1152.52,2467.96 H1643.43 C1711.41,2467.96 1775.32,2441.49 1823.39,2393.42 C1871.46,2345.35 1897.93,2281.44 1897.93,2213.46 V863.46 C1897.93,838.88 1878.03,818.95 1853.45,818.95 Z M1112.51,417.06 H1683.49 V818.95 H1112.51 V417.06 Z M1808.95,2213.46 C1808.95,2304.71 1734.71,2378.95 1643.46,2378.95 H1152.55 C1061.3,2378.95 987.06,2304.71 987.06,2213.46 V907.96 H1808.95 V2213.45 Z",
        ).into())
        .with_child(svg_filled_gradient_path(
            "usb-accent-gradient",
            "M1228.24,540.77 H1287.75 C1295.9,540.77 1302.5,547.37 1302.5,555.52 V615.03 C1302.5,623.18 1295.9,629.78 1287.75,629.78 H1228.24 C1220.09,629.78 1213.49,623.18 1213.49,615.03 V555.52 C1213.49,547.37 1220.09,540.77 1228.24,540.77 Z",
        ).into())
        .with_child(svg_filled_gradient_path(
            "usb-accent-gradient",
            "M1508.24,540.77 H1567.75 C1575.9,540.77 1582.5,547.37 1582.5,555.52 V615.03 C1582.5,623.18 1575.9,629.78 1567.75,629.78 H1508.24 C1500.09,629.78 1493.49,623.18 1493.49,615.03 V555.52 C1493.49,547.37 1500.09,540.77 1508.24,540.77 Z",
        ).into())
        .into()
}

fn svg_filled_gradient_path(gradient_id: &'static str, data: &'static str) -> Node {
    Node::element("path")
        .with_attribute("d", data)
        .with_attribute("fill", format!("url(#{gradient_id})"))
        .with_attribute("stroke", "none")
        .into()
}

fn icon_launch() -> Node {
    svg_icon("icon-launch", &["M5 19 L19 5", "M10 5 L19 5 L19 14"], &[])
}

fn icon_sliders() -> Node {
    svg_icon(
        "icon-sliders",
        &["M4 7 L20 7", "M4 17 L20 17"],
        &[("9", "7", "2"), ("15", "17", "2")],
    )
}

fn icon_minimize() -> Node {
    svg_icon("icon-minimize", &["M5 17 L19 17"], &[])
}

fn icon_chevron() -> Node {
    svg_icon("icon-chevron", &["M9 6 L15 12 L9 18"], &[])
}

fn icon_history() -> Node {
    svg_icon(
        "icon-history",
        &["M12 6 L12 12 L16 14", "M7 8 L4 8 L4 5"],
        &[("12", "12", "8")],
    )
}

fn svg_icon(
    class_name: &'static str,
    paths: &[&'static str],
    circles: &[(&'static str, &'static str, &'static str)],
) -> Node {
    let mut svg = Node::element("svg")
        .with_class("svg-icon")
        .with_class(class_name)
        .with_attribute("xmlns", "http://www.w3.org/2000/svg")
        .with_attribute("viewBox", "0 0 24 24");

    for path in paths {
        svg = svg.with_child(Node::element("path").with_attribute("d", *path).into());
    }

    for (cx, cy, r) in circles {
        svg = svg.with_child(
            Node::element("circle")
                .with_attribute("cx", *cx)
                .with_attribute("cy", *cy)
                .with_attribute("r", *r)
                .into(),
        );
    }

    svg.into()
}

fn result_panel_class(level: ResultLevel) -> &'static str {
    match level {
        ResultLevel::Info => "result-panel-info",
        ResultLevel::Warning => "result-panel-warning",
        ResultLevel::Error => "result-panel-error",
    }
}

fn result_level_label(level: ResultLevel) -> &'static str {
    match level {
        ResultLevel::Info => "Info",
        ResultLevel::Warning => "Warning",
        ResultLevel::Error => "Error",
    }
}

fn stylesheet() -> &'static Stylesheet {
    static STYLESHEET: OnceLock<Stylesheet> = OnceLock::new();

    STYLESHEET.get_or_init(|| {
        parse_stylesheet(include_str!("app.css")).expect("app stylesheet should stay valid")
    })
}

fn command_open_launcher() {
    enqueue(UiCommand::OpenLauncher);
}

fn command_open_settings() {
    enqueue(UiCommand::OpenSettings);
}

fn command_minimize() {
    enqueue(UiCommand::Minimize);
}

fn command_folder_page_counter() {
    enqueue(UiCommand::ToolPressed(ToolId::FolderPageCounter));
}

fn command_usb_safe_eject() {
    enqueue(UiCommand::ToolPressed(ToolId::UsbSafeEject));
}

fn command_pdf_joiner() {
    enqueue(UiCommand::ToolPressed(ToolId::PdfJoiner));
}

fn command_toggle_include_subfolders() {
    enqueue(UiCommand::ToggleIncludeSubfolders);
}

fn command_slide_1() {
    enqueue(UiCommand::PowerPointSlidesPerPageChanged(1));
}

fn command_slide_2() {
    enqueue(UiCommand::PowerPointSlidesPerPageChanged(2));
}

fn command_slide_3() {
    enqueue(UiCommand::PowerPointSlidesPerPageChanged(3));
}

fn command_slide_4() {
    enqueue(UiCommand::PowerPointSlidesPerPageChanged(4));
}

fn command_slide_6() {
    enqueue(UiCommand::PowerPointSlidesPerPageChanged(6));
}

fn command_slide_9() {
    enqueue(UiCommand::PowerPointSlidesPerPageChanged(9));
}

fn command_run_pending_page_counter() {
    enqueue(UiCommand::RunPendingPageCounter);
}

fn command_cancel_pending_tool() {
    enqueue(UiCommand::CancelPendingTool);
}

fn command_toggle_remember_last_folders() {
    enqueue(UiCommand::ToggleRememberLastFolders);
}

fn command_toggle_open_on_tray_click() {
    enqueue(UiCommand::ToggleOpenOnTrayClick);
}

fn command_dismiss_result() {
    enqueue(UiCommand::DismissResult);
}

fn command_select_drive_0() {
    enqueue(UiCommand::UsbDriveSelected(0));
}

fn command_select_drive_1() {
    enqueue(UiCommand::UsbDriveSelected(1));
}

fn command_select_drive_2() {
    enqueue(UiCommand::UsbDriveSelected(2));
}

fn command_select_drive_3() {
    enqueue(UiCommand::UsbDriveSelected(3));
}

fn command_select_drive_4() {
    enqueue(UiCommand::UsbDriveSelected(4));
}

fn command_select_drive_5() {
    enqueue(UiCommand::UsbDriveSelected(5));
}

fn command_select_drive_6() {
    enqueue(UiCommand::UsbDriveSelected(6));
}

fn command_select_drive_7() {
    enqueue(UiCommand::UsbDriveSelected(7));
}

fn command_select_drive_8() {
    enqueue(UiCommand::UsbDriveSelected(8));
}

fn command_select_drive_9() {
    enqueue(UiCommand::UsbDriveSelected(9));
}

fn command_select_drive_10() {
    enqueue(UiCommand::UsbDriveSelected(10));
}

fn command_select_drive_11() {
    enqueue(UiCommand::UsbDriveSelected(11));
}

fn command_select_drive_12() {
    enqueue(UiCommand::UsbDriveSelected(12));
}

fn command_select_drive_13() {
    enqueue(UiCommand::UsbDriveSelected(13));
}

fn command_select_drive_14() {
    enqueue(UiCommand::UsbDriveSelected(14));
}

fn command_select_drive_15() {
    enqueue(UiCommand::UsbDriveSelected(15));
}

fn tool_handler(id: ToolId) -> fn() {
    match id {
        ToolId::FolderPageCounter => command_folder_page_counter,
        ToolId::UsbSafeEject => command_usb_safe_eject,
        ToolId::PdfJoiner => command_pdf_joiner,
    }
}

fn slide_handler(value: u32) -> fn() {
    match value {
        1 => command_slide_1,
        2 => command_slide_2,
        3 => command_slide_3,
        4 => command_slide_4,
        6 => command_slide_6,
        9 => command_slide_9,
        _ => command_slide_4,
    }
}

fn drive_select_handler(index: usize) -> Option<fn()> {
    match index {
        0 => Some(command_select_drive_0),
        1 => Some(command_select_drive_1),
        2 => Some(command_select_drive_2),
        3 => Some(command_select_drive_3),
        4 => Some(command_select_drive_4),
        5 => Some(command_select_drive_5),
        6 => Some(command_select_drive_6),
        7 => Some(command_select_drive_7),
        8 => Some(command_select_drive_8),
        9 => Some(command_select_drive_9),
        10 => Some(command_select_drive_10),
        11 => Some(command_select_drive_11),
        12 => Some(command_select_drive_12),
        13 => Some(command_select_drive_13),
        14 => Some(command_select_drive_14),
        15 => Some(command_select_drive_15),
        _ => None,
    }
}

fn enqueue(command: UiCommand) {
    ui_commands()
        .lock()
        .expect("UI command queue should not be poisoned")
        .push(command);
}

fn take_ui_commands() -> Vec<UiCommand> {
    std::mem::take(
        &mut *ui_commands()
            .lock()
            .expect("UI command queue should not be poisoned"),
    )
}

fn ui_commands() -> &'static Mutex<Vec<UiCommand>> {
    UI_COMMANDS.get_or_init(|| Mutex::new(Vec::new()))
}

#[cfg(test)]
mod tests {
    use cssimpler::core::{
        BackgroundLayer, Color, Node, RenderKind, RenderNode, SvgPaintServerData,
        SvgPathPaintSource,
    };
    use cssimpler::style::build_render_tree_in_viewport;

    #[test]
    fn stylesheet_parses() {
        let _ = super::stylesheet();
    }

    #[test]
    fn full_repaint_marker_changes_the_root_surface_style() {
        let mut scene = vec![
            RenderNode::container(cssimpler::core::LayoutBox::new(0.0, 0.0, 520.0, 680.0))
                .with_style(cssimpler::core::VisualStyle {
                    background: Some(Color::rgb(224, 239, 247)),
                    ..Default::default()
                }),
        ];

        super::apply_full_repaint_marker(&mut scene);

        assert_eq!(scene[0].style.background, Some(Color::rgb(224, 239, 246)));
    }

    #[test]
    fn tool_accent_keeps_css_radius_and_gradient() {
        let button = Node::element("button")
            .with_class("tool-button")
            .with_class("tool-folder")
            .with_style(super::tool_accent_style(
                crate::registry::ToolId::FolderPageCounter,
            ))
            .with_child(Node::element("span").with_class("tool-accent").into());
        let root = Node::element("div")
            .with_class("tool-list")
            .with_child(button.into())
            .into();

        let scene = build_render_tree_in_viewport(&root, &super::stylesheet(), 400, 140);
        let accent =
            find_node_with_gradient(&scene).expect("tool accent should resolve a gradient");

        assert_eq!(accent.layout.width, 18.0);
        assert_eq!(accent.layout.height, 92.0);
        assert!(accent.layout.x >= 0.0);
        assert_eq!(accent.style.corner_radius.top_left, 9.0);
        assert_eq!(accent.style.corner_radius.bottom_left, 9.0);

        let BackgroundLayer::LinearGradient(gradient) = &accent.style.background_layers[0] else {
            panic!("tool accent should use a linear gradient");
        };
        let expected = super::generated_accent_gradient(Color::rgb(34, 135, 214));
        assert_eq!(gradient.stops[0].color, expected.shadow);
        assert_eq!(gradient.stops[1].color, expected.body);
        assert_eq!(gradient.stops[2].color, expected.body);
        assert_eq!(gradient.stops[3].color, expected.saturated);
        assert_eq!(gradient.stops[4].color, expected.shifted);
        assert_eq!(gradient.stops[5].color, expected.end);
    }

    #[test]
    fn scroll_content_keeps_layout_position_with_shadow_gutter() {
        let root = Node::element("div")
            .with_id("app")
            .with_child(
                Node::element("section")
                    .with_class("shell")
                    .with_child(Node::element("header").with_class("topbar").into())
                    .with_child(
                        Node::element("main")
                            .with_id("shadow-scrollport")
                            .with_class("content")
                            .with_child(
                                Node::element("div")
                                    .with_id("shadow-content")
                                    .with_class("view")
                                    .with_child(
                                        Node::element("div")
                                            .with_id("shadow-card")
                                            .with_class("empty-state")
                                            .into(),
                                    )
                                    .into(),
                            )
                            .into(),
                    )
                    .into(),
            )
            .into();

        let scene = build_render_tree_in_viewport(
            &root,
            &super::stylesheet(),
            super::WINDOW_WIDTH,
            super::WINDOW_HEIGHT,
        );
        let scrollport = find_node_by_id(&scene, "shadow-scrollport")
            .expect("shadow scrollport should be rendered");
        let content =
            find_node_by_id(&scene, "shadow-content").expect("content should be rendered");
        let card = find_node_by_id(&scene, "shadow-card").expect("card should be rendered");

        assert_eq!(content.layout.x, 22.0);
        assert_eq!(content.layout.y, 146.0);
        assert_eq!(card.layout.x, 22.0);
        assert_eq!(scrollport.layout.x, content.layout.x - 18.0);
        assert_eq!(scrollport.layout.y, content.layout.y - 18.0);
        assert_eq!(scrollport.content_inset.left, 18.0);
        assert_eq!(scrollport.content_inset.top, 18.0);
        assert_eq!(scrollport.content_inset.bottom, 30.0);
    }

    #[test]
    fn topbar_uses_minimize_instead_of_quit() {
        let root = super::top_actions(super::View::Launcher);
        let scene = build_render_tree_in_viewport(&root, &super::stylesheet(), 476, 40);
        let mut text = Vec::new();
        collect_text(&scene, &mut text);

        assert!(text.iter().any(|value| value == "Minimize"));
        assert!(!text.iter().any(|value| value == "Quit"));
        assert!(
            scene
                .children
                .iter()
                .all(|button| button.layout.x + button.layout.width <= 476.0)
        );
    }

    #[test]
    fn brand_mark_uses_full_size_flat_app_icon_svg() {
        let root = Node::element("span")
            .with_class("brand-mark")
            .with_child(super::icon_app_brand())
            .into();

        let scene = build_render_tree_in_viewport(&root, &super::stylesheet(), 80, 80);
        let svg_node = find_svg_node(&scene).expect("brand mark should resolve as SVG");
        assert_eq!(svg_node.layout.width, 42.0);
        assert_eq!(svg_node.layout.height, 42.0);

        let RenderKind::Svg(svg) = &svg_node.kind else {
            panic!("brand mark should resolve as SVG");
        };

        assert_eq!(svg.paint_servers.len(), 0);
        assert_eq!(svg.paths.len(), 4);
        assert_eq!(
            svg.paths[0].paint.fill,
            Some(SvgPathPaintSource::Color(Color::rgb(20, 106, 158)))
        );
        assert_eq!(svg.paths[0].paint.stroke, None);

        for path in &svg.paths[1..] {
            assert_eq!(path.paint.fill, None);
            assert_eq!(
                path.paint.stroke,
                Some(SvgPathPaintSource::Color(Color::rgb(255, 255, 255)))
            );
        }
    }

    #[test]
    fn folder_icon_uses_mirrored_shape_and_accent_gradient() {
        let root = Node::element("span")
            .with_class("tool-icon")
            .with_class("folder")
            .with_style(super::tool_accent_style(
                crate::registry::ToolId::FolderPageCounter,
            ))
            .with_child(super::icon_folder())
            .into();

        let scene = build_render_tree_in_viewport(&root, &super::stylesheet(), 80, 80);
        let svg_node = find_svg_node(&scene).expect("folder icon should resolve as SVG");
        assert_eq!(svg_node.layout.width, 44.0);
        assert_eq!(svg_node.layout.height, 44.0);

        let RenderKind::Svg(svg) = &svg_node.kind else {
            panic!("folder icon should resolve as SVG");
        };

        assert_eq!(svg.paint_servers.len(), 1);
        let SvgPaintServerData::LinearGradient(gradient) = &svg.paint_servers[0].data;
        let expected = super::generated_svg_accent_gradient(Color::rgb(34, 135, 214));
        assert_eq!(gradient.stops[0].color, expected.shadow);
        assert_eq!(gradient.stops[1].color, expected.body);
        assert_eq!(gradient.stops[2].color, expected.body);
        assert_eq!(gradient.stops[3].color, expected.saturated);
        assert_eq!(gradient.stops[4].color, expected.shifted);
        assert_eq!(gradient.stops[5].color, expected.end);

        assert_eq!(svg.paths.len(), 2);
        let back_bounds = svg.paths[0]
            .geometry
            .bounds
            .expect("folder back path should have bounds");
        assert!((back_bounds.min_x - 2.0).abs() < 0.01);
        assert!((back_bounds.max_x - 20.0).abs() < 0.01);

        for path in &svg.paths {
            assert_eq!(path.paint.fill, None);
            let expected_stroke = super::FOLDER_ICON_STROKE_WIDTH
                .parse::<f32>()
                .expect("folder stroke width should stay numeric");
            assert!((path.paint.stroke_width - expected_stroke).abs() < 0.01);
            assert!(matches!(
                &path.paint.stroke,
                Some(SvgPathPaintSource::PaintServer(reference))
                    if reference.id == "folder-accent-gradient"
            ));
        }
    }

    #[test]
    fn usb_icon_uses_cleaned_shape_and_accent_gradient() {
        let root = Node::element("span")
            .with_class("tool-icon")
            .with_class("usb")
            .with_style(super::tool_accent_style(
                crate::registry::ToolId::UsbSafeEject,
            ))
            .with_child(super::icon_usb())
            .into();

        let scene = build_render_tree_in_viewport(&root, &super::stylesheet(), 80, 80);
        let svg_node = find_svg_node(&scene).expect("usb icon should resolve as SVG");
        assert_eq!(svg_node.layout.width, 44.0);
        assert_eq!(svg_node.layout.height, 44.0);

        let RenderKind::Svg(svg) = &svg_node.kind else {
            panic!("usb icon should resolve as SVG");
        };

        assert_eq!(svg.paint_servers.len(), 1);
        let SvgPaintServerData::LinearGradient(gradient) = &svg.paint_servers[0].data;
        let expected = super::generated_svg_accent_gradient(Color::rgb(30, 180, 132));
        assert_eq!(gradient.stops[0].color, expected.shadow);
        assert_eq!(gradient.stops[1].color, expected.body);
        assert_eq!(gradient.stops[2].color, expected.body);
        assert_eq!(gradient.stops[3].color, expected.saturated);
        assert_eq!(gradient.stops[4].color, expected.shifted);
        assert_eq!(gradient.stops[5].color, expected.end);

        assert_eq!(svg.paths.len(), 4);
        for path in &svg.paths {
            assert_eq!(path.paint.stroke, None);
            assert!(matches!(
                &path.paint.fill,
                Some(SvgPathPaintSource::PaintServer(reference))
                    if reference.id == "usb-accent-gradient"
            ));

            let bounds = path
                .geometry
                .bounds
                .expect("usb artwork path should have bounds");
            assert!(bounds.min_x > 0.0);
            assert!(bounds.min_y > 0.0);
            assert!(bounds.max_x < 2796.0);
            assert!(bounds.max_y < 2796.0);
        }
    }

    #[test]
    fn pdf_icon_uses_vector_badge_and_accent_gradient() {
        let root = Node::element("span")
            .with_class("tool-icon")
            .with_class("pdf")
            .with_style(super::tool_accent_style(crate::registry::ToolId::PdfJoiner))
            .with_child(super::icon_pdf_document())
            .into();

        let scene = build_render_tree_in_viewport(&root, &super::stylesheet(), 80, 80);
        let svg_node = find_svg_node(&scene).expect("pdf icon should resolve as SVG");
        assert_eq!(svg_node.layout.width, 44.0);
        assert_eq!(svg_node.layout.height, 44.0);

        let RenderKind::Svg(svg) = &svg_node.kind else {
            panic!("pdf icon should resolve as SVG");
        };

        assert_eq!(svg.paint_servers.len(), 1);
        let SvgPaintServerData::LinearGradient(gradient) = &svg.paint_servers[0].data;
        let expected = super::generated_svg_accent_gradient(Color::rgb(117, 86, 206));
        assert_eq!(gradient.stops[0].color, expected.shadow);
        assert_eq!(gradient.stops[1].color, expected.body);
        assert_eq!(gradient.stops[2].color, expected.body);
        assert_eq!(gradient.stops[3].color, expected.saturated);
        assert_eq!(gradient.stops[4].color, expected.shifted);
        assert_eq!(gradient.stops[5].color, expected.end);

        assert_eq!(svg.paths.len(), 3);
        for path in &svg.paths[0..2] {
            assert_eq!(path.paint.fill, None);
            assert!(matches!(
                &path.paint.stroke,
                Some(SvgPathPaintSource::PaintServer(reference))
                    if reference.id == "pdf-accent-gradient"
            ));
        }
        assert!(matches!(
            &svg.paths[2].paint.fill,
            Some(SvgPathPaintSource::PaintServer(reference))
                if reference.id == "pdf-accent-gradient"
        ));
        assert_eq!(svg.paths[2].paint.stroke, None);
        assert!(
            svg.paths[2].geometry.contours.len() >= 6,
            "badge should include reversed contours for PDF letter cutouts"
        );
    }

    fn find_node_with_gradient(node: &RenderNode) -> Option<&RenderNode> {
        if !node.style.background_layers.is_empty() {
            return Some(node);
        }

        node.children.iter().find_map(find_node_with_gradient)
    }

    fn find_svg_node(node: &RenderNode) -> Option<&RenderNode> {
        if let RenderKind::Svg(_) = &node.kind {
            return Some(node);
        }

        node.children.iter().find_map(find_svg_node)
    }

    fn find_node_by_id<'a>(node: &'a RenderNode, id: &str) -> Option<&'a RenderNode> {
        if node.element_id.as_deref() == Some(id) {
            return Some(node);
        }

        node.children
            .iter()
            .find_map(|child| find_node_by_id(child, id))
    }

    fn collect_text(node: &RenderNode, text: &mut Vec<String>) {
        if let RenderKind::Text(value) = &node.kind {
            text.push(value.clone());
        }

        for child in &node.children {
            collect_text(child, text);
        }
    }
}
