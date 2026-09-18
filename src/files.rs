use crate::profiles::Profile;
use crate::{
    diagnostics,
    preview::{CHUNK_BYTES, Chunk},
};
use base64::Engine;
use eframe::egui;
use ssh2::{CheckResult, KnownHostFileKind, OpenFlags, OpenType, RenameFlags, Session, Sftp};
use std::{
    collections::{BTreeSet, VecDeque},
    fs,
    io::{Read, Seek, SeekFrom, Write},
    net::{TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Debug)]
pub struct Entry {
    pub path: String,
    pub name: String,
    pub directory: bool,
    pub file: bool,
    pub size: u64,
}
pub enum Request {
    #[cfg(test)]
    DisconnectTransport(Sender<()>),
    Connect {
        profile: Profile,
        secret: Zeroizing<String>,
        trusted: Option<String>,
    },
    List(String),
    Refresh {
        root: String,
        expanded: BTreeSet<String>,
    },
    Preview {
        id: u64,
        remote: String,
        offset: u64,
    },
    ClosePreview,
    Upload {
        local: Vec<PathBuf>,
        remote: String,
    },
    Download {
        remote: String,
        local: PathBuf,
    },
}
pub enum Event {
    Connected(String),
    Disconnected(String),
    Trust(String),
    Entries(String, Vec<Entry>),
    ListFailed { path: String, error: String },
    Refreshed(Vec<(String, Result<Vec<Entry>>)>),
    Preview { id: u64, result: Result<Chunk> },
    Progress { name: String, done: u64, total: u64 },
    Done(String),
    Error(String),
}
pub struct Worker {
    pub sender: Sender<Request>,
    pub receiver: Receiver<Event>,
    pub cancel: Arc<AtomicBool>,
}
impl Worker {
    pub fn new(ctx: egui::Context) -> Self {
        let (sender, requests) = mpsc::channel();
        let (events, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let stopped = cancel.clone();
        static NEXT_WORKER: AtomicU64 = AtomicU64::new(1);
        let worker_id = NEXT_WORKER.fetch_add(1, Ordering::Relaxed);
        std::thread::Builder::new().name(format!("sftp-worker-{worker_id}")).spawn(move || {
            let mut connection: Option<Sftp> = None;
            let mut session: Option<Session> = None;
            let mut preview: Option<(String, ssh2::File)> = None;
            let mut request_id = 0_u64;
            loop {
                let request = match requests.recv_timeout(Duration::from_secs(30)) {
                    Ok(request) => request,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if let Some(active) = session.as_ref()
                            && let Err(error) = active.keepalive_send() {
                                diagnostics::record("sftp_connection_lost", serde_json::json!({"worker_id": worker_id, "cause": "keepalive", "session_code": session_error_code(&error.to_string())}));
                                let _ = events.send(Event::Disconnected(connection_lost_message(&error.to_string())));
                                ctx.request_repaint();
                                discard_connection(&mut session, &mut connection, &mut preview);
                        }
                        continue;
                    }
                };
                request_id += 1;
                let operation = match &request {
                    #[cfg(test)]
                    Request::DisconnectTransport(_) => "fixture_disconnect",
                    Request::Connect { .. } => "connect",
                    Request::List(_) => "list",
                    Request::Refresh { .. } => "refresh",
                    Request::Preview { .. } => "preview",
                    Request::ClosePreview => "close_preview",
                    Request::Upload { .. } => "upload",
                    Request::Download { .. } => "download",
                };
                let started = Instant::now();
                let failed = std::cell::Cell::new(false);
                let transport_failure = std::cell::RefCell::new(None::<String>);
                diagnostics::record("sftp_request_started", serde_json::json!({"worker_id": worker_id, "request_id": request_id, "operation": operation}));
                let report = |event: Event| {
                    let errors: Vec<&str> = match &event {
                        Event::Error(error) | Event::ListFailed { error, .. } | Event::Preview { result: Err(error), .. } => vec![error],
                        Event::Refreshed(listings) => listings.iter().filter_map(|(_, result)| result.as_ref().err().map(String::as_str)).collect(),
                        _ => vec![],
                    };
                    for error in errors {
                        failed.set(true);
                        if is_transport_failure(error) { *transport_failure.borrow_mut() = Some(error.to_owned()); }
                        diagnostics::record("sftp_request_failed", serde_json::json!({"worker_id": worker_id, "request_id": request_id, "operation": operation, "category": error_category(error), "session_code": session_error_code(error)}));
                    }
                    let _ = events.send(event);
                    ctx.request_repaint();
                };
                let result = match request {
                    #[cfg(test)]
                    Request::DisconnectTransport(ack) => {
                        // Keep handles cached to reproduce a connection dying between reads.
                        if let Some(session) = session.as_ref() {
                            let _ = session.disconnect(None, "integration test", None);
                        }
                        let _ = ack.send(());
                        Ok(())
                    }
                    Request::Connect {
                        profile,
                        secret,
                        trusted,
                    } => {
                        discard_connection(&mut session, &mut connection, &mut preview);
                        match connect(&profile, &secret, trusted.as_deref()) {
                            Ok(Connection::Ready(active_session, sftp)) => match sftp.realpath(Path::new(".")) {
                                Ok(home) => {
                                    report(Event::Connected(remote_string(&home)));
                                    connection = Some(sftp);
                                    session = Some(active_session);
                                    Ok(())
                                }
                                Err(e) => Err(e.to_string()),
                            },
                            Ok(Connection::Trust(fingerprint)) => {
                                report(Event::Trust(fingerprint));
                                Ok(())
                            }
                            Err(e) => Err(e),
                        }
                    }
                    Request::List(path) => {
                        let result = connection.as_ref().ok_or_else(|| "Connect the file browser first.".into()).and_then(|sftp| list(sftp, &path));
                        report(list_event(path, result));
                        Ok(())
                    }
                    Request::Preview { id, remote, offset } => {
                        let result = match connection.as_ref() {
                            Some(sftp) => read_preview(sftp, &mut preview, &remote, offset),
                            None => Err("Connect the file browser first.".into()),
                        };
                        if result.as_ref().is_err_and(|error| !is_transport_failure(error)) { preview = None; }
                        report(Event::Preview { id, result });
                        Ok(())
                    }
                    Request::Refresh { root, expanded } => match connection.as_ref() {
                        Some(sftp) => {
                            let mut connection_error: Option<String> = None;
                            report(Event::Refreshed(refresh_listings(
                                root,
                                &expanded,
                                |path| {
                                    if let Some(error) = &connection_error { return Err(error.clone()); }
                                    let result = list(sftp, path);
                                    if let Err(error) = &result
                                        && is_transport_failure(error) { connection_error = Some(error.clone()); }
                                    result
                                },
                            )));
                            Ok(())
                        }
                        None => Err("Connect the file browser first.".into()),
                    },
                    Request::ClosePreview => {
                        preview = None;
                        Ok(())
                    }
                    Request::Upload { local, remote } => match connection.as_ref() {
                        Some(sftp) => {
                            let result = local.iter().try_for_each(|path| {
                                upload(sftp, path, &remote, 0, &stopped, &report)
                            });
                            result
                                .map(|_| report(Event::Done(format!("Upload complete: {remote}"))))
                        }
                        None => Err("Connect the file browser first.".into()),
                    },
                    Request::Download { remote, local } => match connection.as_ref() {
                        Some(sftp) => {
                            download(sftp, &remote, &local, &stopped, &report).map(|_| {
                                report(Event::Done(format!("Downloaded to {}", local.display())))
                            })
                        }
                        None => Err("Connect the file browser first.".into()),
                    },
                };
                if let Err(error) = result {
                    report(Event::Error(error));
                }
                if let Some(error) = transport_failure.borrow().clone()
                    && session.is_some() {
                        diagnostics::record("sftp_connection_lost", serde_json::json!({"worker_id": worker_id, "request_id": request_id, "cause": "request", "session_code": session_error_code(&error)}));
                        report(Event::Disconnected(connection_lost_message(&error)));
                        discard_connection(&mut session, &mut connection, &mut preview);
                }
                diagnostics::record("sftp_request_finished", serde_json::json!({"worker_id": worker_id, "request_id": request_id, "operation": operation, "success": !failed.get(), "elapsed_ms": started.elapsed().as_millis()}));
            }
            discard_connection(&mut session, &mut connection, &mut preview);
            diagnostics::record("sftp_worker_stopped", serde_json::json!({"worker_id": worker_id}));
        }).expect("start SFTP worker");
        Self {
            sender,
            receiver,
            cancel,
        }
    }
}

