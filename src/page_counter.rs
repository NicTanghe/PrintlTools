use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use flate2::read::DeflateDecoder;
use lopdf::Document;

#[derive(Debug, Clone)]
pub struct PageCounterOptions {
    pub folder: PathBuf,
    pub include_subfolders: bool,
    pub powerpoint_slides_per_page: u32,
}

#[derive(Debug, Clone)]
pub struct PageCountSummary {
    pub folder: PathBuf,
    pub include_subfolders: bool,
    pub powerpoint_slides_per_page: u32,
    pub total_pages: usize,
    pub counted_files: usize,
    pub pdf_files: usize,
    pub document_files: usize,
    pub word_counted_files: usize,
    pub libreoffice_fallback_files: usize,
    pub skipped_files: Vec<FileNote>,
    pub failed_files: Vec<FileNote>,
}

#[derive(Debug, Clone)]
pub struct FileNote {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileKind {
    Pdf,
    PowerPoint,
    Document,
    Spreadsheet,
    Unsupported,
}

pub fn count_folder(options: &PageCounterOptions) -> Result<PageCountSummary, String> {
    let mut paths = Vec::new();
    collect_files(&options.folder, options.include_subfolders, &mut paths)?;

    let mut summary = PageCountSummary {
        folder: options.folder.clone(),
        include_subfolders: options.include_subfolders,
        powerpoint_slides_per_page: options.powerpoint_slides_per_page,
        total_pages: 0,
        counted_files: 0,
        pdf_files: 0,
        document_files: 0,
        word_counted_files: 0,
        libreoffice_fallback_files: 0,
        skipped_files: Vec::new(),
        failed_files: Vec::new(),
    };

    for path in paths {
        if is_office_lock_file(&path) {
            continue;
        }

        match classify_file(&path) {
            FileKind::Pdf => {
                summary.pdf_files += 1;

                match count_pdf_pages(&path) {
                    Ok(page_count) => {
                        summary.total_pages += page_count;
                        summary.counted_files += 1;
                    }
                    Err(reason) => summary.failed_files.push(FileNote { path, reason }),
                }
            }
            FileKind::PowerPoint => {
                summary.skipped_files.push(FileNote {
                    path,
                    reason: "PowerPoint slide counting starts in Epic C5.".to_string(),
                });
            }
            FileKind::Document => {
                summary.document_files += 1;

                match count_document_pages(&path) {
                    Ok(document_count) => {
                        summary.total_pages += document_count.pages;
                        summary.counted_files += 1;

                        match document_count.backend {
                            DocumentBackend::MicrosoftWord => {
                                summary.word_counted_files += 1;
                            }
                            DocumentBackend::LibreOfficeFallback => {
                                summary.libreoffice_fallback_files += 1;
                            }
                        }
                    }
                    Err(reason) => summary.failed_files.push(FileNote { path, reason }),
                }
            }
            FileKind::Spreadsheet => {
                summary.skipped_files.push(FileNote {
                    path,
                    reason: "Excel and spreadsheet files are explicitly unsupported.".to_string(),
                });
            }
            FileKind::Unsupported => {}
        }
    }

    Ok(summary)
}

fn collect_files(
    folder: &Path,
    include_subfolders: bool,
    output: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let entries = fs::read_dir(folder)
        .map_err(|error| format!("Could not read folder {}: {error}", folder.display()))?;

    for entry in entries {
        let entry = entry
            .map_err(|error| format!("Could not read an entry in {}: {error}", folder.display()))?;
        let path = entry.path();

        if path.is_dir() {
            if include_subfolders {
                collect_files(&path, true, output)?;
            }
        } else if path.is_file() {
            output.push(path);
        }
    }

    Ok(())
}

fn count_pdf_pages(path: &Path) -> Result<usize, String> {
    let document = Document::load(path).map_err(|error| format!("Could not read PDF: {error}"))?;

    if document.is_encrypted() {
        return Err(
            "Encrypted PDFs require password handling, which is still an open spec question."
                .to_string(),
        );
    }

    Ok(document.get_pages().len())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocumentBackend {
    MicrosoftWord,
    LibreOfficeFallback,
}

#[derive(Debug, Clone, Copy)]
struct DocumentPageCount {
    pages: usize,
    backend: DocumentBackend,
}

fn count_document_pages(path: &Path) -> Result<DocumentPageCount, String> {
    let local_copy = LocalDocumentCopy::new(path)?;
    let path = local_copy.path();
    let docx_metadata_estimate = if is_docx_package(path) {
        count_docx_pages_from_metadata(path).ok()
    } else {
        None
    };

    #[cfg(windows)]
    {
        match count_document_pages_with_libreoffice(path) {
            Ok(pages) => Ok(DocumentPageCount {
                pages,
                backend: DocumentBackend::LibreOfficeFallback,
            }),
            Err(libreoffice_error) => match count_document_pages_with_word(path) {
                Ok(pages) => Ok(DocumentPageCount {
                    pages,
                    backend: DocumentBackend::MicrosoftWord,
                }),
                Err(word_error) => Err(format_document_count_errors(
                    docx_metadata_estimate,
                    Some(&libreoffice_error),
                    Some(&word_error),
                )),
            },
        }
    }

    #[cfg(not(windows))]
    {
        match count_document_pages_with_libreoffice(path) {
            Ok(pages) => Ok(DocumentPageCount {
                pages,
                backend: DocumentBackend::LibreOfficeFallback,
            }),
            Err(libreoffice_error) => Err(format_document_count_errors(
                docx_metadata_estimate,
                Some(&libreoffice_error),
                None,
            )),
        }
    }
}

fn is_docx_package(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "docx" | "docm"))
}

