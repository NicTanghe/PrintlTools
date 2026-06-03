use std::thread;

use iced::futures::channel::oneshot;
use iced::widget::{button, checkbox, column, container, row, scrollable, text};
use iced::{Element, Fill, Subscription, Task, Theme, window};

use crate::dialogs;
use crate::page_counter::{self, PageCounterOptions};
use crate::pdf;
use crate::registry::{self, ToolDefinition, ToolId, ToolStatus};
use crate::results::{ResultLevel, ToolResult, display_path, display_paths};
use crate::tray::{self, TrayEvent};
use crate::usb::{self, DriveInfo};

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
    folder: std::path::PathBuf,
    include_subfolders: bool,
}

#[derive(Debug)]
pub struct PrintLTools {
    view: View,
    include_subfolders: bool,
    remember_last_folders: bool,
    open_launcher_on_tray_click: bool,
    powerpoint_slides_per_page: u32,
    page_counter_prompt: Option<PageCounterPrompt>,
    processing_title: Option<String>,
    processing_detail: Option<String>,
    usb_drives: Vec<DriveInfo>,
    last_window: Option<window::Id>,
    last_result: Option<ToolResult>,
}

#[derive(Debug, Clone)]
pub enum Message {
    WindowOpened(window::Id),
    WindowCloseRequested(window::Id),
    Tray(TrayEvent),
    OpenLauncher,
    OpenSettings,
    Exit,
    ToolPressed(ToolId),
    FolderPicked(Option<std::path::PathBuf>),
    PdfFilesPicked(Option<Vec<std::path::PathBuf>>),
    PdfOutputPicked {
        files: Vec<std::path::PathBuf>,
        output: Option<std::path::PathBuf>,
    },
    PdfMergeFinished(ToolResult),
    UsbDrivesLoaded(Result<Vec<DriveInfo>, String>),
    UsbDriveSelected(DriveInfo),
    UsbEjectFinished(ToolResult),
    IncludeSubfoldersChanged(bool),
    PowerPointSlidesPerPageChanged(u32),
    RunPendingPageCounter,
    PageCounterFinished(ToolResult),
    CancelPendingTool,
    RememberLastFoldersChanged(bool),
    OpenOnTrayClickChanged(bool),
    DismissResult,
}

pub fn run() -> iced::Result {
    iced::application(PrintLTools::default, update, view)
        .title("PrintLTools")
        .theme(theme)
        .window_size((520.0, 620.0))
        .resizable(true)
        .centered()
        .exit_on_close_request(false)
        .subscription(subscription)
        .run()
}

fn theme(_state: &PrintLTools) -> Theme {
    Theme::Light
}

impl Default for PrintLTools {
    fn default() -> Self {
        Self {
            view: View::Launcher,
            include_subfolders: false,
            remember_last_folders: true,
            open_launcher_on_tray_click: true,
            powerpoint_slides_per_page: 4,
            page_counter_prompt: None,
            processing_title: None,
            processing_detail: None,
            usb_drives: Vec::new(),
            last_window: None,
            last_result: None,
        }
    }
}

fn subscription(_state: &PrintLTools) -> Subscription<Message> {
    Subscription::batch([
        window::open_events().map(Message::WindowOpened),
        window::close_requests().map(Message::WindowCloseRequested),
        tray::subscription().map(Message::Tray),
    ])
}