fn connection_lost_message(error: &str) -> String {
    format!("The file connection was lost. Reconnect files to continue.\n{error}")
}

fn discard_connection(
    session: &mut Option<Session>,
    connection: &mut Option<Sftp>,
    preview: &mut Option<(String, ssh2::File)>,
) {
    // Closing handles on a broken socket must not add another full network timeout.
    if let Some(session) = session.as_ref() {
        session.set_timeout(500);
    }
    *preview = None;
    *connection = None;
    *session = None;
}

fn session_error_code(error: &str) -> Option<i32> {
    // Paths in listing errors precede the actual library error. SFTP status errors
    // (e.g. permission denied) do not mean the SSH transport has failed.
    if error.contains("[SFTP(") {
        return None;
    }
    error
        .rsplit_once("[Session(")?
        .1
        .split_once(")]")?
        .0
        .parse()
        .ok()
}

fn is_transport_failure(error: &str) -> bool {
    matches!(
        session_error_code(error),
        Some(-1 | -7 | -9 | -13 | -26 | -27 | -30 | -43 | -45)
    )
}

fn list_event(path: String, result: Result<Vec<Entry>>) -> Event {
    match result {
        Ok(entries) => Event::Entries(path, entries),
        Err(error) => Event::ListFailed { path, error },
    }
}