struct LocalDocumentCopy {
    workspace: TempDir,
    path: PathBuf,
}

impl LocalDocumentCopy {
    fn new(source: &Path) -> Result<Self, String> {
        let source = source
            .canonicalize()
            .map_err(|error| format!("Could not resolve document path: {error}"))?;
        let workspace = TempDir::new("printltools-document-copy")?;
        let extension = source
            .extension()
            .and_then(|extension| extension.to_str())
            .filter(|extension| !extension.is_empty())
            .unwrap_or("doc");
        let path = workspace.path.join(format!("document.{extension}"));

        fs::copy(&source, &path).map_err(|error| {
            format!(
                "Could not copy document {} to a local temporary file: {error}",
                source.display()
            )
        })?;

        Ok(Self { workspace, path })
    }

    fn path(&self) -> &Path {
        let _keep_alive = &self.workspace;
        &self.path
    }
}

fn format_document_count_errors(
    docx_metadata_estimate: Option<usize>,
    libreoffice_error: Option<&str>,
    word_error: Option<&str>,
) -> String {
    let mut errors = Vec::new();

    if let Some(pages) = docx_metadata_estimate {
        errors.push(format!(
            "DOCX saved metadata says {pages} pages, but it was not used because that value can be stale."
        ));
    }
    if let Some(error) = libreoffice_error {
        errors.push(format!("LibreOffice failed: {error}"));
    }
    if let Some(error) = word_error {
        errors.push(format!("Microsoft Word fallback failed: {error}"));
    }

    errors.join("\n")
}

fn count_docx_pages_from_metadata(path: &Path) -> Result<usize, String> {
    let bytes = fs::read(path).map_err(|error| format!("Could not read DOCX package: {error}"))?;
    let app_xml = read_zip_entry(&bytes, "docProps/app.xml")?;
    let app_xml = String::from_utf8(app_xml)
        .map_err(|error| format!("docProps/app.xml is not valid UTF-8: {error}"))?;
    let pages = xml_local_element_text(&app_xml, "Pages")
        .ok_or_else(|| "docProps/app.xml does not contain Pages metadata.".to_string())?;
    let pages = pages
        .trim()
        .parse::<usize>()
        .map_err(|error| format!("DOCX Pages metadata is not a valid number: {error}"))?;

    if pages == 0 {
        return Err("DOCX Pages metadata is zero, so it is probably stale.".to_string());
    }

    Ok(pages)
}

