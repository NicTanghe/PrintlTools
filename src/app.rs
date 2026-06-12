use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process;
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
    Exit,
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
    Exit,
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

pub fn run() -> cssimpler::renderer::Result<()> {
    let app = PollingSceneProvider {
        inner: App::new(PrintLTools::new(), stylesheet(), update, view),
        profiler: FrameProfiler::from_env(),
    };

    cssimpler::renderer::run_with_scene_provider(
        WindowConfig {
            clear_color: Color::rgb(54, 67, 78),
            frame_time: Duration::from_millis(16),
            ..WindowConfig::new("PrintLTools", 900, 760)
                .with_glass_capable(true)
                .with_decorations(false)
        },
        app,
    )
}

struct PollingSceneProvider<P> {
    inner: P,
    profiler: FrameProfiler,
}

impl<P> SceneProvider for PollingSceneProvider<P>
where
    P: SceneProvider,
{
    fn update(&mut self, frame: FrameInfo) {
        self.profiler.record_frame(frame);
        self.inner.update(frame);
    }

    fn scene(&self) -> &[RenderNode] {
        self.inner.scene()
    }

    fn set_viewport(&mut self, viewport: ViewportSize) {
        self.inner.set_viewport(viewport);
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
            UiCommand::Exit => Self::Exit,
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
        Message::Exit => quit(),
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
        TrayEvent::Exit => quit(),
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

fn quit() -> bool {
    tray::shutdown();
    std::process::exit(0);
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
                            {icon_document()}
                        </span>
                        <div class="brand-copy">
                            <h1>PrintLTools</h1>
                            <p>Print and file utilities</p>
                        </div>
                    </div>
                    <div class="top-actions">
                        {nav_button("Launcher", icon_launch(), command_open_launcher, state.view == View::Launcher)}
                        {nav_button("Settings", icon_sliders(), command_open_settings, state.view == View::Settings)}
                        {add_class(nav_button("Quit", icon_exit(), command_exit, false), "danger")}
                    </div>
                </header>
                <main class="content">
                    {body}
                </main>
            </section>
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
                        "Open launcher on tray icon click",
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
    let mut tile = Node::element("span")
        .with_class("tool-icon")
        .with_class(match id {
            ToolId::FolderPageCounter => "folder",
            ToolId::UsbSafeEject => "usb",
            ToolId::PdfJoiner => "pdf",
        })
        .with_child(match id {
            ToolId::FolderPageCounter => icon_folder(),
            ToolId::UsbSafeEject => icon_usb(),
            ToolId::PdfJoiner => icon_document(),
        });

    if id == ToolId::PdfJoiner {
        tile = tile.with_child(
            Node::element("span")
                .with_class("pdf-badge")
                .with_child(Node::text("PDF"))
                .into(),
        );
    }

    tile.into()
}

fn icon_document() -> Node {
    svg_icon(
        "icon-document",
        &[
            "M7 3 L15 3 L19 7 L19 21 L7 21 L7 3 Z",
            "M15 3 L15 7 L19 7",
            "M10 12 L16 12",
            "M10 16 L15 16",
            "M10 8 L12 8",
        ],
        &[],
    )
}

fn icon_folder() -> Node {
    svg_icon(
        "icon-folder",
        &["M3 7 L9 7 L11 9 L21 9 L21 19 L3 19 Z", "M3 7 L3 19"],
        &[],
    )
}

fn icon_usb() -> Node {
    svg_icon(
        "icon-usb",
        &[
            "M12 3 L12 15",
            "M8 7 L12 3 L16 7",
            "M6 11 L18 11",
            "M8 11 L8 16 L12 20 L16 16 L16 11",
        ],
        &[],
    )
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

fn icon_exit() -> Node {
    svg_icon(
        "icon-exit",
        &["M10 7 L15 12 L10 17", "M15 12 L3 12", "M21 5 L21 19 L17 19"],
        &[],
    )
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

fn command_exit() {
    enqueue(UiCommand::Exit);
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
    use cssimpler::core::{BackgroundLayer, Color, Node, RenderNode};
    use cssimpler::style::build_render_tree_in_viewport;

    #[test]
    fn stylesheet_parses() {
        let _ = super::stylesheet();
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

        assert_eq!(accent.layout.width, 24.0);
        assert_eq!(accent.layout.height, 112.0);
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

    fn find_node_with_gradient(node: &RenderNode) -> Option<&RenderNode> {
        if !node.style.background_layers.is_empty() {
            return Some(node);
        }

        node.children.iter().find_map(find_node_with_gradient)
    }
}