// Do not persist server-provided text: it can contain remote paths or usernames.
fn error_category(error: &str) -> &'static str {
    if matches!(session_error_code(error), Some(-9 | -30)) {
        return "timeout";
    }
    if is_transport_failure(error) {
        return "connection";
    }
    let error = error.to_ascii_lowercase();
    if error.contains("permission") || error.contains("sftp(3)") {
        "permission_denied"
    } else if error.contains("timeout") || error.contains("timed out") {
        "timeout"
    } else if error.contains("no such file") || error.contains("sftp(2)") {
        "not_found"
    } else if error.contains("socket")
        || error.contains("disconnect")
        || error.contains("connect the file")
    {
        "connection"
    } else {
        "other"
    }
}

// Follow the fresh directory listings, so removed folders are not requested again.
// Collapsed folders are read when opened, as with normal tree navigation.
fn refresh_listings(
    root: String,
    expanded: &BTreeSet<String>,
    mut read: impl FnMut(&str) -> Result<Vec<Entry>>,
) -> Vec<(String, Result<Vec<Entry>>)> {
    let mut pending = VecDeque::from([(root, 0)]);
    let mut listings = vec![];
    while let Some((path, depth)) = pending.pop_front() {
        let result = read(&path);
        if let Ok(entries) = &result
            && depth < 64
        {
            for entry in entries {
                if entry.directory && expanded.contains(&entry.path) {
                    pending.push_back((entry.path.clone(), depth + 1));
                }
            }
        }
        listings.push((path, result));
    }
    listings
}

fn read_preview(
    sftp: &Sftp,
    handle: &mut Option<(String, ssh2::File)>,
    path: &str,
    offset: u64,
) -> Result<Chunk> {
    if handle
        .as_ref()
        .is_none_or(|(open_path, _)| open_path != path)
        || offset == 0
    {
        *handle = None;
        let metadata = sftp.lstat(Path::new(path)).map_err(|e| e.to_string())?;
        if !metadata.is_file() {
            return Err("Only regular files can be previewed.".into());
        }
        let mut file = sftp.open(Path::new(path)).map_err(|e| e.to_string())?;
        if !file.stat().map_err(|e| e.to_string())?.is_file() {
            return Err("Only regular files can be previewed.".into());
        }
        *handle = Some((path.to_owned(), file));
    }
    let (_, file) = handle.as_mut().expect("preview handle opened above");
    let size = file.stat().map_err(|e| e.to_string())?.size.unwrap_or(0);
    read_range(file, offset, size)
}

