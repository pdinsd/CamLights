//! Diagnostics for code running inside the Frame Server service, where there is no console.
//! Lines go to `%ProgramData%\CamLights\vcam-<account>.log`: ProgramData lets every user create
//! files but not append to another account's, so the service and the app each keep their own.

use std::io::Write;
use std::sync::Mutex;

static FILE_LOCK: Mutex<()> = Mutex::new(());

pub fn write(message: &str) {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(base) = std::env::var_os("ProgramData") else { return };
    let dir = std::path::Path::new(&base).join("CamLights");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let account = std::env::var("USERNAME").unwrap_or_default();
    let account: String = account.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let path = dir.join(format!("vcam-{account}.log"));
    // Keep the log bounded: start over once it passes 1 MiB.
    let truncate = std::fs::metadata(&path).is_ok_and(|m| m.len() > 1 << 20);
    let file = std::fs::OpenOptions::new().create(true).append(!truncate).write(true).truncate(truncate).open(path);
    if let Ok(mut file) = file {
        let _ = writeln!(file, "[{} pid {}] {message}", camlights_vcam_shared::now_ms(), std::process::id());
    }
}

macro_rules! vlog {
    ($($arg:tt)*) => { $crate::log::write(&format!($($arg)*)) };
}
pub(crate) use vlog;
