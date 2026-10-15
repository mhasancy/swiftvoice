//! Process memory statistics (for the benchmark and diagnostics).

/// (current resident set, peak resident set) in bytes, if available.
#[cfg(windows)]
pub fn rss_bytes() -> Option<(u64, u64)> {
    use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut c: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    // SAFETY: valid pseudo-handle and a correctly sized output struct.
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
    (ok != 0).then_some((c.WorkingSetSize as u64, c.PeakWorkingSetSize as u64))
}

#[cfg(target_os = "linux")]
pub fn rss_bytes() -> Option<(u64, u64)> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let field = |name: &str| -> Option<u64> {
        status
            .lines()
            .find(|l| l.starts_with(name))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u64>().ok())
            .map(|kb| kb * 1024)
    };
    Some((field("VmRSS:")?, field("VmHWM:")?))
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn rss_bytes() -> Option<(u64, u64)> {
    None
}

/// Return freed heap memory to the operating system (after a model is
/// unloaded). The allocator otherwise keeps most of a freed model's pages.
#[cfg(windows)]
pub fn release_free_memory() {
    use windows_sys::Win32::System::Memory::{GetProcessHeap, HeapCompact};
    // SAFETY: the process heap handle is always valid; HeapCompact coalesces
    // free blocks and decommits large free ones.
    unsafe {
        HeapCompact(GetProcessHeap(), 0);
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn release_free_memory() {
    extern "C" {
        fn malloc_trim(pad: usize) -> i32;
    }
    // SAFETY: glibc's malloc_trim has no preconditions.
    unsafe {
        malloc_trim(0);
    }
}

#[cfg(not(any(windows, all(target_os = "linux", target_env = "gnu"))))]
pub fn release_free_memory() {}

pub fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn reports_memory_on_supported_platforms() {
        if cfg!(any(windows, target_os = "linux")) {
            let (cur, peak) = super::rss_bytes().expect("memory stats");
            assert!(cur > 0 && peak >= cur);
        }
    }
}
