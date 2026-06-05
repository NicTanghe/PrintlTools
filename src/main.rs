mod app;
mod dialogs;
mod page_counter;
mod pdf;
mod registry;
mod results;
mod tray;
mod usb;

fn main() {
    if let Some(exit_code) = usb::run_eject_helper_from_args(std::env::args_os().collect()) {
        std::process::exit(exit_code);
    }

    if let Err(error) = app::run() {
        eprintln!("PrintLTools failed: {error}");
        std::process::exit(1);
    }
}
