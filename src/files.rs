use crate::profiles::Profile;
use base64::Engine;
use eframe::egui;
use ssh2::{CheckResult, KnownHostFileKind, OpenFlags, OpenType, RenameFlags, Session, Sftp};
use std::{
    fs,
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
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
    Connect {
        profile: Profile,
        secret: Zeroizing<String>,
        trusted: Option<String>,
    },
    List(String),
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
    Trust(String),
    Entries(String, Vec<Entry>),
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
        std::thread::spawn(move || {
            let report = |event| {
                let _ = events.send(event);
                ctx.request_repaint();
            };
            let mut connection: Option<Sftp> = None;
            while let Ok(request) = requests.recv() {
                let result = match request {
                    Request::Connect {
                        profile,
                        secret,
                        trusted,
                    } => {
                        connection = None;
                        match connect(&profile, &secret, trusted.as_deref()) {
                            Ok(Connection::Ready(sftp)) => match sftp.realpath(Path::new(".")) {
                                Ok(home) => {
                                    report(Event::Connected(remote_string(&home)));
                                    connection = Some(sftp);
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
                    Request::List(path) => match connection.as_ref() {
                        Some(sftp) => {
                            list(sftp, &path).map(|entries| report(Event::Entries(path, entries)))
                        }
                        None => Err("Connect the file browser first.".into()),
                    },
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
            }
        });
        Self {
            sender,
            receiver,
            cancel,
        }
    }
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
    Ready(Sftp),
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
    session
        .sftp()
        .map(Connection::Ready)
        .map_err(|e| e.to_string())
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
    entries.sort_by(|a, b| {
        b.directory
            .cmp(&a.directory)
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
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
            Connection::Ready(_) => panic!("Use a fixture port absent from known hosts"),
        };
        assert!(matches!(
            connect(&profile, "", Some("incorrect fingerprint")).unwrap(),
            Connection::Trust(_)
        ));
        let Connection::Ready(sftp) = connect(&profile, "", Some(&trust)).unwrap() else {
            panic!("Trust was not honored")
        };
        let remote = tempfile::tempdir().unwrap();
        let local = tempfile::tempdir().unwrap();
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
