//! A child process with a log file, polite stop, and (Windows) a kill-on-close job object.
use std::fs::{self, File};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Windows: put every child into a job object that kills them when this program ends for any
/// reason (console window closed, killed), so no node or wallet is left running unseen.
#[cfg(windows)]
mod job {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::JobObjects::*;

    struct Job(HANDLE);
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}
    static JOB: OnceLock<Job> = OnceLock::new();

    pub fn adopt(child: &std::process::Child) {
        let job = JOB.get_or_init(|| unsafe {
            let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(h, JobObjectExtendedLimitInformation, &info as *const _ as *const _, std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32);
            Job(h)
        });
        unsafe {
            AssignProcessToJobObject(job.0, child.as_raw_handle() as HANDLE);
        }
    }
}
pub struct Proc {
    pub name: &'static str,
    pub child: Child,
    pub log: PathBuf,
    /// Windows has no SIGTERM: this HTTP address asks the program to exit by itself first.
    pub stop_url: Option<String>,
}

impl Proc {
    pub fn spawn(name: &'static str, mut cmd: Command, log: PathBuf) -> Result<Proc, String> {
        let out = File::create(&log).map_err(|e| format!("cannot write {}: {e}", log.display()))?;
        let err = out.try_clone().map_err(|e| e.to_string())?;
        cmd.stdin(Stdio::null()).stdout(out).stderr(err);
        let child = cmd.spawn().map_err(|e| format!("cannot start {name}: {e}"))?;
        #[cfg(windows)]
        job::adopt(&child);
        Ok(Proc { name, child, log, stop_url: None })
    }
    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
    pub fn tail(&self) -> String {
        let s = fs::read_to_string(&self.log).unwrap_or_default();
        let lines: Vec<&str> = s.lines().collect();
        lines[lines.len().saturating_sub(12)..].join("\n")
    }
    /// Ask politely (SIGTERM), wait up to `secs`, then kill.
    pub fn stop(&mut self, secs: u64) {
        if !self.alive() {
            return;
        }
        println!("stopping {} ...", self.name);
        #[cfg(unix)]
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGTERM);
        }
        #[cfg(not(unix))]
        if let Some(u) = &self.stop_url {
            let _ = ureq::post(u).timeout(Duration::from_secs(5)).send_string("{}");
        } else {
            let _ = self.child.kill();
        }
        let t = Instant::now();
        while self.alive() && t.elapsed() < Duration::from_secs(secs) {
            thread::sleep(Duration::from_millis(200));
        }
        if self.alive() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}