fn update(state: &mut PrintLTools, message: Message) -> Task<Message> {
    match message {
        Message::WindowOpened(id) => {
            state.last_window = Some(id);
            Task::none()
        }
        Message::WindowCloseRequested(id) => {
            state.last_window = Some(id);
            window::set_mode(id, window::Mode::Hidden)
        }
        Message::Tray(event) => handle_tray_event(state, event),
        Message::OpenLauncher => {
            state.view = View::Launcher;
            show_launcher(state)
        }
        Message::OpenSettings => {
            state.view = View::Settings;
            show_launcher(state)
        }
        Message::Exit => quit(state),
        Message::ToolPressed(id) => start_tool(state, id),
        Message::FolderPicked(folder) => handle_folder_picked(state, folder),
        Message::PdfFilesPicked(files) => handle_pdf_files_picked(state, files),
        Message::PdfOutputPicked { files, output } => {
            handle_pdf_output_picked(state, files, output)
        }
        Message::PdfMergeFinished(result) => record_result(state, result),
        Message::UsbDrivesLoaded(result) => handle_usb_drives_loaded(state, result),
        Message::UsbDriveSelected(drive) => handle_usb_drive_selected(state, drive),
        Message::UsbEjectFinished(result) => record_result(state, result),
        Message::IncludeSubfoldersChanged(value) => {
            state.include_subfolders = value;
            Task::none()
        }
        Message::PowerPointSlidesPerPageChanged(value) => {
            state.powerpoint_slides_per_page = value;
            Task::none()
        }
        Message::RunPendingPageCounter => {
            let Some(prompt) = state.page_counter_prompt.clone() else {
                return Task::none();
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

            Task::perform(
                run_result_on_thread(move || run_page_counter(options)),
                Message::PageCounterFinished,
            )
        }
        Message::PageCounterFinished(result) => record_result(state, result),
        Message::CancelPendingTool => {
            state.page_counter_prompt = None;
            state.processing_title = None;
            state.processing_detail = None;
            state.usb_drives.clear();
            state.view = View::Launcher;
            Task::none()
        }
        Message::RememberLastFoldersChanged(value) => {
            state.remember_last_folders = value;
            Task::none()
        }
        Message::OpenOnTrayClickChanged(value) => {
            state.open_launcher_on_tray_click = value;
            Task::none()
        }
        Message::DismissResult => {
            state.last_result = None;
            state.view = View::Launcher;
            Task::none()
        }
    }
}

fn handle_tray_event(state: &mut PrintLTools, event: TrayEvent) -> Task<Message> {
    match event {
        TrayEvent::OpenLauncher => {
            if state.open_launcher_on_tray_click {
                state.view = View::Launcher;
                show_launcher(state)
            } else {
                Task::none()
            }
        }
        TrayEvent::OpenSettings => {
            state.view = View::Settings;
            show_launcher(state)
        }
        TrayEvent::Exit => quit(state),
        TrayEvent::Error(error) => {
            let result = ToolResult::error(
                "Tray integration",
                "The app is running, but the Windows tray icon could not be initialized.",
                vec![error],
            );
            record_result(state, result)
        }
    }
}

fn quit(state: &PrintLTools) -> Task<Message> {
    tray::shutdown();

    if let Some(id) = state.last_window {
        window::close(id)
    } else {
        std::process::exit(0);
    }
}

fn show_launcher(state: &PrintLTools) -> Task<Message> {
    if let Some(id) = state.last_window {
        Task::batch([
            window::set_mode(id, window::Mode::Windowed),
            window::gain_focus(id),
        ])
    } else {
        Task::none()
    }
}

fn start_tool(state: &mut PrintLTools, id: ToolId) -> Task<Message> {
    match id {
        ToolId::FolderPageCounter => Task::perform(
            dialogs::pick_folder_threaded("Select folder for page counting"),
            Message::FolderPicked,
        ),
        ToolId::UsbSafeEject => {
            state.processing_title = Some("Loading drives".to_string());
            state.processing_detail = Some("Scanning removable and external drives.".to_string());
            state.view = View::Processing;

            Task::perform(
                async {
                    run_on_worker_thread(usb::list_drives)
                        .await
                        .unwrap_or_else(Err)
                },
                Message::UsbDrivesLoaded,
            )
        }
        ToolId::PdfJoiner => Task::perform(
            dialogs::pick_pdf_files_threaded("Select PDF files to join"),
            Message::PdfFilesPicked,
        ),
    }
}

fn handle_folder_picked(
    state: &mut PrintLTools,
    folder: Option<std::path::PathBuf>,
) -> Task<Message> {
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

    Task::none()
}

fn handle_pdf_files_picked(
    state: &mut PrintLTools,
    files: Option<Vec<std::path::PathBuf>>,
) -> Task<Message> {
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

    Task::perform(
        dialogs::save_pdf_file_threaded("Save joined PDF as", "joined.pdf"),
        move |output| Message::PdfOutputPicked { files, output },
    )
}

fn handle_pdf_output_picked(
    state: &mut PrintLTools,
    files: Vec<std::path::PathBuf>,
    output: Option<std::path::PathBuf>,
) -> Task<Message> {
    let Some(output) = output else {
        return Task::done(Message::PdfMergeFinished(ToolResult::info(
            "PDF joiner",
            "Output selection was canceled.",
            Vec::new(),
        )));
    };

    state.processing_title = Some("Joining PDFs".to_string());
    state.processing_detail = Some(format!("Merging {} selected PDF files.", files.len()));
    state.view = View::Processing;

    Task::perform(
        run_result_on_thread(move || merge_pdfs_result(files, output)),
        Message::PdfMergeFinished,
    )
}

fn handle_usb_drives_loaded(
    state: &mut PrintLTools,
    result: Result<Vec<DriveInfo>, String>,
) -> Task<Message> {
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
            Task::none()
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

fn handle_usb_drive_selected(state: &mut PrintLTools, drive: DriveInfo) -> Task<Message> {
    state.processing_title = Some("Preparing USB eject".to_string());
    state.processing_detail = Some(format!("Closing processes and ejecting {}.", drive.letter));
    state.view = View::Processing;

    Task::perform(
        run_result_on_thread(move || usb_eject_result(drive)),
        Message::UsbEjectFinished,
    )
}

fn merge_pdfs_result(files: Vec<std::path::PathBuf>, output: std::path::PathBuf) -> ToolResult {
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

fn record_result(state: &mut PrintLTools, result: ToolResult) -> Task<Message> {
    state.last_result = Some(result);
    state.processing_title = None;
    state.processing_detail = None;
    state.usb_drives.clear();
    state.view = View::Result;

    show_launcher(state)
}

async fn run_result_on_thread(f: impl FnOnce() -> ToolResult + Send + 'static) -> ToolResult {
    run_on_worker_thread(f).await.unwrap_or_else(|error| {
        ToolResult::error(
            "Background task",
            "The operation stopped before returning a result.",
            vec![error],
        )
    })
}

async fn run_on_worker_thread<T>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String>
where
    T: Send + 'static,
{
    let (sender, receiver) = oneshot::channel();

    let _ = thread::Builder::new()
        .name("printltools-worker".to_string())
        .spawn(move || {
            let _ = sender.send(f());
        })
        .map_err(|error| error.to_string())?;

    receiver
        .await
        .map_err(|_| "Worker thread stopped before returning a result.".to_string())
}

fn view(state: &PrintLTools) -> Element<'_, Message> {
    let header = row![
        text("PrintLTools").size(30),
        button("Launcher").on_press(Message::OpenLauncher),
        button("Settings").on_press(Message::OpenSettings),
        button("Quit").on_press(Message::Exit),
    ]
    .spacing(10)
    .align_y(iced::Alignment::Center);

    let body = match state.view {
        View::Launcher => launcher_view(state),
        View::Settings => settings_view(state),
        View::PageCounterPrompt => page_counter_prompt_view(state),
        View::UsbDriveSelect => usb_drive_select_view(state),
        View::Processing => processing_view(state),
        View::Result => result_view(state),
    };

    container(column![header, body].spacing(18))
        .padding(20)
        .width(Fill)
        .height(Fill)
        .into()
}

fn launcher_view(state: &PrintLTools) -> Element<'_, Message> {
    let mut tools = column![].spacing(10);

    for tool in registry::all_tools() {
        tools = tools.push(tool_button(tool));
    }

    let result_panel: Element<'_, Message> = if let Some(result) = &state.last_result {
        let level = match result.level {
            ResultLevel::Info => "Info",
            ResultLevel::Warning => "Warning",
            ResultLevel::Error => "Error",
        };

        let mut details = column![
            text(format!("{}: {}", level, result.title)).size(16),
            text(&result.summary)
        ]
        .spacing(6);

        for detail in &result.details {
            details = details.push(text(detail).size(13));
        }

        container(column![details, button("Dismiss").on_press(Message::DismissResult)].spacing(10))
            .padding(12)
            .width(Fill)
            .into()
    } else {
        container(text("Select a tool to start."))
            .padding(12)
            .into()
    };

    scrollable(
        column![
            text("Tools").size(20),
            tools,
            text("Folder options").size(20),
            checkbox(state.include_subfolders)
                .label("Include subfolders")
                .on_toggle(Message::IncludeSubfoldersChanged),
            text("Latest result").size(20),
            result_panel,
        ]
        .spacing(14),
    )
    .into()
}

