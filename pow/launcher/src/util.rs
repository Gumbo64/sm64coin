//! Small shared helpers: paths, ports, child-process priority.
use std::path::PathBuf;
use std::process::Child;

pub const EXE: &str = if cfg!(windows) { ".exe" } else { "" };

/// The release's version: the tag CI builds (`VERSION`, e.g. v0.2.2), else Cargo's.
pub fn version() -> &'static str {
    match option_env!("VERSION") {
        Some(v) if !v.is_empty() => v.trim_start_matches('v'),
        _ => env!("CARGO_PKG_VERSION"),
    }
}

/// Paths from canonicalize() carry a `\\?\` prefix on Windows that some programs cannot read.
pub fn plain(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(r) if !r.starts_with("UNC") => PathBuf::from(r),
        _ => p,
    }
}

pub fn home() -> PathBuf {
    // Windows: the user's profile (a HOME variable only exists inside MSYS/Git shells).
    let (a, b) = if cfg!(windows) { ("USERPROFILE", "HOME") } else { ("HOME", "USERPROFILE") };
    std::env::var_os(a).or_else(|| std::env::var_os(b)).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

pub fn port_free(port: u16, host: &str) -> bool {
    std::net::TcpListener::bind((host, port)).is_ok()
}

/// A free port: `preferred` if free, else one of the next 20, else any port the
/// system hands out. `taken` are ports this launcher already chose.
pub fn pick_port(preferred: u16, host: &str, taken: &[u16]) -> u16 {
    let ok = |p: u16| p != 0 && !taken.contains(&p) && port_free(p, host);
    if let Some(p) = (0..=20u16).map(|i| preferred.saturating_add(i)).find(|&p| ok(p)) {
        return p;
    }
    for _ in 0..20 {
        if let Ok(l) = std::net::TcpListener::bind((host, 0)) {
            if let Ok(a) = l.local_addr() {
                if !taken.contains(&a.port()) {
                    return a.port();
                }
            }
        }
    }
    preferred
}

/// The miner uses every core but one; at normal priority it starved the node and the website.
/// Lowered right after spawning (its search children inherit it).
pub fn lower_priority(child: &Child) {
    #[cfg(unix)]
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, child.id() as libc::id_t, 10);
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        unsafe extern "system" {
            fn SetPriorityClass(h: *mut std::ffi::c_void, class: u32) -> i32;
        }
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x4000;
        unsafe {
            SetPriorityClass(child.as_raw_handle() as *mut _, BELOW_NORMAL_PRIORITY_CLASS);
        }
    }
}
