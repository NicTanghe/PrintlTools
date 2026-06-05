PrintLTools

Windows tray launcher for printing-related tools, inspired by Microsoft PowerToys.

Primary goal

PrintLTools is a lightweight desktop utility that lives in the system tray and exposes small, focused tools for common print-shop and office workflows. The app should be fast to open, stay out of the way, and make repetitive file/print preparation tasks available from one place.

Product principles

- Tray-first: the app normally runs in the notification area, not as a full desktop window.
- Tool launcher: each feature is a separate tool with its own focused prompt flow.
- PowerToys-like: simple launcher surface, clear icons, optional settings, and predictable Windows behavior.
- cssimpler UI: app windows and launcher surfaces are built with cssimpler.
- Local-first: files are processed on the machine unless a future tool explicitly needs an external service.
- Safe by default: never delete source files, never overwrite output without a standard Windows save confirmation, and show clear results after any automatic process shutdown.

Decisions

- Folder page counter is count-only in v1. It does not send files to a printer.
- Document support should cover the common document formats where possible: `.doc`, `.docx`, `.docm`, `.rtf`, and `.odt`.
- Microsoft Office is installed and preferred for Office document/page counting.
- LibreOffice is also installed and can be used as a fallback when Microsoft Office is unsuitable.
- Folder page counting does not include subfolders by default.
- Folder page counting includes a checkbox to include subfolders for that run.
- USB safe eject automatically closes processes that are using the selected drive.
- UI shell uses cssimpler.

Open questions

- For PowerPoint counting, should "slides per page" accept only common handout values such as 1, 2, 3, 4, 6, and 9, or any number?
- Should encrypted/password-protected PDFs and Office documents be skipped with a warning, or should the app prompt for passwords?
- Should "USB drive" mean only removable USB storage volumes, or also external USB hard drives/card readers?
- Should merged PDFs preserve bookmarks, metadata, page labels, forms, and annotations where possible, or is a visual/page merge enough for v1?

Epic A - Workspace & foundations (NON-NEGOTIABLE)
A1. Desktop app baseline

Depends: -
Status: done

Create a Windows desktop app entrypoint for PrintLTools using cssimpler for the visible UI.

Acceptance

App starts on Windows.

App can run continuously without opening a large main window.

App name, version, and basic metadata are defined in one place.

cssimpler is the UI framework for app windows and launcher surfaces, sourced from the acrylic branch.

A2. Tray lifecycle

Depends: A1
Status: done

Add a system tray icon with a context menu.

Tray menu includes:

- Open launcher
- Settings
- Exit

Acceptance

App appears in the Windows notification area.

Clicking the tray icon opens the launcher or tray menu consistently.

Exit fully stops background processes owned by the app.

A3. Tool registry

Depends: A1
Status: done

Represent each utility as a registered tool with:

- Stable id
- Display name
- Icon
- Short description
- Launch action
- Availability/disabled state

Acceptance

New tools can be added without rewriting the launcher shell.

Launcher can show enabled and disabled tools.

Tool actions are isolated from each other.

Epic B - Launcher shell
B1. Launcher window

Depends: A2, A3
Status: done

Create a compact launcher window opened from the tray.

Initial tools:

- Folder page counter
- USB safe eject
- PDF joiner

Acceptance

Window opens quickly from the tray.

Each tool is represented by a clear button with icon and label.

Window can be dismissed without exiting the tray app.

B2. Standard dialog helpers

Depends: B1
Status: done

Centralize Windows-native dialogs:

- Folder selector
- File selector
- Save-file selector
- Confirmation dialog
- Error/warning dialog
- Numeric prompt

Acceptance

All initial tools use native Windows dialogs.

Dialogs are modal to the launcher/tool flow that opened them.

Canceled dialogs stop the current tool without side effects.

B3. Result dialogs

Depends: B2
Status: done

Show task results in a consistent summary dialog.

Acceptance