fn settings_view(state: &PrintLTools) -> Element<'_, Message> {
    column![
        text("Settings").size(20),
        checkbox(state.open_launcher_on_tray_click)
            .label("Open launcher on tray icon click")
            .on_toggle(Message::OpenOnTrayClickChanged),
        checkbox(state.remember_last_folders)
            .label("Remember last folders")
            .on_toggle(Message::RememberLastFoldersChanged),
        text("Start with Windows and persisted settings are planned for Epic F."),
    ]
    .spacing(14)
    .into()
}

fn result_view(state: &PrintLTools) -> Element<'_, Message> {
    let Some(result) = &state.last_result else {
        return container(
            column![
                text("No result").size(20),
                button("Back to launcher").on_press(Message::DismissResult),
            ]
            .spacing(14),
        )
        .padding(12)
        .into();
    };

    let level = match result.level {
        ResultLevel::Info => "Info",
        ResultLevel::Warning => "Warning",
        ResultLevel::Error => "Error",
    };

    let mut details = column![
        text(format!("{}: {}", level, result.title)).size(20),
        text(&result.summary).size(16),
    ]
    .spacing(8);

    for detail in &result.details {
        details = details.push(text(detail).size(13));
    }

    scrollable(
        column![
            details,
            row![
                button("Back to launcher").on_press(Message::DismissResult),
                button("Keep result").on_press(Message::OpenLauncher),
            ]
            .spacing(10),
        ]
        .spacing(16),
    )
    .into()
}