fn read_zip_entry(zip: &[u8], expected_name: &str) -> Result<Vec<u8>, String> {
    let eocd = find_end_of_central_directory(zip)?;
    let central_directory_size = read_u32(zip, eocd + 12)? as usize;
    let central_directory_offset = read_u32(zip, eocd + 16)? as usize;
    let central_directory_end = central_directory_offset
        .checked_add(central_directory_size)
        .ok_or_else(|| "DOCX ZIP central directory size overflowed.".to_string())?;

    if central_directory_end > zip.len() {
        return Err("DOCX ZIP central directory points past the end of the file.".to_string());
    }

    let mut cursor = central_directory_offset;

    while cursor < central_directory_end {
        if read_u32(zip, cursor)? != 0x0201_4b50 {
            return Err("DOCX ZIP central directory is invalid.".to_string());
        }

        let general_purpose_flags = read_u16(zip, cursor + 8)?;
        let compression_method = read_u16(zip, cursor + 10)?;
        let compressed_size = read_u32(zip, cursor + 20)? as usize;
        let file_name_length = read_u16(zip, cursor + 28)? as usize;
        let extra_field_length = read_u16(zip, cursor + 30)? as usize;
        let file_comment_length = read_u16(zip, cursor + 32)? as usize;
        let local_header_offset = read_u32(zip, cursor + 42)? as usize;
        let name_start = cursor + 46;
        let name_end = name_start
            .checked_add(file_name_length)
            .ok_or_else(|| "DOCX ZIP file name length overflowed.".to_string())?;

        if name_end > zip.len() {
            return Err("DOCX ZIP file name points past the end of the file.".to_string());
        }

        let name = String::from_utf8_lossy(&zip[name_start..name_end]);

        if name.eq_ignore_ascii_case(expected_name) {
            if general_purpose_flags & 0x1 != 0 {
                return Err("DOCX package entry is encrypted.".to_string());
            }

            return read_zip_local_entry(
                zip,
                local_header_offset,
                compressed_size,
                compression_method,
            );
        }

        cursor = name_end
            .checked_add(extra_field_length)
            .and_then(|value| value.checked_add(file_comment_length))
            .ok_or_else(|| "DOCX ZIP central directory entry length overflowed.".to_string())?;
    }

    Err(format!("DOCX package does not contain {expected_name}."))
}

fn read_zip_local_entry(
    zip: &[u8],
    local_header_offset: usize,
    compressed_size: usize,
    compression_method: u16,
) -> Result<Vec<u8>, String> {
    if read_u32(zip, local_header_offset)? != 0x0403_4b50 {
        return Err("DOCX ZIP local file header is invalid.".to_string());
    }

    let file_name_length = read_u16(zip, local_header_offset + 26)? as usize;
    let extra_field_length = read_u16(zip, local_header_offset + 28)? as usize;
    let data_start = local_header_offset
        .checked_add(30)
        .and_then(|value| value.checked_add(file_name_length))
        .and_then(|value| value.checked_add(extra_field_length))
        .ok_or_else(|| "DOCX ZIP local file header length overflowed.".to_string())?;
    let data_end = data_start
        .checked_add(compressed_size)
        .ok_or_else(|| "DOCX ZIP compressed entry length overflowed.".to_string())?;

    if data_end > zip.len() {
        return Err("DOCX ZIP entry points past the end of the file.".to_string());
    }

    match compression_method {
        0 => Ok(zip[data_start..data_end].to_vec()),
        8 => {
            let mut decoder = DeflateDecoder::new(&zip[data_start..data_end]);
            let mut output = Vec::new();
            decoder
                .read_to_end(&mut output)
                .map_err(|error| format!("Could not decompress DOCX metadata: {error}"))?;
            Ok(output)
        }
        other => Err(format!(
            "DOCX metadata uses unsupported ZIP compression method {other}."
        )),
    }
}

