use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveInfo {
    pub root: PathBuf,
    pub letter: String,
    pub label: String,
    pub drive_type: DriveKind,
    pub total_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveKind {
    Removable,
    Fixed,
}

#[derive(Debug, Clone)]
pub struct UsbEjectSummary {
    pub drive: DriveInfo,
    pub resources_registered: usize,
    pub resource_scan_limited: bool,
    pub restart_manager_processes: Vec<ProcessAction>,
    pub process_scan_actions: Vec<ProcessAction>,
    pub notes: Vec<String>,
    pub ejected: bool,
    pub admin_required: bool,
    pub eject_method: EjectMethod,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessAction {
    pub pid: u32,
    pub name: String,
    pub path: Option<String>,
    pub action: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EjectMethod {
    None,
    Shell,
    Mountvol,
}

pub fn list_drives() -> Result<Vec<DriveInfo>, String> {
    platform::list_drives()
}

pub fn safe_eject(drive: DriveInfo) -> Result<UsbEjectSummary, String> {
    platform::safe_eject(drive)
}

pub fn run_eject_helper_from_args(args: Vec<OsString>) -> Option<i32> {
    platform::run_eject_helper_from_args(args)
}

pub fn format_bytes(value: Option<u64>) -> String {
    let Some(value) = value else {
        return "unknown".to_string();
    };

    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut unit = 0;
    let mut amount = value as f64;

    while amount >= 1024.0 && unit + 1 < UNITS.len() {
        amount /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{} {}", value, UNITS[unit])
    } else {
        format!("{amount:.1} {}", UNITS[unit])
    }
}

impl DriveInfo {
    pub fn display_name(&self) -> String {
        let label = if self.label.is_empty() {
            "Untitled"
        } else {
            &self.label
        };

        format!(
            "{} {} - {} total, {} free ({})",
            self.letter,
            label,
            format_bytes(self.total_bytes),
            format_bytes(self.free_bytes),
            self.drive_type.label()
        )
    }
}

impl DriveKind {
    pub fn label(self) -> &'static str {
        match self {
            DriveKind::Removable => "removable",
            DriveKind::Fixed => "fixed/possible external",
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::collections::{HashMap, HashSet, VecDeque};

    use windows::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    };
    use windows::Win32::System::RestartManager::{
        CCH_RM_SESSION_KEY, RM_PROCESS_INFO, RmEndSession, RmForceShutdown, RmGetList,
        RmRegisterResources, RmShutdown, RmStartSession,
    };
    use windows::core::{PCWSTR, PWSTR};

    use super::*;

    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    const ERROR_MORE_DATA: u32 = 234;
    const ERROR_SUCCESS: u32 = 0;
    const HELPER_ARG: &str = "--usb-eject-helper";
    const MAX_RESTART_MANAGER_RESOURCES: usize = 4096;

    pub fn list_drives() -> Result<Vec<DriveInfo>, String> {
        let mask = unsafe { GetLogicalDrives() };
        if mask == 0 {
            return Err("GetLogicalDrives returned no drives.".to_string());
        }

        let system_drive = std::env::var("SystemDrive")
            .unwrap_or_else(|_| "C:".to_string())
            .to_ascii_uppercase();
        let mut drives = Vec::new();

        for index in 0..26 {
            if mask & (1 << index) == 0 {
                continue;
            }

            let letter_char = (b'A' + index as u8) as char;
            let letter = format!("{letter_char}:");
            let root = format!("{letter}\\");
            let root_wide = wide_null(&root);
            let drive_type = unsafe { GetDriveTypeW(PCWSTR(root_wide.as_ptr())) };

            let kind = match drive_type {
                DRIVE_REMOVABLE => DriveKind::Removable,
                DRIVE_FIXED if letter.to_ascii_uppercase() != system_drive => DriveKind::Fixed,
                _ => continue,
            };

            let (total_bytes, free_bytes) = drive_space(&root_wide);

            drives.push(DriveInfo {
                root: PathBuf::from(&root),
                letter,
                label: volume_label(&root_wide),
                drive_type: kind,
                total_bytes,
                free_bytes,
            });
        }

        drives.sort_by(|a, b| a.letter.cmp(&b.letter));
        Ok(drives)
    }

    pub fn safe_eject(drive: DriveInfo) -> Result<UsbEjectSummary, String> {
        safe_eject_impl(drive, true)
    }

    fn safe_eject_impl(drive: DriveInfo, allow_elevation: bool) -> Result<UsbEjectSummary, String> {
        let root = normalize_drive_root(&drive.root)?;
        let (resources, resource_scan_limited, mut notes) =
            collect_restart_manager_resources(&root);
        let mut restart_manager_processes = Vec::new();
        let mut admin_required = false;

        match close_restart_manager_locks(&resources) {
            Ok(actions) => restart_manager_processes = actions,
            Err(RestartManagerError::AccessDenied(error)) if allow_elevation => {
                return run_elevated_helper(&drive, &root).map(|mut summary| {
                    summary.drive = drive;
                    summary
                });
            }
            Err(RestartManagerError::AccessDenied(error)) => {
                admin_required = true;
                notes.push(error);
                notes.push(
                    "Restart Manager could not inspect protected locking processes even in the elevated helper."
                        .to_string(),
                );
            }
            Err(RestartManagerError::Other(error)) => {
                notes.push(format!("Restart Manager could not close locks: {error}"));
            }
        }

        match close_explorer_windows_on_drive(&root) {
            Ok(actions) => {
                if !actions.is_empty() {
                    notes.push(format!(
                        "Closed {} Explorer window(s) open on the selected drive.",
                        actions.len()
                    ));
                }
            }
            Err(error) => notes.push(format!("Explorer window cleanup failed: {error}")),
        }

        let process_scan_actions = match close_processes_referencing_drive(&root) {
            Ok(process_scan_actions) => {
                restart_explorer_if_needed(&process_scan_actions, &mut notes);
                process_scan_actions
            }
            Err(error) => {
                notes.push(format!("Process scan failed: {error}"));
                Vec::new()
            }
        };

        let process_cleanup_failed = process_scan_actions
            .iter()
            .any(|action| action.action.starts_with("failed"));
        if process_cleanup_failed {
            notes.push(
                "Eject was not requested because one or more matched processes could not be closed."
                    .to_string(),
            );
        }

        let lock_check_failed = match inspect_restart_manager_locks(&resources) {
            Ok(remaining_locks) => {
                let has_locks = !remaining_locks.is_empty();
                if has_locks {
                    merge_process_actions(&mut restart_manager_processes, remaining_locks);
                    notes.push(
                        "Eject was not requested because Restart Manager still reports processes locking the drive."
                            .to_string(),
                    );
                }
                false
            }
            Err(RestartManagerError::AccessDenied(error)) if allow_elevation => {
                return run_elevated_helper(&drive, &root).map(|mut summary| {
                    summary.drive = drive;
                    summary
                });
            }
            Err(RestartManagerError::AccessDenied(error)) => {
                admin_required = true;
                notes.push(error);
                notes.push(
                    "Eject was not requested because remaining drive locks could not be inspected."
                        .to_string(),
                );
                true
            }
            Err(RestartManagerError::Other(error)) => {
                notes.push(format!("Final lock check failed: {error}"));
                notes.push(
                    "Eject was not requested because remaining drive locks could not be verified."
                        .to_string(),
                );
                true
            }
        };

        let has_remaining_locks = restart_manager_processes
            .iter()
            .any(|action| action.action == "still locking after cleanup");
        let can_request_eject = !admin_required
            && !process_cleanup_failed
            && !lock_check_failed
            && !has_remaining_locks;

        let eject_result = if can_request_eject {
            Some(request_shell_eject(&root))
        } else {
            None
        };
        let ejected = eject_result.as_ref().is_some_and(Result::is_ok);
        let mut eject_method = EjectMethod::None;
        match eject_result {
            Some(Ok(outcome)) => {
                eject_method = outcome.method;
                notes.push(outcome.note);
            }
            Some(Err(error)) => notes.push(error),
            None => {}
        }

        Ok(UsbEjectSummary {
            drive,
            resources_registered: resources.len(),
            resource_scan_limited,
            restart_manager_processes,
            process_scan_actions,
            notes,
            ejected,
            admin_required,
            eject_method,
        })
    }

    fn drive_space(root_wide: &[u16]) -> (Option<u64>, Option<u64>) {
        let mut total = 0;
        let mut free = 0;

        match unsafe {
            GetDiskFreeSpaceExW(
                PCWSTR(root_wide.as_ptr()),
                None,
                Some(&mut total),
                Some(&mut free),
            )
        } {
            Ok(()) => (Some(total), Some(free)),
            Err(_) => (None, None),
        }
    }

    fn volume_label(root_wide: &[u16]) -> String {
        let mut label = [0u16; 261];

        if unsafe {
            GetVolumeInformationW(
                PCWSTR(root_wide.as_ptr()),
                Some(&mut label),
                None,
                None,
                None,
                None,
            )
        }
        .is_err()
        {
            return String::new();
        }

        wide_fixed_to_string(&label)
    }

    fn normalize_drive_root(root: &Path) -> Result<PathBuf, String> {
        let value = root.to_string_lossy();
        let mut chars = value.chars();
        let Some(letter) = chars.next() else {
            return Err("Selected drive path is empty.".to_string());
        };

        if !letter.is_ascii_alphabetic() {
            return Err(format!(
                "Selected path `{value}` does not start with a drive letter."
            ));
        }

        Ok(PathBuf::from(format!("{}:\\", letter.to_ascii_uppercase())))
    }

    fn collect_restart_manager_resources(root: &Path) -> (Vec<PathBuf>, bool, Vec<String>) {
        let mut resources = Vec::new();
        let mut notes = Vec::new();
        let mut limited = false;
        let mut queue = VecDeque::from([root.to_path_buf()]);

        resources.push(root.to_path_buf());

        while let Some(folder) = queue.pop_front() {
            if resources.len() >= MAX_RESTART_MANAGER_RESOURCES {
                limited = true;
                break;
            }

            let entries = match fs::read_dir(&folder) {
                Ok(entries) => entries,
                Err(error) => {
                    notes.push(format!("Could not scan {}: {error}", folder.display()));
                    continue;
                }
            };

            for entry in entries.filter_map(Result::ok) {
                if resources.len() >= MAX_RESTART_MANAGER_RESOURCES {
                    limited = true;
                    break;
                }

                let path = entry.path();
                if path.is_dir() {
                    queue.push_back(path);
                } else if path.is_file() {
                    resources.push(path);
                }
            }
        }

        (resources, limited, notes)
    }

    #[derive(Debug)]
    enum RestartManagerError {
        AccessDenied(String),
        Other(String),
    }

    impl From<String> for RestartManagerError {
        fn from(value: String) -> Self {
            RestartManagerError::Other(value)
        }
    }

    struct EjectOutcome {
        method: EjectMethod,
        note: String,
    }

    fn close_restart_manager_locks(
        resources: &[PathBuf],
    ) -> Result<Vec<ProcessAction>, RestartManagerError> {
        let session = RmSession::new()?;
        session.register_resources(resources)?;

        let initial = session.processes()?;
        if initial.is_empty() {
            return Ok(Vec::new());
        }

        let mut actions = process_actions_from_rm(&initial, "detected by Restart Manager");
        session.shutdown(false)?;
        thread::sleep(Duration::from_millis(1200));

        let remaining_after_graceful = session.processes()?;
        if !remaining_after_graceful.is_empty() {
            session.shutdown(true)?;
            thread::sleep(Duration::from_millis(1200));
        }

        let remaining_pids = session
            .processes()?
            .into_iter()
            .map(|process| process.Process.dwProcessId)
            .collect::<HashSet<_>>();

        for action in &mut actions {
            action.action = if remaining_pids.contains(&action.pid) {
                "still locking after Restart Manager shutdown".to_string()
            } else {
                "closed by Restart Manager".to_string()
            };
        }

        Ok(actions)
    }

    fn process_actions_from_rm(processes: &[RM_PROCESS_INFO], action: &str) -> Vec<ProcessAction> {
        processes
            .iter()
            .map(|process| ProcessAction {
                pid: process.Process.dwProcessId,
                name: wide_fixed_to_string(&process.strAppName),
                path: None,
                action: action.to_string(),
            })
            .collect()
    }

    fn inspect_restart_manager_locks(
        resources: &[PathBuf],
    ) -> Result<Vec<ProcessAction>, RestartManagerError> {
        let session = RmSession::new()?;
        session.register_resources(resources)?;
        let locks = session.processes()?;

        Ok(process_actions_from_rm(
            &locks,
            "still locking after cleanup",
        ))
    }

    fn merge_process_actions(target: &mut Vec<ProcessAction>, additions: Vec<ProcessAction>) {
        for addition in additions {
            if let Some(existing) = target.iter_mut().find(|action| action.pid == addition.pid) {
                if existing.name.is_empty() {
                    existing.name = addition.name;
                }
                if existing.path.is_none() {
                    existing.path = addition.path;
                }
                existing.action = addition.action;
            } else {
                target.push(addition);
            }
        }
    }

    struct RmSession {
        handle: u32,
    }

    impl RmSession {
        fn new() -> Result<Self, String> {
            let mut handle = 0;
            let mut session_key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
            let error =
                unsafe { RmStartSession(&mut handle, None, PWSTR(session_key.as_mut_ptr())) };

            if error.0 != ERROR_SUCCESS {
                return Err(format!("RmStartSession failed: {}", error.0));
            }

            Ok(Self { handle })
        }

        fn register_resources(&self, resources: &[PathBuf]) -> Result<(), String> {
            for chunk in resources.chunks(512) {
                let wide_paths = chunk
                    .iter()
                    .map(|path| wide_null(&path.to_string_lossy()))
                    .collect::<Vec<_>>();
                let pointers = wide_paths
                    .iter()
                    .map(|path| PCWSTR(path.as_ptr()))
                    .collect::<Vec<_>>();

                let error =
                    unsafe { RmRegisterResources(self.handle, Some(&pointers), None, None) };

                if error.0 != ERROR_SUCCESS {
                    return Err(format!("RmRegisterResources failed: {}", error.0));
                }
            }

            Ok(())
        }

        fn processes(&self) -> Result<Vec<RM_PROCESS_INFO>, RestartManagerError> {
            let mut needed = 0;
            let mut count = 0;
            let mut reboot_reasons = 0;
            let error = unsafe {
                RmGetList(
                    self.handle,
                    &mut needed,
                    &mut count,
                    None,
                    &mut reboot_reasons,
                )
            };

            match error.0 {
                ERROR_SUCCESS => Ok(Vec::new()),
                ERROR_MORE_DATA => {
                    let mut processes = vec![RM_PROCESS_INFO::default(); needed as usize];
                    count = needed;
                    let error = unsafe {
                        RmGetList(
                            self.handle,
                            &mut needed,
                            &mut count,
                            Some(processes.as_mut_ptr()),
                            &mut reboot_reasons,
                        )
                    };

                    if error.0 != ERROR_SUCCESS {
                        return Err(RestartManagerError::Other(format!(
                            "RmGetList failed: {}",
                            error.0
                        )));
                    }

                    processes.truncate(count as usize);
                    Ok(processes)
                }
                5 => Err(RestartManagerError::AccessDenied(
                    "RmGetList failed with access denied while inspecting protected locking processes."
                        .to_string(),
                )),
                other => Err(RestartManagerError::Other(format!("RmGetList failed: {other}"))),
            }
        }

        fn shutdown(&self, force: bool) -> Result<(), String> {
            let flags = if force { RmForceShutdown.0 as u32 } else { 0 };
            let error = unsafe { RmShutdown(self.handle, flags, None) };

            if error.0 == ERROR_SUCCESS {
                Ok(())
            } else {
                Err(format!("RmShutdown failed: {}", error.0))
            }
        }
    }

    impl Drop for RmSession {
        fn drop(&mut self) {
            let _ = unsafe { RmEndSession(self.handle) };
        }
    }

    fn close_explorer_windows_on_drive(root: &Path) -> Result<Vec<ProcessAction>, String> {
        let root = root.to_string_lossy();
        let alt_root = root.replace('\\', "/");
        let script = format!(
            r#"
$ErrorActionPreference = 'Stop'
$root = '{}'
$altRoot = '{}'
function Clean([string]$value) {{
    if ($null -eq $value) {{ return '' }}
    return ($value -replace "`t", ' ' -replace "`r", ' ' -replace "`n", ' ')
}}
function WindowPath($window) {{
    try {{
        $path = $window.Document.Folder.Self.Path
        if (-not [string]::IsNullOrWhiteSpace($path)) {{ return $path }}
    }} catch {{ }}
    try {{
        $location = $window.LocationURL
        if (-not [string]::IsNullOrWhiteSpace($location) -and $location.StartsWith('file:', [System.StringComparison]::OrdinalIgnoreCase)) {{
            return ([System.Uri]$location).LocalPath
        }}
    }} catch {{ }}
    return ''
}}
$shell = New-Object -ComObject Shell.Application
$windows = @($shell.Windows())
foreach ($window in $windows) {{
    $fullName = ''
    try {{ $fullName = [System.IO.Path]::GetFileName($window.FullName) }} catch {{ }}
    if ($fullName -and $fullName -ne 'explorer.exe') {{
        continue
    }}
    $path = WindowPath $window
    if (
        $path -and (
            $path.StartsWith($root, [System.StringComparison]::OrdinalIgnoreCase) -or
            $path.StartsWith($altRoot, [System.StringComparison]::OrdinalIgnoreCase)
        )
    ) {{
        try {{
            $window.Quit()
            Write-Output ("0`texplorer.exe`t{{0}}`tclosed Explorer window" -f (Clean $path))
        }} catch {{
            Write-Output ("0`texplorer.exe`t{{0}}`tfailed: {{1}}" -f (Clean $path), (Clean $_.Exception.Message))
        }}
    }}
}}
"#,
            powershell_single_quoted(&root),
            powershell_single_quoted(&alt_root)
        );

        let output = run_powershell(&script, Duration::from_secs(20), "Explorer window cleanup")?;
        Ok(parse_process_actions(&output))
    }

    fn close_processes_referencing_drive(root: &Path) -> Result<Vec<ProcessAction>, String> {
        let root = root.to_string_lossy();
        let alt_root = root.replace('\\', "/");
        let script = format!(
            r#"
$ErrorActionPreference = 'Stop'
$root = '{}'
$altRoot = '{}'
$currentPid = $PID
function Clean([string]$value) {{
    if ($null -eq $value) {{ return '' }}
    return ($value -replace "`t", ' ' -replace "`r", ' ' -replace "`n", ' ')
}}
$matches = Get-CimInstance Win32_Process | Where-Object {{
    $_.ProcessId -ne $currentPid -and $_.ProcessId -gt 4 -and (
        ($_.ExecutablePath -and $_.ExecutablePath.StartsWith($root, [System.StringComparison]::OrdinalIgnoreCase)) -or
        ($_.CommandLine -and $_.CommandLine.IndexOf($root, [System.StringComparison]::OrdinalIgnoreCase) -ge 0) -or
        ($_.CommandLine -and $_.CommandLine.IndexOf($altRoot, [System.StringComparison]::OrdinalIgnoreCase) -ge 0)
    )
}}
foreach ($proc in $matches) {{
    $status = 'matched'
    try {{
        $p = Get-Process -Id $proc.ProcessId -ErrorAction Stop
        if ($p.MainWindowHandle -ne 0) {{
            [void]$p.CloseMainWindow()
            Start-Sleep -Milliseconds 1200
        }}
        $p.Refresh()
        if (-not $p.HasExited) {{
            Stop-Process -Id $proc.ProcessId -Force -ErrorAction Stop
            $status = 'terminated'
        }} else {{
            $status = 'closed'
        }}
    }} catch {{
        $status = 'failed: ' + $_.Exception.Message
    }}
    Write-Output ("{{0}}`t{{1}}`t{{2}}`t{{3}}" -f $proc.ProcessId, (Clean $proc.Name), (Clean $proc.ExecutablePath), (Clean $status))
}}
"#,
            powershell_single_quoted(&root),
            powershell_single_quoted(&alt_root)
        );

        let output = run_powershell(&script, Duration::from_secs(60), "process cleanup")?;
        Ok(parse_process_actions(&output))
    }

    fn request_shell_eject(root: &Path) -> Result<EjectOutcome, String> {
        let root = root.to_string_lossy();
        let drive = root.trim_end_matches('\\');
        let script = format!(
            r#"
$ErrorActionPreference = 'Stop'
$root = '{}'
$drive = '{}'
$shell = New-Object -ComObject Shell.Application
$item = $shell.Namespace(17).ParseName($drive)
if ($null -eq $item) {{
    $item = $shell.Namespace(17).ParseName($root)
}}
if ($null -eq $item) {{
    throw "Windows Shell could not find drive $drive for eject."
}}
$verb = $null
foreach ($candidate in $item.Verbs()) {{
    $name = $candidate.Name.Replace('&', '')
    if ($name -match 'Eject|Safely Remove|Uitwerpen|Éjecter|Auswerfen') {{
        $verb = $candidate
        break
    }}
}}
if ($null -ne $verb) {{
    $verb.DoIt()
}} else {{
    $item.InvokeVerb('Eject')
}}
Start-Sleep -Seconds 3
if (Test-Path -LiteralPath $root) {{
    throw "Windows still reports $root as mounted after the Shell eject request."
}} else {{
    Write-Output 'Shell eject completed and the drive is no longer mounted.'
}}
"#,
            powershell_single_quoted(&root),
            powershell_single_quoted(drive)
        );

        let output = run_powershell(&script, Duration::from_secs(30), "safe eject")?;
        let note = output
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("Eject request completed.")
            .to_string();
        let method = EjectMethod::Shell;

        Ok(EjectOutcome { method, note })
    }

    fn restart_explorer_if_needed(actions: &[ProcessAction], notes: &mut Vec<String>) {
        let explorer_was_handled = actions.iter().any(|action| {
            action.name.eq_ignore_ascii_case("explorer.exe")
                && (action.action == "closed" || action.action == "terminated")
        });

        if !explorer_was_handled {
            return;
        }

        match Command::new("explorer.exe")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(_) => notes.push("Explorer was relaunched after USB process cleanup.".to_string()),
            Err(error) => notes.push(format!("Explorer relaunch failed: {error}")),
        }
    }

    pub fn run_eject_helper_from_args(args: Vec<OsString>) -> Option<i32> {
        if args.len() != 4 || args.get(1).and_then(|value| value.to_str()) != Some(HELPER_ARG) {
            return None;
        }

        let Some(root) = args.get(2).map(PathBuf::from) else {
            return Some(2);
        };
        let Some(output_path) = args.get(3).map(PathBuf::from) else {
            return Some(2);
        };

        let drive = helper_drive_info(&root);
        let result = safe_eject_impl(drive, false);
        let encoded = match result {
            Ok(summary) => encode_summary(&summary),
            Err(error) => format!("ERROR\t{}\n", encode_field(&error)),
        };

        match fs::write(output_path, encoded) {
            Ok(()) => Some(0),
            Err(_) => Some(1),
        }
    }

    fn run_elevated_helper(drive: &DriveInfo, root: &Path) -> Result<UsbEjectSummary, String> {
        let exe = std::env::current_exe()
            .map_err(|error| format!("Could not locate current executable: {error}"))?;
        let helper_root = root.to_string_lossy().trim_end_matches('\\').to_string();
        let result_path = std::env::temp_dir().join(format!(
            "printltools-usb-eject-{}-{}.txt",
            std::process::id(),
            timestamp_nanos()
        ));

        let script = format!(
            r#"
$ErrorActionPreference = 'Stop'
$exe = '{}'
$root = '{}'
$out = '{}'
$args = @('--usb-eject-helper', $root, $out)
$process = Start-Process -FilePath $exe -ArgumentList $args -Verb RunAs -Wait -PassThru
if (-not (Test-Path -LiteralPath $out)) {{
    if ($process.ExitCode -ne 0 -and $null -ne $process.ExitCode) {{
        throw "Elevated helper exited with code $($process.ExitCode) and did not create its result file."
    }}
    throw "Elevated helper did not create its result file."
}}
"#,
            powershell_single_quoted(&exe.to_string_lossy()),
            powershell_single_quoted(&helper_root),
            powershell_single_quoted(&result_path.to_string_lossy())
        );

        run_powershell(&script, Duration::from_secs(240), "USB elevation")?;
        let encoded = fs::read_to_string(&result_path)
            .map_err(|error| format!("Could not read elevated helper result: {error}"))?;
        let _ = fs::remove_file(&result_path);

        let mut summary = decode_summary(&encoded, drive.clone())?;
        summary.notes.insert(
            0,
            "USB operation was completed by an elevated helper.".to_string(),
        );

        Ok(summary)
    }

    fn helper_drive_info(root: &Path) -> DriveInfo {
        let root = normalize_drive_root(root).unwrap_or_else(|_| root.to_path_buf());
        let letter = root
            .to_string_lossy()
            .chars()
            .next()
            .map(|letter| format!("{}:", letter.to_ascii_uppercase()))
            .unwrap_or_else(|| "?".to_string());

        DriveInfo {
            root,
            letter,
            label: String::new(),
            drive_type: DriveKind::Removable,
            total_bytes: None,
            free_bytes: None,
        }
    }

    fn timestamp_nanos() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    }

    fn encode_summary(summary: &UsbEjectSummary) -> String {
        let mut output = String::new();

        output.push_str(&format!("EJECTED\t{}\n", summary.ejected));
        output.push_str(&format!("ADMIN\t{}\n", summary.admin_required));
        output.push_str(&format!(
            "METHOD\t{}\n",
            encode_eject_method(summary.eject_method)
        ));
        output.push_str(&format!("RESOURCES\t{}\n", summary.resources_registered));
        output.push_str(&format!("LIMITED\t{}\n", summary.resource_scan_limited));

        for action in &summary.restart_manager_processes {
            output.push_str(&encode_action("RM", action));
        }
        for action in &summary.process_scan_actions {
            output.push_str(&encode_action("PS", action));
        }
        for note in &summary.notes {
            output.push_str(&format!("NOTE\t{}\n", encode_field(note)));
        }

        output
    }

    fn decode_summary(encoded: &str, drive: DriveInfo) -> Result<UsbEjectSummary, String> {
        let mut summary = UsbEjectSummary {
            drive,
            resources_registered: 0,
            resource_scan_limited: false,
            restart_manager_processes: Vec::new(),
            process_scan_actions: Vec::new(),
            notes: Vec::new(),
            ejected: false,
            admin_required: false,
            eject_method: EjectMethod::None,
        };

        for line in encoded.lines() {
            let mut parts = line.split('\t');
            match parts.next().unwrap_or("") {
                "ERROR" => {
                    return Err(decode_field(
                        parts.next().unwrap_or("Elevated helper failed."),
                    ));
                }
                "EJECTED" => summary.ejected = parts.next() == Some("true"),
                "ADMIN" => summary.admin_required = parts.next() == Some("true"),
                "METHOD" => {
                    summary.eject_method = decode_eject_method(parts.next().unwrap_or("none"))
                }
                "RESOURCES" => {
                    summary.resources_registered =
                        parts.next().unwrap_or("0").parse().unwrap_or_default()
                }
                "LIMITED" => summary.resource_scan_limited = parts.next() == Some("true"),
                "RM" => {
                    if let Some(action) = decode_action(parts) {
                        summary.restart_manager_processes.push(action);
                    }
                }
                "PS" => {
                    if let Some(action) = decode_action(parts) {
                        summary.process_scan_actions.push(action);
                    }
                }
                "NOTE" => summary.notes.push(decode_field(parts.next().unwrap_or(""))),
                _ => {}
            }
        }

        Ok(summary)
    }

    fn encode_action(kind: &str, action: &ProcessAction) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}\n",
            kind,
            action.pid,
            encode_field(&action.name),
            encode_field(action.path.as_deref().unwrap_or("")),
            encode_field(&action.action)
        )
    }

    fn decode_action<'a>(mut parts: impl Iterator<Item = &'a str>) -> Option<ProcessAction> {
        Some(ProcessAction {
            pid: parts.next()?.parse().ok()?,
            name: decode_field(parts.next().unwrap_or("")),
            path: parts
                .next()
                .map(decode_field)
                .filter(|path| !path.is_empty()),
            action: decode_field(parts.next().unwrap_or("")),
        })
    }

    fn encode_eject_method(method: EjectMethod) -> &'static str {
        match method {
            EjectMethod::None => "none",
            EjectMethod::Shell => "shell",
            EjectMethod::Mountvol => "mountvol",
        }
    }

    fn decode_eject_method(value: &str) -> EjectMethod {
        match value {
            "shell" => EjectMethod::Shell,
            "mountvol" => EjectMethod::Mountvol,
            _ => EjectMethod::None,
        }
    }

    fn encode_field(value: &str) -> String {
        value
            .replace('%', "%25")
            .replace('\t', "%09")
            .replace('\r', "%0D")
            .replace('\n', "%0A")
    }

    fn decode_field(value: &str) -> String {
        value
            .replace("%0A", "\n")
            .replace("%0D", "\r")
            .replace("%09", "\t")
            .replace("%25", "%")
    }

    fn parse_process_actions(output: &str) -> Vec<ProcessAction> {
        output
            .lines()
            .filter_map(|line| {
                let mut parts = line.splitn(4, '\t');
                let pid = parts.next()?.trim().parse().ok()?;
                let name = parts.next().unwrap_or("").trim().to_string();
                let path = parts
                    .next()
                    .map(str::trim)
                    .filter(|path| !path.is_empty())
                    .map(ToOwned::to_owned);
                let action = parts.next().unwrap_or("").trim().to_string();

                Some(ProcessAction {
                    pid,
                    name,
                    path,
                    action,
                })
            })
            .collect()
    }

    fn run_powershell(script: &str, timeout: Duration, label: &str) -> Result<String, String> {
        let mut command = Command::new("powershell.exe");
        command
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-Command")
            .arg(script);

        run_command_with_timeout(&mut command, timeout, label)
    }

    fn run_command_with_timeout(
        command: &mut Command,
        timeout: Duration,
        label: &str,
    ) -> Result<String, String> {
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("Could not start {label}: {error}"))?;

        let started = Instant::now();

        loop {
            match child.try_wait() {
                Ok(Some(_status)) => {
                    let output = child
                        .wait_with_output()
                        .map_err(|error| format!("Could not read {label} output: {error}"))?;

                    if output.status.success() {
                        return String::from_utf8(output.stdout)
                            .map_err(|error| format!("{label} output was not UTF-8: {error}"));
                    }

                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let message = [stderr.trim(), stdout.trim()]
                        .into_iter()
                        .filter(|part| !part.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n");

                    return Err(if message.is_empty() {
                        format!("{label} exited with status {}.", output.status)
                    } else {
                        message
                    });
                }
                Ok(None) => {
                    if started.elapsed() >= timeout {
                        kill_process_tree(&mut child);
                        let _ = child.wait();
                        return Err(format!(
                            "{label} timed out after {} seconds.",
                            timeout.as_secs()
                        ));
                    }

                    thread::sleep(Duration::from_millis(100));
                }
                Err(error) => {
                    kill_process_tree(&mut child);
                    let _ = child.wait();
                    return Err(format!("Could not monitor {label}: {error}"));
                }
            }
        }
    }

    fn kill_process_tree(child: &mut Child) {
        let pid = child.id().to_string();
        let _ = Command::new("taskkill")
            .arg("/PID")
            .arg(pid)
            .arg("/T")
            .arg("/F")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        let _ = child.kill();
    }

    fn powershell_single_quoted(value: &str) -> String {
        value.replace('\'', "''")
    }

    fn wide_null(value: &str) -> Vec<u16> {
        value.encode_utf16().chain([0]).collect()
    }

    fn wide_fixed_to_string(value: &[u16]) -> String {
        let length = value
            .iter()
            .position(|character| *character == 0)
            .unwrap_or(value.len());

        String::from_utf16_lossy(&value[..length])
    }

    #[allow(dead_code)]
    fn dedupe_process_actions(actions: Vec<ProcessAction>) -> Vec<ProcessAction> {
        let mut seen = HashMap::new();
        for action in actions {
            seen.entry(action.pid).or_insert(action);
        }

        seen.into_values().collect()
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;

    pub fn list_drives() -> Result<Vec<DriveInfo>, String> {
        Err("USB safe eject is only implemented on Windows.".to_string())
    }

    pub fn safe_eject(_drive: DriveInfo) -> Result<UsbEjectSummary, String> {
        Err("USB safe eject is only implemented on Windows.".to_string())
    }

    pub fn run_eject_helper_from_args(_args: Vec<OsString>) -> Option<i32> {
        None
    }
}
