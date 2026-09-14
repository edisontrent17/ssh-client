use serde::{Deserialize, Serialize};
use std::{fs, io::Write, path::Path};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    pub name: String,
    pub host: String,
    pub user: String,
    pub port: String,
    pub identity: String,
}

impl Profile {
    pub fn label(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.host
        } else {
            &self.name
        }
    }

    pub fn ssh_args(&self) -> Result<Vec<String>, String> {
        let host = self.host.trim();
        if host.is_empty()
            || host.starts_with('-')
            || !host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ".-_:".contains(c))
        {
            return Err("Enter a hostname, IPv4/IPv6 address, or SSH config alias.".into());
        }
        let user = self.user.trim();
        if !user.is_empty()
            && (user.starts_with('-')
                || !user
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-@\\".contains(c)))
        {
            return Err("The username contains unsupported characters.".into());
        }
        let mut args = vec![
            "-tt".into(),
            "-o".into(),
            "StrictHostKeyChecking=ask".into(),
            "-o".into(),
            "ConnectTimeout=15".into(),
            "-o".into(),
            "ServerAliveInterval=30".into(),
            "-o".into(),
            "ServerAliveCountMax=3".into(),
        ];
        if !user.is_empty() {
            args.extend(["-l".into(), user.into()]);
        }
        if !self.port.trim().is_empty() {
            let port = self
                .port
                .trim()
                .parse::<u16>()
                .ok()
                .filter(|p| *p > 0)
                .ok_or("Port must be between 1 and 65535.")?;
            args.extend(["-p".into(), port.to_string()]);
        }
        if !self.identity.trim().is_empty() {
            let identity = self.identity.trim();
            if !Path::new(identity).is_absolute() || identity.chars().any(char::is_control) {
                return Err("The private key must have an absolute filesystem path.".into());
            }
            if !Path::new(identity).is_file() {
                return Err(format!("Private key file does not exist: {identity}"));
            }
            args.extend(["-i".into(), identity.into()]);
        }
        args.extend(["--".into(), host.into()]);
        Ok(args)
    }
}

pub fn load(path: &Path) -> Result<Vec<Profile>, String> {
    match fs::read(path) {
        Ok(data) => serde_json::from_slice(&data)
            .map_err(|e| format!("Cannot read saved connections at {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("Cannot open {}: {e}", path.display())),
    }
}

pub fn save(path: &Path, profiles: &[Profile]) -> Result<(), String> {
    let write = || -> Result<(), Box<dyn std::error::Error>> {
        let parent = path.parent().ok_or("Missing configuration directory")?;
        fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut file, profiles)?;
        file.flush()?;
        file.as_file().sync_all()?;
        file.persist(path)?;
        Ok(())
    };
    write().map_err(|e| format!("Cannot save connections to {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn profile(host: &str) -> Profile {
        Profile {
            host: host.into(),
            ..Default::default()
        }
    }

    #[test]
    fn rejects_options_shell_syntax_and_invalid_ports() {
        for host in [
            "",
            "-oProxyCommand=evil",
            "host;id",
            "$(id)",
            "a b",
            "user@host",
            "ho\nst",
        ] {
            assert!(profile(host).ssh_args().is_err(), "{host}");
        }
        for port in ["0", "65536", "-1", "ssh"] {
            let mut p = profile("server");
            p.port = port.into();
            assert!(p.ssh_args().is_err());
        }
    }

    #[test]
    fn preserves_config_defaults_and_host_verification() {
        let args = profile("production").ssh_args().unwrap();
        assert_eq!(&args[args.len() - 2..], ["--", "production"]);
        assert!(!args.iter().any(|a| a == "-l" || a == "-p"));
        assert!(args.contains(&"StrictHostKeyChecking=ask".to_string()));
        assert!(profile("2001:db8::1").ssh_args().is_ok());
    }

    #[test]
    fn persistence_roundtrip_and_corruption_detection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        assert!(load(&path).unwrap().is_empty());
        let profiles = vec![profile("one"), profile("two")];
        save(&path, &profiles).unwrap();
        assert_eq!(load(&path).unwrap(), profiles);
        save(&path, &profiles[..1]).unwrap();
        assert_eq!(load(&path).unwrap(), profiles[..1]);
        fs::write(&path, b"invalid").unwrap();
        assert!(load(&path).is_err());
    }
}
