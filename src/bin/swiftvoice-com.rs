//! `swiftvoice.com`: the console front end of `swiftvoice.exe`.
//!
//! `swiftvoice.exe` is a GUI-subsystem program, so it starts from Explorer
//! or at sign-in without a console window. Terminals do not wait for such
//! programs, though: `swiftvoice --benchmark …` would return to the prompt
//! at once and print its report after it. The package therefore also
//! ships this small console-subsystem launcher as `swiftvoice.com`. Typing
//! `swiftvoice` in cmd or PowerShell runs it first, because PATHEXT lists
//! .COM before .EXE (Visual Studio's devenv.com/devenv.exe work the same
//! way).
//!
//! The launcher runs `swiftvoice.exe` with the terminal's input and output,
//! waits, and returns its exit code; Ctrl+C stops it. Without a command
//! (the tray app) it starts `swiftvoice.exe` and returns immediately.

#[cfg(windows)]
mod imp {
    use std::os::windows::io::AsRawHandle;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicIsize, Ordering};

    use windows_sys::core::BOOL;
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    use windows_sys::Win32::System::Threading::TerminateProcess;

    static CHILD: AtomicIsize = AtomicIsize::new(0);

    unsafe extern "system" fn on_ctrl(_kind: u32) -> BOOL {
        let handle = CHILD.load(Ordering::SeqCst);
        if handle != 0 {
            // SAFETY: the handle belongs to the child we are waiting for.
            unsafe { TerminateProcess(handle as _, 130) };
        }
        1
    }

    pub fn main() -> i32 {
        let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
        let exe = match std::env::current_exe() {
            Ok(p) => p.with_file_name("swiftvoice.exe"),
            Err(e) => {
                eprintln!("swiftvoice: cannot locate swiftvoice.exe: {e}");
                return 1;
            }
        };
        if !exe.is_file() {
            eprintln!("swiftvoice: {} is missing; reinstall SwiftVoice", exe.display());
            return 1;
        }
        let tray = matches!(swiftvoice::cli::parse(args.clone()).map(|i| i.command), Ok(swiftvoice::cli::Command::Tray));
        let mut cmd = Command::new(&exe);
        cmd.args(&args[1..]);
        if tray {
            return match cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
                Ok(_) => 0,
                Err(e) => {
                    eprintln!("swiftvoice: cannot start {}: {e}", exe.display());
                    1
                }
            };
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("swiftvoice: cannot start {}: {e}", exe.display());
                return 1;
            }
        };
        CHILD.store(child.as_raw_handle() as isize, Ordering::SeqCst);
        // SAFETY: registers a handler with the signature Windows expects.
        unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) };
        let code = match child.wait() {
            Ok(status) => status.code().unwrap_or(1),
            Err(e) => {
                eprintln!("swiftvoice: {e}");
                1
            }
        };
        CHILD.store(0, Ordering::SeqCst);
        code
    }
}

#[cfg(windows)]
fn main() {
    std::process::exit(imp::main());
}

/// Elsewhere there is no GUI/console split: run the command line in-process.
#[cfg(not(windows))]
fn main() {
    let started = std::time::Instant::now();
    swiftvoice::asr::runtime::disable_telemetry();
    let code = match swiftvoice::cli::parse(std::env::args_os()) {
        Ok(inv) => swiftvoice::cli::run(inv, started),
        Err(e) => {
            eprintln!("error: {e}\n\n{}", swiftvoice::cli::USAGE);
            2
        }
    };
    std::process::exit(code);
}