fn find_end_of_central_directory(zip: &[u8]) -> Result<usize, String> {
    let minimum_length = 22;
    if zip.len() < minimum_length {
        return Err("DOCX package is too small to be a ZIP file.".to_string());
    }

    let search_start = zip.len().saturating_sub(65_557);

    for index in (search_start..=zip.len() - minimum_length).rev() {
        if zip[index..].starts_with(&[0x50, 0x4b, 0x05, 0x06]) {
            return Ok(index);
        }
    }

    Err("DOCX package is missing the ZIP end record.".to_string())
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, String> {
    let end = offset
        .checked_add(2)
        .ok_or_else(|| "DOCX ZIP offset overflowed.".to_string())?;
    let value = bytes
        .get(offset..end)
        .ok_or_else(|| "DOCX ZIP field points past the end of the file.".to_string())?;

    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| "DOCX ZIP offset overflowed.".to_string())?;
    let value = bytes
        .get(offset..end)
        .ok_or_else(|| "DOCX ZIP field points past the end of the file.".to_string())?;

    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn xml_local_element_text(xml: &str, local_name: &str) -> Option<String> {
    let mut offset = 0;

    while let Some(relative_start) = xml[offset..].find('<') {
        let start = offset + relative_start;
        let tag_start = start + 1;

        if xml[tag_start..].starts_with('/') {
            offset = tag_start + 1;
            continue;
        }

        let relative_end = xml[tag_start..].find('>')?;
        let tag_end = tag_start + relative_end;
        let raw_name = xml[tag_start..tag_end]
            .trim()
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches('/');
        let raw_name = raw_name.strip_prefix('/').unwrap_or(raw_name);
        let candidate_local_name = raw_name.rsplit(':').next().unwrap_or(raw_name);

        if candidate_local_name == local_name {
            let close_tag = format!("</{raw_name}>");
            let content_start = tag_end + 1;
            let relative_close = xml[content_start..].find(&close_tag)?;
            let content_end = content_start + relative_close;

            return Some(xml[content_start..content_end].to_string());
        }

        offset = tag_end + 1;
    }

    None
}

#[cfg(windows)]
fn count_document_pages_with_word(path: &Path) -> Result<usize, String> {
    let path = path
        .canonicalize()
        .map_err(|error| format!("Could not resolve document path: {error}"))?;
    let workspace = TempDir::new("printltools-word-export")?;
    let pdf_path = workspace.path.join("word-export.pdf");
    let escaped_path = powershell_single_quoted(&path.to_string_lossy());
    let escaped_pdf_path = powershell_single_quoted(&pdf_path.to_string_lossy());
    let script = format!(
        r#"
$ErrorActionPreference = 'Stop'
$path = '{escaped_path}'
$pdfPath = '{escaped_pdf_path}'
$word = $null
$doc = $null
try {{
    $word = New-Object -ComObject Word.Application
    $word.Visible = $false
    $word.DisplayAlerts = 0
    try {{ $word.AutomationSecurity = 3 }} catch {{ }}
    try {{ $word.Options.UpdateLinksAtOpen = $false }} catch {{ }}
    try {{ $word.Options.SaveNormalPrompt = $false }} catch {{ }}
    $documents = $word.Documents
    $openErrors = New-Object System.Collections.Generic.List[string]
    try {{
        $doc = $documents.OpenNoRepairDialog($path, $false, $true, $false)
    }} catch {{
        $openErrors.Add($_.Exception.Message)
    }}
    if ($null -eq $doc) {{
        try {{ $doc = $word.ActiveDocument }} catch {{ }}
    }}
    if ($null -eq $doc) {{
        try {{
            $doc = $documents.Open($path, $false, $true, $false)
        }} catch {{
            $openErrors.Add($_.Exception.Message)
        }}
    }}
    if ($null -eq $doc) {{
        try {{ $doc = $word.ActiveDocument }} catch {{ }}
    }}
    if ($null -eq $doc) {{
        try {{
            $doc = $documents.Open($path)
        }} catch {{
            $openErrors.Add($_.Exception.Message)
        }}
    }}
    if ($null -eq $doc) {{
        try {{ $doc = $word.ActiveDocument }} catch {{ }}
    }}
    if ($null -eq $doc) {{
        throw "Microsoft Word did not open the local temporary document copy. Attempts: $($openErrors -join ' | ')"
    }}
    try {{ $doc.Repaginate() | Out-Null }} catch {{ }}
    if (Test-Path -LiteralPath $pdfPath) {{
        Remove-Item -LiteralPath $pdfPath -Force
    }}
    $doc.ExportAsFixedFormat($pdfPath, 17)
    if (-not (Test-Path -LiteralPath $pdfPath)) {{
        throw "Microsoft Word did not create the PDF export at $pdfPath."
    }}
    Write-Output $pdfPath
}} finally {{
    if ($null -ne $doc) {{
        try {{ $doc.Close($false) | Out-Null }} catch {{ }}
        try {{ [System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($doc) | Out-Null }} catch {{ }}
    }}
    if ($null -ne $word) {{
        try {{ $word.Quit() | Out-Null }} catch {{ }}
        try {{ [System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($word) | Out-Null }} catch {{ }}
    }}
    [GC]::Collect()
    [GC]::WaitForPendingFinalizers()
}}
"#
    );

    let output = run_powershell(&script, Duration::from_secs(180))?;
    let exported_pdf = output
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| "Word did not return a PDF export path.".to_string())?;

    count_pdf_pages(Path::new(exported_pdf)).map_err(|error| {
        format!("Word exported a PDF, but the exported PDF could not be counted: {error}")
    })
}

#[cfg(windows)]
fn run_powershell(script: &str, timeout: Duration) -> Result<String, String> {
    run_command_with_timeout(
        Command::new("powershell.exe")
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-Sta")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-Command")
            .arg(script),
        timeout,
        "Word automation",
    )
}

fn count_document_pages_with_libreoffice(path: &Path) -> Result<usize, String> {
    let path = path
        .canonicalize()
        .map_err(|error| format!("Could not resolve document path: {error}"))?;
    let workspace = TempDir::new("printltools-libreoffice")?;
    let mut attempts = Vec::new();

    for candidate in libreoffice_candidates() {
        if candidate.is_absolute() && !candidate.exists() {
            attempts.push(format!("{} does not exist", candidate.display()));
            continue;
        }

        let mut command = Command::new(&candidate);
        if let Some(parent) = candidate.parent().filter(|parent| parent.exists()) {
            command.current_dir(parent);
        }
        command
            .arg("--headless")
            .arg("--convert-to")
            .arg("pdf")
            .arg("--outdir")
            .arg(&workspace.path)
            .arg(&path);

        match run_command_with_timeout(&mut command, Duration::from_secs(180), "LibreOffice") {
            Ok(_output) => {
                let pdf = find_converted_pdf(&workspace.path)?;
                return count_pdf_pages(&pdf)
                    .map_err(|error| format!("Converted PDF could not be counted: {error}"));
            }
            Err(error) => {
                attempts.push(format!("{} failed: {error}", candidate.display()));
            }
        }
    }

    Err(format!(
        "LibreOffice could not convert the document. Attempts: {}",
        attempts.join(" | ")
    ))
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
    #[cfg(windows)]
    {
        let pid = child.id().to_string();
        let _ = Command::new("taskkill")
            .arg("/PID")
            .arg(pid)
            .arg("/T")
            .arg("/F")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    let _ = child.kill();
}

fn libreoffice_candidates() -> Vec<PathBuf> {
    vec![
        PathBuf::from(r"C:\Program Files\LibreOffice\program\soffice.com"),
        PathBuf::from(r"C:\Program Files\LibreOffice\program\soffice.exe"),
        PathBuf::from(r"C:\Program Files (x86)\LibreOffice\program\soffice.com"),
        PathBuf::from(r"C:\Program Files (x86)\LibreOffice\program\soffice.exe"),
        PathBuf::from("soffice.com"),
        PathBuf::from("soffice.exe"),
    ]
}

fn find_converted_pdf(folder: &Path) -> Result<PathBuf, String> {
    let pdfs = fs::read_dir(folder)
        .map_err(|error| format!("Could not read LibreOffice output folder: {error}"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
        })
        .collect::<Vec<_>>();

    match pdfs.as_slice() {
        [pdf] => Ok(pdf.clone()),
        [] => Err("LibreOffice did not create a PDF output file.".to_string()),
        _ => Err("LibreOffice created more than one PDF output file.".to_string()),
    }
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Result<Self, String> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("System clock error: {error}"))?
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{stamp}", std::process::id()));

        fs::create_dir_all(&path).map_err(|error| {
            format!(
                "Could not create temporary folder {}: {error}",
                path.display()
            )
        })?;

        Ok(Self { path })
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = remove_dir_all_if_exists(&self.path);
    }
}

