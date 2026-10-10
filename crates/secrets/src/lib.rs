//! Passwords, API keys and tokens, kept where the operating system keeps secrets: the macOS
//! Keychain, the Windows Credential Manager, or the desktop's keyring through Secret Service
//! (GNOME Keyring, KWallet, KeePassXC…) on Linux. Where none can be reached (a server without a
//! desktop session, a sandbox without permission, or a portable install), they go in a JSON file
//! readable only by the user, as before.
//!
//! Names are never secret: in keychain mode a list of them is kept beside the file, so callers can
//! find and clean up their own entries (the keychain itself can't be listed portably).
#![cfg_attr(test, allow(clippy::unwrap_used))]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// The service name entries are filed under in the keychain.
const SERVICE: &str = "CreativeCraft";

/// Somewhere secrets can be kept, one string per name.
pub trait Vault: Send + Sync {
    fn get(&self, name: &str) -> Result<Option<String>, String>;
    fn set(&self, name: &str, value: &str) -> Result<(), String>;
    fn delete(&self, name: &str) -> Result<(), String>;
}

/// The OS keychain, through `keyring-core` and this platform's store.
struct Keychain {
    store: Arc<keyring_core::CredentialStore>,
}

impl Keychain {
    fn entry(&self, name: &str) -> Result<keyring_core::Entry, String> {
        self.store.build(SERVICE, name, None).map_err(|e| e.to_string())
    }
}

