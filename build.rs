use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/windows/app-icon.ico");
    println!("cargo:rerun-if-changed=assets/windows/tray-icon.ico");
    println!("cargo:rerun-if-changed=assets/windows/app-icon.rc");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let resource_dir = manifest_dir.join("assets").join("windows");
    let resource_file = resource_dir.join("app-icon.rc");
    let output_file =
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("printltools-app-icon.res");

    let status = Command::new("rc.exe")
        .current_dir(&resource_dir)
        .arg("/nologo")
        .arg("/fo")
        .arg(&output_file)
        .arg(&resource_file)
        .status()
        .expect("failed to run the Windows resource compiler");

    assert!(status.success(), "Windows resource compilation failed");
    println!(
        "cargo:rustc-link-arg-bin=printltools={}",
        output_file.display()
    );
}
