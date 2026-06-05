use std::path::PathBuf;

use rfd::FileDialog;

pub fn pick_folder(title: &str) -> Option<PathBuf> {
    FileDialog::new().set_title(title).pick_folder()
}

pub fn pick_pdf_files(title: &str) -> Option<Vec<PathBuf>> {
    FileDialog::new()
        .set_title(title)
        .add_filter("PDF files", &["pdf"])
        .pick_files()
}

pub fn save_pdf_file(title: &str, default_name: &str) -> Option<PathBuf> {
    FileDialog::new()
        .set_title(title)
        .set_file_name(default_name)
        .add_filter("PDF files", &["pdf"])
        .save_file()
}
