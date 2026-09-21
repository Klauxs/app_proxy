#![forbid(unsafe_code)]
#![cfg(windows)]

use app_proxy_windows::{
    Error, Result,
    setup::{self, Maintenance},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub const NAMES: [&str; 2] = ["app-proxy.exe", "app-proxy-host.exe"];
const MARKER: &str = ".app-proxy-install.json";
const JOURNAL: &str = ".app-proxy-upgrade.json";
const LIMIT: u64 = 256 * 1024 * 1024;

pub struct FilePayload<'a> {
    pub bytes: &'a [u8],
    pub sha256: &'a str,
}
pub struct Payload<'a> {
    pub build: &'a str,
    pub files: [FilePayload<'a>; 2],
}
impl Payload<'_> {
    fn manifest(&self) -> Result<Installed> {
        for file in &self.files {
            if file.bytes.is_empty()
                || file.bytes.len() as u64 > LIMIT
                || digest(file.bytes) != file.sha256
            {
                return Err(Error::Invalid("SETUP_PAYLOAD_INVALID"));
            }
        }
        Ok(Installed {
            format: "app-proxy-install-v1".into(),
            build: self.build.into(),
            hashes: [self.files[0].sha256.into(), self.files[1].sha256.into()],
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Installed {
    format: String,
    build: String,
    hashes: [String; 2],
}
impl Installed {
    fn validate(&self) -> Result<()> {
        if self.format != "app-proxy-install-v1"
            || self.build.is_empty()
            || self.build.len() > 128
            || self.hashes.iter().any(|h| {
                h.len() != 64
                    || !h
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            })
        {
            return Err(Error::Invalid("SETUP_INSTALL_RECORD_INVALID"));
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    old: Option<Installed>,
    new: Installed,
    published: bool,
}

pub struct Installation {
    root: PathBuf,
    journal: Journal,
    lease: Maintenance,
    _owner: fs::File,
}
impl Installation {
    /// Publish the pair under a persistent transaction. Before publication,
    /// failures roll back only verified owned bytes; after publication, rerun
    /// the same setup to finish UAC/integrations without changing file identities.
    pub fn begin(root: &Path, payload: &Payload<'_>) -> Result<Self> {
        Self::begin_with(root, payload, |_| Ok(()))
    }
    fn begin_with(
        root: &Path,
        payload: &Payload<'_>,
        mut boundary: impl FnMut(usize) -> Result<()>,
    ) -> Result<Self> {
        let new = payload.manifest()?;
        new.validate()?;
        let _root_pin = setup::pin_directory(root)?;
        let owner_path = root.join(".app-proxy-setup.lock");
        if owner_path.try_exists()? {
            setup::verify_plain_file(&owner_path)?;
        }
        let owner = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(owner_path)?;
        let lease = Maintenance::acquire(root)?;
        wait_for_exit(root, Duration::from_secs(60))?;
        let journal_path = root.join(JOURNAL);
        if journal_path.try_exists()? {
            let mut pending: Journal = read_json(&journal_path)?;
            validate_journal(&pending)?;
            if pending.published {
                verify_installation(root, &pending.new)?;
                if pending.new == new {
                    return Ok(Self {
                        root: root.into(),
                        journal: pending,
                        lease,
                        _owner: owner,
                    });
                }
                // A fixed installer must be able to replace a release whose
                // finalization failed. Roll back only journal-owned binaries
                // first; the new transaction still validates every file.
                pending.published = false;
                atomic_json(&journal_path, &pending)?;
            }
            rollback(root, &pending, (pending.new == new).then_some(payload))?;
        }
        let old = if root.join(MARKER).try_exists()? {
            let installed: Installed = read_json(&root.join(MARKER))?;
            installed.validate()?;
            verify_installation(root, &installed)?;
            Some(installed)
        } else {
            // A first install owns no arbitrary existing directory contents.
            for item in fs::read_dir(root)? {
                let name = item?.file_name();
                if name != ".app-proxy-update.lock" && name != ".app-proxy-setup.lock" {
                    return Err(Error::Invalid(
                        "安装目录已有未登记文件，请保留原文件并处理目录冲突后重试。",
                    ));
                }
            }
            None
        };
        let mut journal = Journal {
            version: 1,
            old,
            new,
            published: false,
        };
        atomic_json(&journal_path, &journal)?;
        let result = (|| {
            for (index, name) in NAMES.iter().enumerate() {
                let target = root.join(name);
                if journal
                    .old
                    .as_ref()
                    .is_some_and(|m| m.hashes[index] == journal.new.hashes[index])
                {
                    continue;
                }
                let stage = root.join(format!(".setup-new-{name}"));
                if stage.try_exists()? {
                    remove_expected(&stage, &journal.new.hashes[index])?;
                }
                let mut output = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&stage)?;
                output.write_all(payload.files[index].bytes)?;
                output.sync_all()?;
                drop(output);
                verify_hash(&stage, &journal.new.hashes[index])?;
                if let Some(old) = &journal.old {
                    verify_hash(&target, &old.hashes[index])?;
                    let backup = root.join(format!(".setup-old-{name}"));
                    if backup.try_exists()? {
                        return Err(Error::Invalid("SETUP_BACKUP_CONFLICT"));
                    }
                    fs::rename(&target, &backup)?;
                }
                boundary(index * 2)?;
                fs::rename(&stage, &target)?;
                boundary(index * 2 + 1)?;
            }
            atomic_json(&root.join(MARKER), &journal.new)?;
            boundary(4)?;
            journal.published = true;
            atomic_json(&journal_path, &journal)?;
            Ok(())
        })();
        if let Err(error) = result {
            journal.published = false;
            if rollback(root, &journal, Some(payload)).is_err() {
                return Err(Error::Invalid(
                    "升级中断，自动恢复尚未完成；请保留目录并重新运行安装包。",
                ));
            }
            return Err(error);
        }
        Ok(Self {
            root: root.into(),
            journal,
            lease,
            _owner: owner,
        })
    }
    pub fn release_for_verification(&mut self) {
        self.lease.release_signal();
    }
    pub fn complete(self) -> Result<()> {
        verify_installation(&self.root, &self.journal.new)?;
        // Remove only the exact old package files; application/config data are
        // never inside this transaction or its cleanup allowlist.
        for (i, name) in NAMES.iter().enumerate() {
            let path = self.root.join(format!(".setup-old-{name}"));
            if path.try_exists()? {
                let old = self
                    .journal
                    .old
                    .as_ref()
                    .ok_or(Error::Invalid("SETUP_BACKUP_CONFLICT"))?;
                remove_expected(&path, &old.hashes[i])?;
            }
        }
        fs::remove_file(self.root.join(JOURNAL))?;
        Ok(())
    }
}

fn validate_journal(value: &Journal) -> Result<()> {
    if value.version != 1 {
        return Err(Error::Invalid("SETUP_JOURNAL_INVALID"));
    }
    value.new.validate()?;
    if let Some(old) = &value.old {
        old.validate()?;
    }
    Ok(())
}

fn rollback(root: &Path, journal: &Journal, payload: Option<&Payload<'_>>) -> Result<()> {
    validate_journal(journal)?;
    for (i, name) in NAMES.iter().enumerate() {
        let target = root.join(name);
        let backup = root.join(format!(".setup-old-{name}"));
        if let Some(old) = &journal.old {
            if backup.try_exists()? {
                verify_hash(&backup, &old.hashes[i])?;
                if target.try_exists()? {
                    remove_expected(&target, &journal.new.hashes[i])?;
                }
                fs::rename(backup, target)?;
            } else {
                verify_hash(&target, &old.hashes[i])?;
            }
        } else if target.try_exists()? {
            remove_expected(&target, &journal.new.hashes[i])?;
        }
        let stage = root.join(format!(".setup-new-{name}"));
        if stage.try_exists()? {
            setup::verify_plain_file(&stage)?;
            let mut bytes = Vec::new();
            fs::File::open(&stage)?
                .take(LIMIT + 1)
                .read_to_end(&mut bytes)?;
            if digest(&bytes) != journal.new.hashes[i]
                && !payload.is_some_and(|p| p.files[i].bytes.starts_with(&bytes))
            {
                return Err(Error::Invalid("SETUP_STAGING_CHANGED_USE_ORIGINAL_PACKAGE"));
            }
            fs::remove_file(stage)?;
        }
    }
    let marker = root.join(MARKER);
    if marker.try_exists()? {
        let observed: Installed = read_json(&marker)?;
        if observed != journal.new && Some(&observed) != journal.old.as_ref() {
            return Err(Error::Invalid("SETUP_INSTALL_RECORD_CHANGED"));
        }
    }
    if let Some(old) = &journal.old {
        atomic_json(&marker, old)?;
    } else if marker.try_exists()? {
        fs::remove_file(marker)?;
    }
    fs::remove_file(root.join(JOURNAL))?;
    Ok(())
}

fn wait_for_exit(root: &Path, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let mut pins = Vec::new();
        let mut busy = false;
        for name in NAMES
            .into_iter()
            .map(String::from)
            .chain(NAMES.map(|n| format!(".setup-old-{n}")))
        {
            let path = root.join(name);
            if !path.try_exists()? {
                continue;
            }
            setup::verify_plain_file(&path)?;
            match OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(0)
                .open(path)
            {
                Ok(file) => pins.push(file),
                Err(e) if matches!(e.raw_os_error(), Some(5 | 32 | 33)) => {
                    busy = true;
                    break;
                }
                Err(e) => return Err(e.into()),
            }
        }
        if !busy {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::Invalid(
                "程序仍被占用。请退出 AppProxy 菜单后重新运行安装包；应用与代理数据会保留。",
            ));
        }
        drop(pins);
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn verify_installation(root: &Path, installed: &Installed) -> Result<()> {
    for (i, name) in NAMES.iter().enumerate() {
        verify_hash(&root.join(name), &installed.hashes[i])?;
    }
    Ok(())
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn verify_hash(path: &Path, expected: &str) -> Result<()> {
    setup::verify_plain_file(path)?;
    let file = fs::File::open(path)?;
    if file.metadata()?.len() > LIMIT {
        return Err(Error::Invalid("SETUP_FILE_SIZE"));
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if digest(&bytes) != expected {
        return Err(Error::Invalid("已安装文件被修改，安装程序未覆盖它。"));
    }
    Ok(())
}
fn remove_expected(path: &Path, expected: &str) -> Result<()> {
    verify_hash(path, expected)?;
    fs::remove_file(path)?;
    Ok(())
}
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    setup::verify_plain_file(path)?;
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(32769).read_to_end(&mut bytes)?;
    if bytes.len() > 32768 {
        return Err(Error::Invalid("SETUP_RECORD_SIZE"));
    }
    Ok(serde_json::from_slice(&bytes)?)
}
fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    if path.try_exists()? {
        setup::verify_plain_file(path)?;
    }
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    file.write_all(&serde_json::to_vec(value)?)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| Error::Io(e.error))?;
    Ok(())
}

#[cfg(test)]
mod tests;