impl Vault for Keychain {
    fn get(&self, name: &str) -> Result<Option<String>, String> {
        match self.entry(name)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        self.entry(name)?.set_password(value).map_err(|e| e.to_string())
    }
    fn delete(&self, name: &str) -> Result<(), String> {
        match self.entry(name)?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(target_os = "macos")]
fn platform_store() -> Result<(Arc<keyring_core::CredentialStore>, &'static str), String> {
    let s: Arc<keyring_core::CredentialStore> = apple_native_keyring_store::keychain::Store::new().map_err(|e| e.to_string())?;
    Ok((s, "the macOS Keychain"))
}

#[cfg(windows)]
fn platform_store() -> Result<(Arc<keyring_core::CredentialStore>, &'static str), String> {
    let s: Arc<keyring_core::CredentialStore> = windows_native_keyring_store::Store::new().map_err(|e| e.to_string())?;
    Ok((s, "the Windows Credential Manager"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_store() -> Result<(Arc<keyring_core::CredentialStore>, &'static str), String> {
    let s: Arc<keyring_core::CredentialStore> = zbus_secret_service_keyring_store::Store::new().map_err(|e| e.to_string())?;
    Ok((s, "your desktop's keyring"))
}

#[cfg(not(any(unix, windows)))]
fn platform_store() -> Result<(Arc<keyring_core::CredentialStore>, &'static str), String> {
    Err("no keychain on this platform".into())
}

/// The keychain, if this computer has one that answers (looked for once per process).
fn system_keychain() -> Option<(Arc<dyn Vault>, &'static str)> {
    static FOUND: OnceLock<Option<(Arc<dyn Vault>, &'static str)>> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            if std::env::var_os("CREATIVECRAFT_NO_KEYCHAIN").is_some() {
                return None;
            }
            let (store, place) = match platform_store() {
                Ok(s) => s,
                Err(e) => {
                    log::info!("no keychain ({e}); secrets stay in a private file");
                    return None;
                }
            };
            let vault = Keychain { store };
            // a lookup that finds nothing proves the keychain answers
            match vault.get("availability-check") {
                Ok(_) => Some((Arc::new(vault) as Arc<dyn Vault>, place)),
                Err(e) => {
                    log::info!("the keychain didn't answer ({e}); secrets stay in a private file");
                    None
                }
            }
        })
        .clone()
}

/// The secrets of one CreativeCraft installation.
#[derive(Clone)]
pub struct Secrets {
    /// The private file: all secrets in file mode; in keychain mode, whatever couldn't be moved.
    file: PathBuf,
    vault: Option<(Arc<dyn Vault>, &'static str)>,
    /// Serialises file updates between threads of this process.
    lock: Arc<Mutex<()>>,
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secrets").field("file", &self.file).field("place", &self.place()).finish()
    }
}

impl Secrets {
    /// Only the private file (portable installs, tests).
    pub fn file_only(file: impl Into<PathBuf>) -> Secrets {
        Secrets { file: file.into(), vault: None, lock: Arc::default() }
    }

    /// The OS keychain when there is one, else the private file. Secrets already in the file are
    /// moved into the keychain (each read back before it leaves the file).
    pub fn system(file: impl Into<PathBuf>) -> Secrets {
        match system_keychain() {
            Some((vault, place)) => Secrets::with_vault(file, vault, place),
            None => Secrets::file_only(file),
        }
    }

    /// Secrets kept in `vault`, with `file` for anything it can't take. Moves what the file holds.
    pub fn with_vault(file: impl Into<PathBuf>, vault: Arc<dyn Vault>, place: &'static str) -> Secrets {
        let s = Secrets { file: file.into(), vault: Some((vault, place)), lock: Arc::default() };
        s.migrate();
        s
    }

    /// Where secrets are kept, for Settings ("the macOS Keychain", "a file only you can read").
    pub fn place(&self) -> &'static str {
        self.vault.as_ref().map_or("a file only you can read", |(_, p)| p)
    }

    pub fn in_keychain(&self) -> bool {
        self.vault.is_some()
    }

    fn names_file(&self) -> PathBuf {
        self.file.with_file_name("secret-names.json")
    }

    fn read_file(&self) -> BTreeMap<String, String> {
        std::fs::read(&self.file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    fn read_names(&self) -> BTreeSet<String> {
        std::fs::read(self.names_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    fn write_names(&self, names: &BTreeSet<String>) -> std::io::Result<()> {
        write_private(&self.names_file(), &serde_json::to_vec_pretty(names).map_err(std::io::Error::other)?)
    }

    fn write_file(&self, map: &BTreeMap<String, String>) -> std::io::Result<()> {
        if map.is_empty() && self.vault.is_some() {
            return match std::fs::remove_file(&self.file) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        write_private(&self.file, &serde_json::to_vec_pretty(map).map_err(std::io::Error::other)?)
    }

    /// Move the file's secrets into the keychain; anything that doesn't read back stays in the file.
    fn migrate(&self) {
        let Some((vault, _)) = &self.vault else { return };
        let _guard = self.lock.lock();
        let mut file = self.read_file();
        if file.is_empty() {
            return;
        }
        let mut names = self.read_names();
        let mut moved = Vec::new();
        for (name, value) in &file {
            if vault.set(name, value).is_ok() && vault.get(name).ok().flatten().as_deref() == Some(value.as_str()) {
                moved.push(name.clone());
            } else {
                log::warn!("couldn't move {name} into the keychain; it stays in the private file");
            }
        }
        if moved.is_empty() {
            return;
        }
        names.extend(moved.iter().cloned());
        // the names first: a secret is never in neither place
        if self.write_names(&names).is_err() {
            return;
        }
        for name in &moved {
            file.remove(name);
        }
        if let Err(e) = self.write_file(&file) {
            log::warn!("moved secrets into the keychain but couldn't tidy the private file: {e}");
        }
    }

    pub fn get(&self, name: &str) -> Option<String> {
        if let Some((vault, _)) = &self.vault
            && let Ok(Some(v)) = vault.get(name)
        {
            return Some(v);
        }
        self.read_file().remove(name)
    }

    /// Keep `value` under `name` (in the keychain when there is one; the file otherwise or if the
    /// keychain refuses it).
    pub fn set(&self, name: &str, value: &str) -> std::io::Result<()> {
        let _guard = self.lock.lock();
        if let Some((vault, _)) = &self.vault {
            match vault.set(name, value) {
                Ok(()) => {
                    let mut names = self.read_names();
                    if names.insert(name.to_string()) {
                        self.write_names(&names)?;
                    }
                    // drop any older copy in the file
                    let mut file = self.read_file();
                    if file.remove(name).is_some() {
                        self.write_file(&file)?;
                    }
                    return Ok(());
                }
                Err(e) => log::warn!("the keychain refused {name} ({e}); keeping it in the private file"),
            }
        }
        let mut file = self.read_file();
        if file.get(name).map(String::as_str) != Some(value) {
            file.insert(name.to_string(), value.to_string());
            self.write_file(&file)?;
        }
        Ok(())
    }

    pub fn remove(&self, name: &str) -> std::io::Result<()> {
        let _guard = self.lock.lock();
        if let Some((vault, _)) = &self.vault {
            if let Err(e) = vault.delete(name) {
                log::warn!("couldn't remove {name} from the keychain: {e}");
            }
            let mut names = self.read_names();
            if names.remove(name) {
                self.write_names(&names)?;
            }
        }
        let mut file = self.read_file();
        if file.remove(name).is_some() {
            self.write_file(&file)?;
        }
        Ok(())
    }

    /// Every name with a secret (keychain and file).
    pub fn names(&self) -> Vec<String> {
        let mut all: BTreeSet<String> = self.read_file().into_keys().collect();
        if self.vault.is_some() {
            all.extend(self.read_names());
        }
        all.into_iter().collect()
    }
}

/// Write a file readable only by its owner: a temp file (created 0600 on Unix), then a rename.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    std::io::Write::write_all(&mut f, bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A keychain in memory, which can be told to refuse writes.
    #[derive(Default)]
    struct Memory {
        map: Mutex<BTreeMap<String, String>>,
        refuse: std::sync::atomic::AtomicBool,
    }

    impl Vault for Memory {
        fn get(&self, name: &str) -> Result<Option<String>, String> {
            Ok(self.map.lock().unwrap().get(name).cloned())
        }
        fn set(&self, name: &str, value: &str) -> Result<(), String> {
            if self.refuse.load(std::sync::atomic::Ordering::Relaxed) {
                return Err("locked".into());
            }
            self.map.lock().unwrap().insert(name.into(), value.into());
            Ok(())
        }
        fn delete(&self, name: &str) -> Result<(), String> {
            self.map.lock().unwrap().remove(name);
            Ok(())
        }
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cc-secrets-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn file_mode_keeps_a_private_file() {
        let d = dir("file");
        let s = Secrets::file_only(d.join("secrets.json"));
        s.set("webdav me", "hunter2").unwrap();
        assert_eq!(s.get("webdav me").as_deref(), Some("hunter2"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(d.join("secrets.json")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(s.names(), vec!["webdav me".to_string()]);
        s.remove("webdav me").unwrap();
        assert_eq!(s.get("webdav me"), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn existing_secrets_move_into_the_keychain_and_names_stay_findable() {
        let d = dir("move");
        let file = d.join("secrets.json");
        Secrets::file_only(&file).set("generate openai", "sk-1").unwrap();
        Secrets::file_only(&file).set("google-token", "{\"t\":1}").unwrap();
        let vault = Arc::new(Memory::default());
        let s = Secrets::with_vault(&file, vault.clone(), "a test keychain");
        assert!(!file.exists(), "nothing left in the file");
        assert_eq!(vault.map.lock().unwrap().get("generate openai").map(String::as_str), Some("sk-1"));
        assert_eq!(s.get("google-token").as_deref(), Some("{\"t\":1}"));
        assert_eq!(s.names(), vec!["generate openai".to_string(), "google-token".to_string()]);
        // the names file holds names, never values
        let names = std::fs::read_to_string(d.join("secret-names.json")).unwrap();
        assert!(names.contains("generate openai") && !names.contains("sk-1"));
        s.remove("generate openai").unwrap();
        assert_eq!(s.get("generate openai"), None);
        assert_eq!(s.names(), vec!["google-token".to_string()]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_keychain_that_refuses_leaves_secrets_in_the_file() {
        let d = dir("refuse");
        let file = d.join("secrets.json");
        Secrets::file_only(&file).set("s3 key", "abc").unwrap();
        let vault = Arc::new(Memory::default());
        vault.refuse.store(true, std::sync::atomic::Ordering::Relaxed);
        let s = Secrets::with_vault(&file, vault.clone(), "a locked keychain");
        assert_eq!(s.get("s3 key").as_deref(), Some("abc"), "not moved, not lost");
        s.set("new", "value").unwrap();
        assert_eq!(s.get("new").as_deref(), Some("value"));
        assert!(std::fs::read_to_string(&file).unwrap().contains("value"));
        // once it accepts again, a new value goes to the keychain and the file copy is dropped
        vault.refuse.store(false, std::sync::atomic::Ordering::Relaxed);
        s.set("new", "value2").unwrap();
        assert!(!std::fs::read_to_string(&file).unwrap().contains("value"));
        assert_eq!(s.get("new").as_deref(), Some("value2"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
