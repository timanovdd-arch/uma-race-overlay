use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

fn log_path() -> &'static PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
        PathBuf::from(base).join("uma_race_overlay.log")
    })
}

pub fn log(msg: &str) {
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(log_path()) {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{}] {}", t, msg);
    }
}

macro_rules! logf {
    ($($arg:tt)*) => {
        crate::logger::log(&format!($($arg)*))
    };
}
pub(crate) use logf;
