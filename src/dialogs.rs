use std::path::PathBuf;
use std::thread;

use iced::futures::channel::oneshot;
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

pub async fn pick_folder_threaded(title: &'static str) -> Option<PathBuf> {
    run_on_dialog_thread(move || pick_folder(title))
        .await
        .unwrap_or(None)
}

pub async fn pick_pdf_files_threaded(title: &'static str) -> Option<Vec<PathBuf>> {
    run_on_dialog_thread(move || pick_pdf_files(title))
        .await
        .unwrap_or(None)
}

pub async fn save_pdf_file_threaded(
    title: &'static str,
    default_name: &'static str,
) -> Option<PathBuf> {
    run_on_dialog_thread(move || save_pdf_file(title, default_name))
        .await
        .unwrap_or(None)
}

async fn run_on_dialog_thread<T>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, oneshot::Canceled>
where
    T: Send + 'static,
{
    let (sender, receiver) = oneshot::channel();

    let _ = thread::Builder::new()
        .name("printltools-dialog".to_string())
        .spawn(move || {
            let _ = sender.send(f());
        });

    receiver.await
}