Successful tools show a concise completion summary.

Partial failures show what succeeded, what failed, and why.

Long file paths remain readable or copyable.

Epic C - Folder page counter
C1. Folder selection flow

Depends: B2
Status: done

User clicks the folder page counter button, then selects a folder and chooses whether to include subfolders.

Acceptance

Native folder selector opens.

Include-subfolders checkbox is available and off by default.

Canceling selection returns to launcher with no work started.

Selected folder path is shown in the result summary.

Selected recursion mode is shown in the result summary.

C2. PowerPoint slides-per-page prompt

Depends: C1
Status: done

After folder selection, prompt the user for how many PowerPoint slides should count as one printed page.

Default options should include common handout layouts:

- 1 slide per page
- 2 slides per page
- 3 slides per page
- 4 slides per page
- 6 slides per page
- 9 slides per page

Acceptance

User must choose or enter a valid positive number.

PowerPoint effective page count is `ceil(slide_count / slides_per_page)`.

Chosen value is shown in the result summary.

C3. Supported file discovery

Depends: C1
Status: done

Find supported files in the selected folder.

Initial supported categories:

- PDF files
- PowerPoint files
- Word/document files: `.doc`, `.docx`, `.docm`, `.rtf`, `.odt`

Explicitly unsupported:

- Excel/spreadsheet files

Acceptance

Unsupported files are ignored or listed separately as skipped.

Excel files are not counted.

Temporary Office lock files are ignored.

C4. PDF page counting

Depends: C3
Status: done

Count pages in PDF files.

Acceptance

Each readable PDF contributes its actual page count.

Unreadable PDFs are reported as failed without stopping the whole run.

Encrypted PDFs are handled according to the open question decision.

C5. PowerPoint slide counting

Depends: C2, C3
Status: todo

Count slides in PowerPoint files and convert them to effective printed pages using the selected slides-per-page value.

Acceptance

Each readable presentation contributes an effective page count.

Hidden slides behavior is defined before implementation.

Unreadable/corrupt presentations are reported as failed.

C6. Document page counting

Depends: C3
Status: todo

Count pages in supported document files.

Preferred backends:

- Microsoft Office first
- LibreOffice fallback

Acceptance

Each readable document contributes its printable page count.

Counting method is deterministic enough for the same machine, page size, and layout settings.

Documents that require unavailable software are reported clearly.

The summary identifies when LibreOffice fallback was used.

C7. Folder count summary

Depends: C4, C5, C6
Status: todo

Present a result summary after counting.

Summary includes:

- Total effective pages
- Number of counted files
- Count by file type
- Skipped files
- Failed files with reason

Acceptance

User can copy the summary.

Errors in one file do not discard successful counts from other files.

No source files are modified.

Epic D - USB safe eject
D1. USB drive selection

Depends: B2
Status: done

User clicks the USB safe eject button, then selects a removable drive.

Acceptance

Only eligible removable drives are shown by default.

Drive label, letter, and capacity are visible.

Canceling selection does not modify any process or device state.

D2. Locking process detection

Depends: D1
Status: done

Detect processes that are using files, folders, or handles on the selected drive.

Acceptance

App can list processes preventing safe eject.

Process name and executable path are shown where available.

Access denied cases are reported clearly.

D3. Process shutdown flow

Depends: D2
Status: done

Automatically close or terminate processes using the selected drive.

Policy for v1:

- Drive selection is the user confirmation for this tool run
- Try graceful close first
- Escalate to force terminate when graceful close does not release the drive
- Show a result summary listing affected processes

Acceptance

User sees which processes were affected.

The app does not kill unrelated processes.

Failures to close a process are shown in the result.

D4. Safe eject command

Depends: D3
Status: done

Request Windows to safely eject the selected USB drive after locks are cleared.

Acceptance

Successful eject shows a completion message.

If Windows still refuses eject, show the remaining reason when available.

