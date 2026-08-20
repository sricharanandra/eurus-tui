use std::fs::OpenOptions;
use std::io::Write;
use std::sync::OnceLock;
use std::sync::Mutex;
use std::path::PathBuf;

static LOGGER: OnceLock<Mutex<DebugLogger>> = OnceLock::new();

struct DebugLogger {
    enabled: bool,
    path: Option<PathBuf>,
}

fn logger() -> &'static Mutex<DebugLogger> {
    LOGGER.get_or_init(|| {
        let enabled = std::env::var("EURUS_VOICE_DEBUG").map(|v| v == "1").unwrap_or(false);
        let path = if enabled {
            dirs::config_dir().map(|d| d.join("eurus").join("voice.log"))
        } else {
            None
        };
        Mutex::new(DebugLogger { enabled, path })
    })
}

pub fn log(msg: impl AsRef<str>) {
    let guard = logger().lock().unwrap();
    if !guard.enabled {
        return;
    }
    if let Some(path) = &guard.path {
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
            let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
            let _ = writeln!(f, "[{}] {}", ts, msg.as_ref());
        }
    }
}

macro_rules! voice_debug {
    ($($arg:tt)*) => {
        $crate::voice::debug::log(format!($($arg)*))
    };
}
pub(crate) use voice_debug;
