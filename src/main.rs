// Release builds are GUI-subsystem applications (no console window when
// started from Explorer or at sign-in). Command-line use attaches to the
// parent console instead; debug builds keep a console for development.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::time::Instant;

fn main() {
    let started = Instant::now();
    // First, while the process is single-threaded: keep ONNX Runtime from
    // ever starting its telemetry uploader (see asr::runtime).
    swiftvoice::asr::runtime::disable_telemetry();
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    #[cfg(windows)]
    if args.len() > 1 {
        swiftvoice::platform::attach_parent_console();
    }
    let code = match swiftvoice::cli::parse(args) {
        Ok(inv) => swiftvoice::cli::run(inv, started),
        Err(e) => {
            eprintln!("error: {e}\n\n{}", swiftvoice::cli::USAGE);
            2
        }
    };
    std::process::exit(code);
}
