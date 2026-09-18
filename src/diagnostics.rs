//! Local lifecycle diagnostics. Never record terminal bytes, profiles, or secrets.
use std::{
    fs::{self, File},
    io::{Seek, SeekFrom, Write},
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_BYTES: u64 = 1024 * 1024;
static LOG: OnceLock<Mutex<DiagnosticLog>> = OnceLock::new();

struct DiagnosticLog {
    file: File,
    path: PathBuf,
    bytes: u64,
}

impl DiagnosticLog {
    fn write(&mut self, value: &serde_json::Value) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(value)?;
        line.push(b'\n');
        if self.bytes + line.len() as u64 > MAX_BYTES {
            self.file.set_len(0)?;
            self.file.seek(SeekFrom::Start(0))?;
            self.bytes = 0;
        }
        self.file.write_all(&line)?;
        self.file.flush()?;
        self.bytes += line.len() as u64;
        Ok(())
    }
}

/// Enabled only by the actual app entrypoint, not UI fixtures or benchmarks.
pub fn init() -> std::io::Result<()> {
    let dir = directories::ProjectDirs::from("app", "relay", "Relay")
        .ok_or_else(|| std::io::Error::other("Cannot locate diagnostics directory"))?
        .data_local_dir()
        .join("diagnostics");
    init_in(dir)
}

fn init_in(dir: PathBuf) -> std::io::Result<()> {
    fs::create_dir_all(&dir)?;
    let file = tempfile::Builder::new()
        .prefix("relay-")
        .suffix(".log")
        .tempfile_in(&dir)?;
    let (file, path) = file.keep().map_err(|error| error.error)?;
    // Separate files avoid interleaved records when two Relay windows are running.
    let mut previous: Vec<_> = fs::read_dir(&dir)?
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            entry.path() != path
                && name.starts_with("relay-")
                && name.ends_with(".log")
                && entry.file_type().is_ok_and(|kind| kind.is_file())
        })
        .collect();
    previous.sort_by_key(|entry| entry.metadata().and_then(|m| m.modified()).ok());
    let remove = previous.len().saturating_sub(7);
    for entry in previous.into_iter().take(remove) {
        let _ = fs::remove_file(entry.path());
    }
    let _ = LOG.set(Mutex::new(DiagnosticLog {
        file,
        path,
        bytes: 0,
    }));
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let trace = std::backtrace::Backtrace::force_capture().to_string();
        let trace: String = trace.chars().take(32_768).collect();
        record(
            "panic",
            serde_json::json!({
                "thread": std::thread::current().name(),
                "location": info.location().map(|p| format!("{}:{}:{}", p.file(), p.line(), p.column())),
                "backtrace": trace,
                "payload": "omitted to avoid recording terminal content or credentials"
            }),
        );
        previous_hook(info);
    }));
    record(
        "app_started",
        serde_json::json!({"version": env!("CARGO_PKG_VERSION")}),
    );
    Ok(())
}

pub fn path() -> Option<PathBuf> {
    LOG.get()?.lock().ok().map(|log| log.path.clone())
}

pub fn record(event: &'static str, details: serde_json::Value) {
    let Some(log) = LOG.get() else { return };
    // A panic while writing must not deadlock its own panic hook.
    let Ok(mut log) = log.try_lock() else { return };
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let _ = log.write(&serde_json::json!({"time_unix_ms": time, "pid": std::process::id(), "event": event, "details": details}));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "subprocess fixture invoked by panic_hook_records_context_without_payload"]
    fn panic_probe() {
        let Some(dir) = std::env::var_os("RELAY_DIAGNOSTIC_TEST_DIR") else {
            return;
        };
        init_in(dir.into()).unwrap();
        let worker = crate::files::Worker::new(eframe::egui::Context::default());
        worker
            .sender
            .send(crate::files::Request::List(
                "/synthetic-secret-directory".into(),
            ))
            .unwrap();
        assert!(matches!(
            worker
                .receiver
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
            crate::files::Event::ListFailed { .. }
        ));
        std::thread::Builder::new()
            .name("diagnostic-worker".into())
            .spawn(|| {
                panic!("synthetic-secret-must-not-enter-the-log");
            })
            .unwrap()
            .join()
            .expect_err("fixture worker must panic");
    }

    #[test]
    fn panic_hook_records_context_without_payload() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..10 {
            fs::write(dir.path().join(format!("relay-old-{i}.log")), b"old").unwrap();
        }
        fs::write(dir.path().join("unrelated.txt"), b"keep").unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "diagnostics::tests::panic_probe"])
            .env("RELAY_DIAGNOSTIC_TEST_DIR", dir.path())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stdout)
        );
        let files: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "log"))
            .collect();
        assert_eq!(files.len(), 8);
        assert!(dir.path().join("unrelated.txt").exists());
        let text = files
            .iter()
            .map(|entry| fs::read_to_string(entry.path()).unwrap())
            .find(|text| text.contains("app_started"))
            .unwrap();
        assert!(!text.contains("synthetic-secret"));
        let records: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let panic = records
            .iter()
            .find(|value| value["event"] == "panic")
            .unwrap();
        let failure = records
            .iter()
            .find(|value| value["event"] == "sftp_request_failed")
            .unwrap();
        assert_eq!(failure["details"]["category"], "connection");
        assert_eq!(panic["details"]["thread"], "diagnostic-worker");
        assert!(
            panic["details"]["location"]
                .as_str()
                .unwrap()
                .contains("diagnostics.rs")
        );
        assert!(!panic["details"]["backtrace"].as_str().unwrap().is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let new_log = files
                .iter()
                .find(|entry| {
                    fs::read_to_string(entry.path())
                        .unwrap()
                        .contains("app_started")
                })
                .unwrap();
            assert_eq!(
                new_log.metadata().unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn records_are_json_lines_and_storage_is_bounded() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut log = DiagnosticLog {
            file: file.reopen().unwrap(),
            path: file.path().into(),
            bytes: 0,
        };
        let event =
            serde_json::json!({"event": "terminal_process_exited", "tab_id": 2, "code": 255});
        log.write(&event).unwrap();
        let text = fs::read_to_string(file.path()).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(text.trim()).unwrap(),
            event
        );
        log.file.set_len(MAX_BYTES).unwrap();
        log.bytes = MAX_BYTES;
        log.write(&event).unwrap();
        assert_eq!(fs::read_to_string(file.path()).unwrap(), text);
        assert!(log.file.metadata().unwrap().len() < MAX_BYTES);
    }
}