fn remove_dir_all_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn powershell_single_quoted(value: &str) -> String {
    value.replace('\'', "''")
}

fn classify_file(path: &Path) -> FileKind {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase());

    match extension.as_deref() {
        Some("pdf") => FileKind::Pdf,
        Some("ppt" | "pptx" | "pptm" | "pps" | "ppsx") => FileKind::PowerPoint,
        Some("doc" | "docx" | "docm" | "rtf" | "odt") => FileKind::Document,
        Some("xls" | "xlsx" | "xlsm" | "xlsb" | "ods" | "csv") => FileKind::Spreadsheet,
        _ => FileKind::Unsupported,
    }
}

fn is_office_lock_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("~$"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use lopdf::content::{Content, Operation};
    use lopdf::{Document, Object, Stream, dictionary};

    use super::{PageCounterOptions, count_docx_pages_from_metadata, count_folder};

    #[test]
    fn counts_pdf_pages_and_respects_subfolder_option() {
        let workspace = TestWorkspace::new();
        let nested = workspace.path.join("nested");
        fs::create_dir_all(&nested).unwrap();

        create_test_pdf(&workspace.path.join("root.pdf"), "Root");
        create_test_pdf(&nested.join("nested.pdf"), "Nested");
        fs::write(workspace.path.join("sheet.xlsx"), b"not counted").unwrap();

        let root_only = count_folder(&PageCounterOptions {
            folder: workspace.path.clone(),
            include_subfolders: false,
            powerpoint_slides_per_page: 4,
        })
        .unwrap();

        assert_eq!(root_only.total_pages, 1);
        assert_eq!(root_only.counted_files, 1);
        assert_eq!(root_only.pdf_files, 1);
        assert_eq!(root_only.skipped_files.len(), 1);

        let recursive = count_folder(&PageCounterOptions {
            folder: workspace.path.clone(),
            include_subfolders: true,
            powerpoint_slides_per_page: 4,
        })
        .unwrap();

        assert_eq!(recursive.total_pages, 2);
        assert_eq!(recursive.counted_files, 2);
        assert_eq!(recursive.pdf_files, 2);
    }

    #[test]
    fn reads_docx_pages_from_metadata_without_office() {
        let workspace = TestWorkspace::new();
        let document = workspace.path.join("document.docx");
        create_test_docx(&document, 7);

        assert_eq!(count_docx_pages_from_metadata(&document).unwrap(), 7);
    }

    struct TestWorkspace {
        path: PathBuf,
    }

    impl TestWorkspace {
        fn new() -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "printltools-page-counter-test-{}-{stamp}",
                std::process::id()
            ));

            fs::create_dir_all(&path).unwrap();

            Self { path }
        }
    }

    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn create_test_pdf(path: &Path, label: &str) {
        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Courier",
        });
        let resources_id = document.add_object(dictionary! {
            "Font" => dictionary! {
                "F1" => font_id,
            },
        });
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 24.into()]),
                Operation::new("Td", vec![100.into(), 600.into()]),
                Operation::new("Tj", vec![Object::string_literal(label)]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id =
            document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        });

        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
            }),
        );
        let catalog_id = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        document.trailer.set("Root", catalog_id);
        document.save(path).unwrap();
    }

    fn create_test_docx(path: &Path, pages: usize) {
        let app_xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties">
  <Pages>{pages}</Pages>
</Properties>"#
        );

        write_stored_zip(path, "docProps/app.xml", app_xml.as_bytes());
    }

    fn write_stored_zip(path: &Path, name: &str, data: &[u8]) {
        let mut zip = Vec::new();
        let name = name.as_bytes();

        push_u32(&mut zip, 0x0403_4b50);
        push_u16(&mut zip, 20);
        push_u16(&mut zip, 0);
        push_u16(&mut zip, 0);
        push_u16(&mut zip, 0);
        push_u16(&mut zip, 0);
        push_u32(&mut zip, 0);
        push_u32(&mut zip, data.len() as u32);
        push_u32(&mut zip, data.len() as u32);
        push_u16(&mut zip, name.len() as u16);
        push_u16(&mut zip, 0);
        zip.extend_from_slice(name);
        zip.extend_from_slice(data);

        let central_directory_offset = zip.len();
        push_u32(&mut zip, 0x0201_4b50);
        push_u16(&mut zip, 20);
        push_u16(&mut zip, 20);
        push_u16(&mut zip, 0);
        push_u16(&mut zip, 0);
        push_u16(&mut zip, 0);
        push_u16(&mut zip, 0);
        push_u32(&mut zip, 0);
        push_u32(&mut zip, data.len() as u32);
        push_u32(&mut zip, data.len() as u32);
        push_u16(&mut zip, name.len() as u16);
        push_u16(&mut zip, 0);
        push_u16(&mut zip, 0);
        push_u16(&mut zip, 0);
        push_u16(&mut zip, 0);
        push_u32(&mut zip, 0);
        push_u32(&mut zip, 0);
        zip.extend_from_slice(name);

        let central_directory_size = zip.len() - central_directory_offset;
        push_u32(&mut zip, 0x0605_4b50);
        push_u16(&mut zip, 0);
        push_u16(&mut zip, 0);
        push_u16(&mut zip, 1);
        push_u16(&mut zip, 1);
        push_u32(&mut zip, central_directory_size as u32);
        push_u32(&mut zip, central_directory_offset as u32);
        push_u16(&mut zip, 0);

        fs::write(path, zip).unwrap();
    }

    fn push_u16(output: &mut Vec<u8>, value: u16) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u32(output: &mut Vec<u8>, value: u32) {
        output.extend_from_slice(&value.to_le_bytes());
    }
}