fn page_counter_prompt_view(state: &PrintLTools) -> Element<'_, Message> {
    let Some(prompt) = &state.page_counter_prompt else {
        return container(text("No folder selected.")).padding(12).into();
    };

    let mut slide_options = row![].spacing(8);
    for value in [1, 2, 3, 4, 6, 9] {
        let label = if state.powerpoint_slides_per_page == value {
            format!("{value} selected")
        } else {
            value.to_string()
        };

        slide_options = slide_options
            .push(button(text(label)).on_press(Message::PowerPointSlidesPerPageChanged(value)));
    }

    column![
        text("Folder page counter").size(20),
        text(format!("Folder: {}", display_path(&prompt.folder))),
        text(format!(
            "Include subfolders: {}",
            if prompt.include_subfolders {
                "yes"
            } else {
                "no"
            }
        )),
        text("PowerPoint slides per printed page").size(16),
        slide_options,
        row![
            button("Count pages").on_press(Message::RunPendingPageCounter),
            button("Cancel").on_press(Message::CancelPendingTool),
        ]
        .spacing(10),
    ]
    .spacing(14)
    .into()
}

fn usb_drive_select_view(state: &PrintLTools) -> Element<'_, Message> {
    let mut drives = column![].spacing(10);

    for drive in &state.usb_drives {
        drives = drives.push(
            button(
                column![
                    text(drive.display_name()).size(16),
                    text(format!("Root: {}", display_path(&drive.root))).size(12),
                ]
                .spacing(4),
            )
            .width(Fill)
            .on_press(Message::UsbDriveSelected(drive.clone())),
        );
    }

    scrollable(
        column![
            text("USB safe eject").size(20),
            text("Select the drive to close locking processes and request safe eject."),
            drives,
            button("Cancel").on_press(Message::CancelPendingTool),
        ]
        .spacing(14),
    )
    .into()
}

fn processing_view(state: &PrintLTools) -> Element<'_, Message> {
    column![
        text(state.processing_title.as_deref().unwrap_or("Processing")).size(20),
        text(
            state
                .processing_detail
                .as_deref()
                .unwrap_or("The selected operation is still running.")
        ),
        text("You can leave this window open while the background worker finishes."),
    ]
    .spacing(14)
    .into()
}

fn tool_button(tool: &'static ToolDefinition) -> Element<'static, Message> {
    let status = match tool.status {
        ToolStatus::Ready => "Ready",
        ToolStatus::Planned => "Planned",
    };

    let content = column![
        text(tool.name).size(18),
        text(tool.description).size(13),
        text(format!("{} - {}", tool.short_name, status)).size(12),
    ]
    .spacing(4);

    let button = button(container(content).width(Fill))
        .padding(12)
        .width(Fill);

    match tool.status {
        ToolStatus::Ready => button.on_press(Message::ToolPressed(tool.id)).into(),
        ToolStatus::Planned => button.into(),
    }
}