The app does not fake success if the drive remains mounted.

D5. Admin/elevation handling

Depends: D2, D3
Status: todo

Define behavior when detecting or closing locks requires elevated permissions.

Acceptance

Non-admin limitations are visible to the user.

App can request elevation only for the operation that needs it.

Normal tray app does not always require administrator privileges.

Epic E - PDF joiner
E1. Multi-PDF selection

Depends: B2
Status: done

User clicks the PDF joiner button, then selects multiple PDF files.

Acceptance

Native file picker filters for PDF files.

Multiple selection is enabled.

Canceling selection returns to launcher with no output file.

E2. Merge order confirmation

Depends: E1
Status: todo

Before saving, show the selected PDFs in merge order and allow basic reordering.

Acceptance

Default order matches file picker order or sorted filename order, whichever is chosen before implementation.

User can remove accidental selections before merge.

User can proceed or cancel.

E3. Save output prompt

Depends: E1
Status: done

Ask for output filename and location using a standard save-file dialog.

Acceptance

Default extension is `.pdf`.

Existing files use the normal Windows overwrite confirmation.

Canceling save does not create an output file.

E4. PDF merge engine

Depends: E3
Status: done

Join selected PDFs into one PDF.

Acceptance

Input page order is preserved.

Original files are never modified.

Unreadable PDFs fail with a clear message.

Output is not written if merge cannot be completed safely.

E5. Merge result summary

Depends: E4
Status: done

Show completion details after merge.

Acceptance

Summary shows output path, input file count, and total page count when available.

User can open the output folder from the result dialog.

Epic F - Settings & preferences
F1. General settings

Depends: A2, B1
Status: todo

Settings surface includes:

- Start with Windows
- Open launcher on tray icon click
- Minimize to tray
- Remember last folders

Acceptance

Settings persist across launches.

Settings can be changed without restarting where practical.

F2. Tool preferences

Depends: C2, E2
Status: todo

Store per-tool defaults.

Initial defaults:

- Last PowerPoint slides-per-page value
- Folder counter recursion choice
- PDF joiner default output folder
- PDF joiner default ordering mode

Acceptance

User can reset preferences to defaults.

Stored paths handle missing folders gracefully.

Epic G - Logging & diagnostics
G1. User-visible error model

Depends: B3
Status: todo

Define consistent error messages for tool failures.

Acceptance

Errors explain what failed and what the user can do next.

Technical details can be copied for debugging.

G2. Local logs

Depends: A1
Status: todo

Write local diagnostic logs for tool runs.

Acceptance

Logs avoid storing file contents.

Logs can include file paths, counts, process names, and error codes.

User can find logs from Settings.

Epic H - Packaging & distribution
H1. Windows installer

Depends: A1, A2
Status: todo

Package the app as a Windows installer.

Acceptance

Installer adds app shortcut and uninstall entry.

Optional setting enables start with Windows.

App can be upgraded without losing settings.

H2. Code signing decision

Depends: H1
Status: todo

Decide whether releases need code signing.

Acceptance

Unsigned install behavior is documented if signing is not used.

Signed build pipeline is documented if signing is used.

Constraints

- Windows first.
- Tray resident.
- cssimpler for app windows and launcher UI.
- No Excel/spreadsheet support in the starter tool set.
- Native file/folder/save dialogs.
- Source files must not be modified by counting or merging tools.
- Folder page counter is count-only in v1 and does not print files.
- Folder page counter excludes subfolders by default, with an opt-in checkbox.
- USB eject flow auto-closes locking processes for the selected drive and shows a result summary.
- The app should remain useful as a launcher as more printing tools are added.

Suggested implementation order

A1 + A2 + A3
B1 + B2 + B3
E1 + E3 + E4 + E5
C1 + C2 + C3 + C4
C5 + C6 + C7
D1 + D2 + D3 + D4 + D5
F1 + F2
G1 + G2
H1 + H2
