use crate::error::{AbError, Result};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub fn default_home() -> PathBuf {
    std::env::var_os("AGENTBOX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into()))
                .join(".local/state/agentbox-rust")
        })
}
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let mut diff = a.len() ^ b.len();
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= (x ^ y) as usize;
    }
    diff == 0
}
pub fn private_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700).create(path)?;
    }
    let m = fs::symlink_metadata(path)?;
    if !m.is_dir()
        || m.file_type().is_symlink()
        || m.uid() != unsafe { libc::geteuid() }
        || m.mode() & 0o077 != 0
    {
        return Err(AbError::new(
            403,
            "State directory must be an owned, nonsymlink directory with mode 0700",
        ));
    }
    Ok(())
}
pub fn private_file(path: &Path, create: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(create)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let m = file.metadata()?;
    if !m.is_file()
        || m.uid() != unsafe { libc::geteuid() }
        || m.mode() & 0o077 != 0
        || m.nlink() != 1
    {
        return Err(AbError::new(
            403,
            "Private state file must be owned, regular, unlinked elsewhere, and mode 0600",
        ));
    }
    Ok(file)
}
pub fn init_home(path: &Path) -> Result<PathBuf> {
    private_dir(path)?;
    let path = fs::canonicalize(path)?;
    if path == Path::new("/")
        || path == Path::new("/tmp")
        || std::env::var_os("HOME")
            .map(PathBuf::from)
            .and_then(|p| fs::canonicalize(p).ok())
            .as_ref()
            == Some(&path)
    {
        return Err(AbError::new(
            422,
            "Use a dedicated private state subdirectory, not a filesystem, home, or temporary root",
        ));
    }
    use std::os::unix::ffi::OsStrExt;
    if path.join("broker.sock").as_os_str().as_bytes().len() > 100 {
        return Err(AbError::new(
            422,
            "State path is too long for a Unix socket; choose a shorter --home",
        ));
    }
    private_dir(&path.join("runtime"))?;
    let token_path = path.join("token");
    if token_path.symlink_metadata().is_err() {
        let staged = path.join(format!(".token-{}", uuid::Uuid::new_v4().simple()));
        let publication = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&staged)?;
            let token = format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            );
            file.write_all(token.as_bytes())?;
            file.sync_all()?;
            drop(file);
            let from = std::ffi::CString::new(staged.as_os_str().as_bytes())
                .map_err(|_| AbError::new(422, "Invalid state path"))?;
            let to = std::ffi::CString::new(token_path.as_os_str().as_bytes())
                .map_err(|_| AbError::new(422, "Invalid state path"))?;
            if unsafe { crate::platform::rename_exclusive(&from, &to) } != 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(e.into());
                }
            }
            File::open(&path)?.sync_all()?;
            Ok(())
        })();
        let _ = fs::remove_file(&staged);
        publication?;
    }
    read_token(&path)?;
    Ok(path)
}
pub fn read_token(home: &Path) -> Result<String> {
    private_dir_existing(home)?;
    let mut token = String::new();
    private_file(&home.join("token"), false)?
        .take(129)
        .read_to_string(&mut token)?;
    if token.len() != 64 || !token.bytes().all(|x| x.is_ascii_hexdigit()) {
        return Err(AbError::new(403, "Invalid private token file"));
    }
    Ok(token)
}
pub fn private_dir_existing(path: &Path) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    if !m.is_dir()
        || m.file_type().is_symlink()
        || m.uid() != unsafe { libc::geteuid() }
        || m.mode() & 0o077 != 0
    {
        return Err(AbError::new(403, "Unsafe private state directory"));
    }
    Ok(())
}
pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn token_is_private_stable() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("state");
        init_home(&p).unwrap();
        let a = read_token(&p).unwrap();
        assert_eq!(a, read_token(&p).unwrap());
        assert_eq!(fs::metadata(p.join("token")).unwrap().mode() & 0o777, 0o600);
    }
    #[test]
    fn unsafe_token_rejected() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("state");
        init_home(&p).unwrap();
        fs::set_permissions(p.join("token"), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_token(&p).is_err());
    }
    #[test]
    fn symlink_rejected() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("state");
        init_home(&p).unwrap();
        let q = d.path().join("alias");
        std::os::unix::fs::symlink(&p, &q).unwrap();
        assert!(read_token(&q).is_err());
    }
    #[test]
    fn equality() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
    }
    #[test]
    fn concurrent_token_creation_is_atomic() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("state");
        let jobs: Vec<_> = (0..16)
            .map(|_| {
                let p = p.clone();
                std::thread::spawn(move || {
                    init_home(&p).unwrap();
                    read_token(&p).unwrap()
                })
            })
            .collect();
        let values: Vec<_> = jobs.into_iter().map(|j| j.join().unwrap()).collect();
        assert!(values.iter().all(|v| v == &values[0]));
    }
    #[test]
    fn fifo_token_is_rejected_without_blocking() {
        use std::os::unix::ffi::OsStrExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("state");
        private_dir(&p).unwrap();
        let path = std::ffi::CString::new(p.join("token").as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert!(read_token(&p).is_err());
    }
}