fn read_range(reader: &mut (impl Read + Seek), offset: u64, size: u64) -> Result<Chunk> {
    reader
        .seek(SeekFrom::Start(offset))
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::with_capacity(CHUNK_BYTES + 3);
    reader
        .take((CHUNK_BYTES + 3) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(Chunk {
        offset,
        size,
        bytes,
    })
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

struct Resolved {
    host: String,
    user: String,
    port: u16,
    identities: Vec<PathBuf>,
    host_alias: String,
}
fn resolve(profile: &Profile) -> Result<Resolved> {
    let mut command = Command::new("ssh");
    command.arg("-G").args(profile.ssh_args()?);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command
        .output()
        .map_err(|e| format!("OpenSSH is required: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    let config = String::from_utf8_lossy(&output.stdout);
    let value = |key: &str| {
        config
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{key} ")))
            .unwrap_or("")
            .to_owned()
    };
    for key in ["proxyjump", "proxycommand"] {
        let proxy = value(key);
        if !proxy.is_empty() && proxy != "none" {
            return Err("The file browser currently needs a direct SSH connection; jump hosts are supported in terminal tabs only.".into());
        }
    }
    let home = directories::UserDirs::new()
        .ok_or("Cannot locate the home directory")?
        .home_dir()
        .to_path_buf();
    let identities = config
        .lines()
        .filter_map(|l| l.strip_prefix("identityfile "))
        .map(|p| {
            if let Some(rest) = p.strip_prefix("~/") {
                home.join(rest)
            } else {
                PathBuf::from(p)
            }
        })
        .filter(|p| p.is_file())
        .collect();
    Ok(Resolved {
        host: value("hostname"),
        user: value("user"),
        port: value("port")
            .parse()
            .map_err(|_| "Cannot resolve SSH port")?,
        identities,
        host_alias: value("hostkeyalias"),
    })
}
enum Connection {
    Ready(Session, Sftp),
    Trust(String),
}
fn connect(profile: &Profile, secret: &str, trusted: Option<&str>) -> Result<Connection> {
    let config = resolve(profile)?;
    let addresses = (config.host.as_str(), config.port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?;
    let mut tcp = None;
    for address in addresses.take(8) {
        if let Ok(stream) = TcpStream::connect_timeout(&address, Duration::from_secs(8)) {
            tcp = Some(stream);
            break;
        }
    }
    let tcp = tcp.ok_or("Could not reach the SFTP server.")?;
    tcp.set_read_timeout(Some(Duration::from_secs(15)))
        .map_err(|e| e.to_string())?;
    tcp.set_write_timeout(Some(Duration::from_secs(15)))
        .map_err(|e| e.to_string())?;
    let mut session = Session::new().map_err(|e| e.to_string())?;
    session.set_timeout(15000);
    session.set_tcp_stream(tcp);
    session.handshake().map_err(|e| e.to_string())?;
    let (key, _) = session.host_key().ok_or("Server supplied no host key")?;
    let fingerprint = format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(
            session
                .host_key_hash(ssh2::HashType::Sha256)
                .ok_or("Cannot fingerprint host key")?
        )
    );
    let mut known = session.known_hosts().map_err(|e| e.to_string())?;
    if let Some(home) = directories::UserDirs::new() {
        let path = home.home_dir().join(".ssh").join("known_hosts");
        if path.exists() {
            known
                .read_file(&path, KnownHostFileKind::OpenSSH)
                .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
        }
    }
    #[cfg(unix)]
    {
        let path = Path::new("/etc/ssh/ssh_known_hosts");
        if path.exists() {
            known
                .read_file(path, KnownHostFileKind::OpenSSH)
                .map_err(|e| e.to_string())?;
        }
    }
    let check_host = if config.host_alias.is_empty() || config.host_alias == "none" {
        &config.host
    } else {
        &config.host_alias
    };
    match known.check_port(check_host, config.port, key) {
        CheckResult::Match => {}
        CheckResult::NotFound if trusted == Some(fingerprint.as_str()) => {}
        CheckResult::NotFound => return Ok(Connection::Trust(fingerprint)),
        CheckResult::Mismatch => {
            return Err(format!(
                "Host key changed for {}. Connection blocked. Received {fingerprint}",
                config.host
            ));
        }
        CheckResult::Failure => return Err("Host key verification failed.".into()),
    }
    let _ = session.userauth_agent(&config.user);
    if !session.authenticated() {
        for key in &config.identities {
            let _ = session.userauth_pubkey_file(
                &config.user,
                None,
                key,
                if secret.is_empty() {
                    None
                } else {
                    Some(secret)
                },
            );
            if session.authenticated() {
                break;
            }
        }
    }
    if !session.authenticated() && !secret.is_empty() {
        let _ = session.userauth_password(&config.user, secret);
    }
    if !session.authenticated() {
        return Err("SFTP authentication failed. Load your key into the SSH agent, or enter a password / private-key passphrase and retry.".into());
    }
    session.set_keepalive(true, 30);
    let sftp = session.sftp().map_err(|e| e.to_string())?;
    Ok(Connection::Ready(session, sftp))
}

fn remote_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
pub fn join_remote(parent: &str, name: &str) -> Result<String> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
        return Err("Unsupported file name.".into());
    }
    Ok(format!("{}/{name}", parent.trim_end_matches('/')))
}
fn list(sftp: &Sftp, path: &str) -> Result<Vec<Entry>> {
    let mut entries: Vec<_> = sftp
        .readdir(Path::new(path))
        .map_err(|e| format!("Cannot list {path}: {e}"))?
        .into_iter()
        .map(|(path, stat)| Entry {
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into(),
            path: remote_string(&path),
            directory: stat.is_dir(),
            file: stat.is_file(),
            size: stat.size.unwrap_or(0),
        })
        .collect();
    entries.sort_by_cached_key(|entry| (!entry.directory, entry.name.to_lowercase()));
    Ok(entries)
}
fn copy_stream(
    input: &mut impl Read,
    output: &mut impl Write,
    name: &str,
    total: u64,
    cancel: &AtomicBool,
    report: &impl Fn(Event),
) -> Result<()> {
    let mut buffer = [0_u8; 65536];
    let mut done = 0;
    let mut last = Instant::now();
    report(Event::Progress {
        name: name.into(),
        done,
        total,
    });
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("Transfer cancelled.".into());
        }
        let count = input.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        output
            .write_all(&buffer[..count])
            .map_err(|e| e.to_string())?;
        done += count as u64;
        if last.elapsed() >= Duration::from_millis(100) {
            report(Event::Progress {
                name: name.into(),
                done,
                total,
            });
            last = Instant::now();
        }
    }
    output.flush().map_err(|e| e.to_string())?;
    report(Event::Progress {
        name: name.into(),
        done,
        total,
    });
    Ok(())
}
fn upload(
    sftp: &Sftp,
    local: &Path,
    remote: &str,
    depth: usize,
    cancel: &AtomicBool,
    report: &impl Fn(Event),
) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err("Transfer cancelled.".into());
    }
    if depth > 64 {
        return Err("Folder nesting exceeds 64 levels.".into());
    }
    let meta = fs::symlink_metadata(local).map_err(|e| e.to_string())?;
    if meta.is_symlink() {
        return Err(format!(
            "Symbolic links are not uploaded: {}",
            local.display()
        ));
    }
    let name = local
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Unsupported local filename")?;
    let destination = join_remote(remote, name)?;
    if meta.is_dir() {
        match sftp.lstat(Path::new(&destination)) {
            Ok(stat) if stat.is_dir() => {}
            Ok(_) => return Err(format!("Destination already exists: {destination}")),
            Err(e) if e.code() == ssh2::ErrorCode::SFTP(2) => sftp
                .mkdir(Path::new(&destination), 0o755)
                .map_err(|e| e.to_string())?,
            Err(e) => return Err(e.to_string()),
        }
        for entry in fs::read_dir(local).map_err(|e| e.to_string())? {
            upload(
                sftp,
                &entry.map_err(|e| e.to_string())?.path(),
                &destination,
                depth + 1,
                cancel,
                report,
            )?;
        }
        return Ok(());
    }
    if !meta.is_file() {
        return Err("Only regular files and folders can be uploaded.".into());
    }
    match sftp.lstat(Path::new(&destination)) {
        Ok(_) => {
            return Err(format!(
                "Destination already exists; no file was replaced: {destination}"
            ));
        }
        Err(e) if e.code() == ssh2::ErrorCode::SFTP(2) => {}
        Err(e) => return Err(e.to_string()),
    }
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let temporary = join_remote(
        remote,
        &format!(".relay-upload-{}-{suffix}", std::process::id()),
    )?;
    let mut input = fs::File::open(local).map_err(|e| e.to_string())?;
    let mut output = sftp
        .open_mode(
            Path::new(&temporary),
            OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUSIVE,
            0o600,
            OpenType::File,
        )
        .map_err(|e| e.to_string())?;
    let result = copy_stream(
        &mut input,
        &mut output,
        &destination,
        meta.len(),
        cancel,
        report,
    )
    .and_then(|_| output.close().map_err(|e| e.to_string()))
    .and_then(|_| {
        sftp.rename(
            Path::new(&temporary),
            Path::new(&destination),
            Some(RenameFlags::empty()),
        )
        .map_err(|e| e.to_string())
    });
    if result.is_err() {
        drop(output);
        let _ = sftp.unlink(Path::new(&temporary));
    }
    result
}
fn download(
    sftp: &Sftp,
    remote: &str,
    local: &Path,
    cancel: &AtomicBool,
    report: &impl Fn(Event),
) -> Result<()> {
    let meta = sftp.lstat(Path::new(remote)).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("Select a regular file to download.".into());
    }
    if local.exists() {
        return Err(format!(
            "Choose a new destination; {} already exists.",
            local.display()
        ));
    }
    let mut output =
        tempfile::NamedTempFile::new_in(local.parent().ok_or("Missing download directory")?)
            .map_err(|e| e.to_string())?;
    let mut input = sftp.open(Path::new(remote)).map_err(|e| e.to_string())?;
    copy_stream(
        &mut input,
        &mut output,
        remote,
        meta.size.unwrap_or(0),
        cancel,
        report,
    )?;
    output.as_file().sync_all().map_err(|e| e.to_string())?;
    output.persist_noclobber(local).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_failures_are_distinct_from_file_and_permission_errors() {
        for code in [-1, -7, -9, -13, -26, -27, -30, -43, -45] {
            let error = format!("Cannot list /folder: [Session({code})] error");
            assert_eq!(session_error_code(&error), Some(code));
            assert!(is_transport_failure(&error));
        }
        assert_eq!(
            error_category("[Session(-7)] Unable to send STAT/LSTAT/SETSTAT command"),
            "connection"
        );
        assert_eq!(
            error_category("[Session(-43)] Unable to send FXP_OP"),
            "connection"
        );
        assert!(!is_transport_failure("[SFTP(3)] permission denied"));
        assert!(!is_transport_failure(
            "Cannot list /[Session(-7)]: [SFTP(2)] no such file"
        ));
        assert!(!is_transport_failure("[Session(-31)] SFTP protocol error"));
        assert!(!is_transport_failure(
            "Only regular files can be previewed."
        ));
    }

    #[test]
    fn listing_failures_keep_the_directory_and_have_private_log_categories() {
        assert!(
            matches!(list_event("/private/folder".into(), Err("Permission denied".into())), Event::ListFailed { path, error } if path == "/private/folder" && error == "Permission denied")
        );
        assert_eq!(
            error_category("Permission denied for /secret/path"),
            "permission_denied"
        );
        assert_eq!(
            error_category("Failed waiting on socket: timed out"),
            "timeout"
        );
        assert_eq!(error_category("No such file /secret/path"), "not_found");
        let worker = Worker::new(egui::Context::default());
        worker
            .sender
            .send(Request::List("/private/folder".into()))
            .unwrap();
        assert!(
            matches!(worker.receiver.recv_timeout(Duration::from_secs(2)).unwrap(), Event::ListFailed { path, .. } if path == "/private/folder")
        );
    }
    #[test]
    fn refresh_follows_fresh_expanded_folders_and_continues_after_errors() {
        let expanded = [
            "/root/open",
            "/root/gone",
            "/root/denied",
            "/root/closed/nested",
        ]
        .map(String::from)
        .into_iter()
        .collect();
        let folder = |path: &str| Entry {
            path: path.into(),
            name: path.rsplit('/').next().unwrap().into(),
            directory: true,
            file: false,
            size: 0,
        };
        let mut calls = vec![];
        let listings = refresh_listings("/root".into(), &expanded, |path| {
            calls.push(path.to_owned());
            match path {
                "/root" => Ok(vec![
                    folder("/root/denied"),
                    folder("/root/open"),
                    folder("/root/closed"),
                ]),
                "/root/denied" => Err("Permission denied".into()),
                "/root/open" => Ok(vec![]),
                _ => panic!("Must not request deleted or collapsed folders: {path}"),
            }
        });
        assert_eq!(calls, ["/root", "/root/denied", "/root/open"]);
        assert!(listings[1].1.is_err());
        assert!(listings[2].1.is_ok());
    }

    #[test]
    fn preview_reads_only_the_requested_range() {
        struct Counted {
            data: std::io::Cursor<Vec<u8>>,
            read: usize,
        }
        impl Read for Counted {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                let n = self.data.read(bytes)?;
                self.read += n;
                Ok(n)
            }
        }
        impl Seek for Counted {
            fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
                self.data.seek(pos)
            }
        }
        let data: Vec<u8> = (0..CHUNK_BYTES * 20).map(|n| (n % 251) as u8).collect();
        let mut input = Counted {
            data: std::io::Cursor::new(data.clone()),
            read: 0,
        };
        let offset = CHUNK_BYTES as u64 * 8;
        let chunk = read_range(&mut input, offset, data.len() as u64).unwrap();
        assert_eq!(input.read, CHUNK_BYTES + 3);
        assert_eq!(
            chunk.bytes,
            data[offset as usize..offset as usize + CHUNK_BYTES + 3]
        );
        assert_eq!(chunk.offset, offset);
        assert!(chunk.has_next());
        let last = read_range(&mut input, data.len() as u64 - 10, data.len() as u64).unwrap();
        assert_eq!(last.bytes.len(), 10);
        assert!(!last.has_next());
    }

    #[test]
    fn remote_paths_are_posix_and_cannot_escape_destination() {
        assert_eq!(
            join_remote("/home/test", "a b.txt").unwrap(),
            "/home/test/a b.txt"
        );
        assert_eq!(join_remote("/", "test").unwrap(), "/test");
        for name in ["..", ".", "a/b", "a\\b", "\0"] {
            assert!(join_remote("/upload", name).is_err());
        }
    }
    #[test]
    fn streams_binary_data_and_cancels_before_write() {
        let data = vec![0xAB; 200_000];
        let mut output = Vec::new();
        copy_stream(
            &mut data.as_slice(),
            &mut output,
            "test",
            data.len() as u64,
            &AtomicBool::new(false),
            &|_| {},
        )
        .unwrap();
        assert_eq!(output, data);
        let mut output = Vec::new();
        assert!(
            copy_stream(
                &mut data.as_slice(),
                &mut output,
                "test",
                0,
                &AtomicBool::new(true),
                &|_| {}
            )
            .is_err()
        );
        assert!(output.is_empty());
    }

    #[test]
    #[ignore = "Requires the disposable localhost SSH fixture; see the test script."]
    fn real_sftp_roundtrip_tree_overwrite_and_cancellation() {
        let key = std::env::var("RELAY_TEST_KEY").expect("RELAY_TEST_KEY");
        let port = std::env::var("RELAY_TEST_PORT").expect("RELAY_TEST_PORT");
        let user = std::env::var("RELAY_TEST_USER").expect("RELAY_TEST_USER");
        let profile = Profile {
            host: "127.0.0.1".into(),
            port,
            user,
            identity: key,
            ..Default::default()
        };
        let trust = match connect(&profile, "", None).unwrap() {
            Connection::Trust(fingerprint) => fingerprint,
            Connection::Ready(_, _) => panic!("Use a fixture port absent from known hosts"),
        };
        assert!(matches!(
            connect(&profile, "", Some("incorrect fingerprint")).unwrap(),
            Connection::Trust(_)
        ));
        let Connection::Ready(_session, sftp) = connect(&profile, "", Some(&trust)).unwrap() else {
            panic!("Trust was not honored")
        };
        let remote = tempfile::tempdir().unwrap();
        let worker = Worker::new(egui::Context::default());
        worker
            .sender
            .send(Request::Connect {
                profile: profile.clone(),
                secret: Zeroizing::new(String::new()),
                trusted: Some(trust.clone()),
            })
            .unwrap();
        assert!(matches!(
            worker
                .receiver
                .recv_timeout(Duration::from_secs(10))
                .unwrap(),
            Event::Connected(_)
        ));
        let missing = remote_string(&remote.path().join("missing-folder"));
        worker.sender.send(Request::List(missing.clone())).unwrap();
        assert!(
            matches!(worker.receiver.recv_timeout(Duration::from_secs(10)).unwrap(), Event::ListFailed { path, .. } if path == missing)
        );
        worker
            .sender
            .send(Request::List(remote_string(remote.path())))
            .unwrap();
        assert!(
            matches!(
                worker
                    .receiver
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap(),
                Event::Entries(_, _)
            ),
            "listing can succeed after an earlier failure"
        );
        let local = tempfile::tempdir().unwrap();
        let preview_path = remote.path().join("reconnect-preview.txt");
        fs::write(&preview_path, b"preview survives reconnect").unwrap();
        let receive = || {
            worker
                .receiver
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
        };
        worker
            .sender
            .send(Request::Preview {
                id: 10,
                remote: remote_string(&preview_path),
                offset: 0,
            })
            .unwrap();
        assert!(matches!(
            receive(),
            Event::Preview {
                id: 10,
                result: Ok(_)
            }
        ));
        let (ack, broken) = mpsc::channel();
        worker
            .sender
            .send(Request::DisconnectTransport(ack))
            .unwrap();
        broken.recv_timeout(Duration::from_secs(2)).unwrap();
        worker
            .sender
            .send(Request::Preview {
                id: 11,
                remote: remote_string(&preview_path),
                offset: 1,
            })
            .unwrap();
        let Event::Preview {
            id: 11,
            result: Err(error),
        } = receive()
        else {
            panic!("broken transport must fail the preview")
        };
        assert!(is_transport_failure(&error), "{error}");
        assert!(matches!(receive(), Event::Disconnected(message) if message.contains("Reconnect")));
        worker
            .sender
            .send(Request::List(remote_string(remote.path())))
            .unwrap();
        assert!(
            matches!(receive(), Event::ListFailed { error, .. } if error.contains("Connect the file browser")),
            "broken session must not be reused for tree requests"
        );
        worker
            .sender
            .send(Request::Connect {
                profile: profile.clone(),
                secret: Zeroizing::new(String::new()),
                trusted: Some(trust.clone()),
            })
            .unwrap();
        assert!(matches!(receive(), Event::Connected(_)));
        worker
            .sender
            .send(Request::Preview {
                id: 12,
                remote: remote_string(&preview_path),
                offset: 1,
            })
            .unwrap();
        assert!(
            matches!(receive(), Event::Preview { id: 12, result: Ok(chunk) } if chunk.offset == 1 && chunk.bytes == b"review survives reconnect")
        );
        assert!(_session.keepalive_send().is_ok());
        let folder = local.path().join("folder with spaces");
        fs::create_dir_all(folder.join("nested")).unwrap();
        let data: Vec<_> = (0..180_000).map(|n| (n % 251) as u8).collect();
        fs::write(folder.join("nested").join("binary.dat"), &data).unwrap();
        let cancel = AtomicBool::new(false);
        upload(
            &sftp,
            &folder,
            remote.path().to_str().unwrap(),
            0,
            &cancel,
            &|_| {},
        )
        .unwrap();
        let remote_file = remote
            .path()
            .join("folder with spaces")
            .join("nested")
            .join("binary.dat");
        let entries = list(&sftp, remote.path().to_str().unwrap()).unwrap();
        assert!(
            entries
                .iter()
                .any(|e| e.directory && e.name == "folder with spaces")
        );
        let destination = local.path().join("download.bin");
        let mut handle = None;
        let first = read_preview(&sftp, &mut handle, remote_file.to_str().unwrap(), 0).unwrap();
        assert_eq!(first.bytes, data[..CHUNK_BYTES + 3]);
        let second = read_preview(
            &sftp,
            &mut handle,
            remote_file.to_str().unwrap(),
            CHUNK_BYTES as u64,
        )
        .unwrap();
        assert_eq!(second.bytes, data[CHUNK_BYTES..CHUNK_BYTES * 2 + 3]);
        assert!(handle.is_some());
        assert!(read_preview(&sftp, &mut handle, remote.path().to_str().unwrap(), 0).is_err());
        assert_eq!(fs::read(&remote_file).unwrap(), data);
        download(
            &sftp,
            remote_file.to_str().unwrap(),
            &destination,
            &cancel,
            &|_| {},
        )
        .unwrap();
        assert_eq!(fs::read(&destination).unwrap(), data);
        assert!(
            download(
                &sftp,
                remote_file.to_str().unwrap(),
                &destination,
                &cancel,
                &|_| {}
            )
            .is_err()
        );
        assert!(
            upload(
                &sftp,
                &folder,
                remote.path().to_str().unwrap(),
                0,
                &cancel,
                &|_| {}
            )
            .is_err()
        );
        assert_eq!(fs::read(&remote_file).unwrap(), data);
        let cancelled = local.path().join("cancelled.bin");
        assert!(
            download(
                &sftp,
                remote_file.to_str().unwrap(),
                &cancelled,
                &AtomicBool::new(true),
                &|_| {}
            )
            .is_err()
        );
        assert!(!cancelled.exists());
        let upload_cancel = local.path().join("cancel-upload.bin");
        fs::write(&upload_cancel, &data).unwrap();
        let cancel_during = AtomicBool::new(false);
        assert!(
            upload(
                &sftp,
                &upload_cancel,
                remote.path().to_str().unwrap(),
                0,
                &cancel_during,
                &|_| cancel_during.store(true, Ordering::Relaxed)
            )
            .is_err()
        );
        assert!(!remote.path().join("cancel-upload.bin").exists());
        assert!(!fs::read_dir(remote.path()).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".relay-upload")
        }));
    }
}
