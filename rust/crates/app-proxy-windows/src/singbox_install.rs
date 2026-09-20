//! Fixed official artifact, strict member allowlist, protected staging and
//! complete-directory publication. No package manager or user install is edited.
use crate::{
    Error, Result, singbox_binary, storage_security as security,
    store::{self, Store},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Cursor, Read},
    os::windows::{
        fs::OpenOptionsExt,
        io::{AsRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
};
use uuid::Uuid;
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ};

// Keep the native error and the bounded operation name without logging paths.
fn install_io<T>(operation: &'static str, result: std::io::Result<T>) -> Result<T> {
    result.map_err(|error| match error.raw_os_error() {
        Some(code) => Error::Windows {
            operation,
            code: code as u32,
        },
        None => Error::Io(error),
    })
}

fn install_store_io<T>(operation: &'static str, result: Result<T>) -> Result<T> {
    result.or_else(|error| match error {
        Error::Io(error) => install_io(operation, Err(error)),
        other => Err(other),
    })
}

pub const VERSION: &str = "1.14.1";
pub const URL: &str = "https://github.com/SagerNet/sing-box/releases/download/v1.14.1/sing-box-1.14.1-windows-amd64.zip";
pub const ARCHIVE_SIZE: usize = 32_841_719;
pub const ARCHIVE_SHA256: &str = "5197f16d492d93202dc623622149a6ed040f8eca263128f91d603f2b901baa89";
const PREFIX: &str = "sing-box-1.14.1-windows-amd64/";
const CHECK: &[u8] = br#"{"log":{"disabled":true},"inbounds":[],"outbounds":[]}"#;

struct Member {
    name: &'static str,
    size: usize,
    digest: &'static str,
}
const MEMBERS: [Member; 3] = [
    Member {
        name: "sing-box.exe",
        size: 81_883_648,
        digest: "b838de45bd0b2e6ddbed1977e4745622f7dffab3b293807ff4c6b1b640fed909",
    },
    Member {
        name: "libcronet.dll",
        size: 9_529_344,
        digest: "3217c6260fbca5f16072e0b79735742f40109a63bb0ff88fd6b96dd6b54a2928",
    },
    Member {
        name: "LICENSE",
        size: 808,
        digest: "bb3805862b583aee73ad6f7805ec634747a37257a637a3069857843f05ea589c",
    },
];

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema_version: u32,
    store_id: Uuid,
    version: String,
    source: String,
    archive_sha256: String,
}

pub struct InstallStage {
    path: PathBuf,
    root: PathBuf,
    store_id: Uuid,
    sid: String,
    files: Vec<File>,
    directory: Option<OwnedHandle>,
    _parents: Vec<OwnedHandle>,
    validated: bool,
    published: bool,
    _owner: std::sync::Arc<File>,
}

impl Store {
    /// Short gate: the returned capability retains store ownership while hashing,
    /// unpacking and probing run without holding the configuration mutex.
    pub fn core_installation(&self) -> Result<CoreInstallation> {
        let manifest = self.load()?;
        let root = security::directory(self.root(), false)?;
        security::verify(root.as_raw_handle(), &manifest.owner_sid, true)?;
        Ok(CoreInstallation {
            root: self.root().to_owned(),
            store_id: manifest.store_id,
            sid: manifest.owner_sid,
            _root: root,
            owner: self.owner_lease(),
        })
    }
}

pub struct CoreInstallation {
    root: PathBuf,
    store_id: Uuid,
    sid: String,
    _root: OwnedHandle,
    owner: std::sync::Arc<File>,
}

impl CoreInstallation {
    fn install_parents(&self, create: bool) -> Result<Vec<OwnedHandle>> {
        let mut pins = Vec::new();
        for relative in ["", "bin", "bin/sing-box"] {
            let path = self.root.join(relative);
            if create && !install_io("CoreInstallInspectParent", path.try_exists())? {
                install_io("CoreInstallCreateParent", fs::create_dir(&path))?;
            }
            let pin = security::directory(&path, false)?;
            security::verify(pin.as_raw_handle(), &self.sid, false)?;
            pins.push(pin);
        }
        Ok(pins)
    }

    /// Existing complete versions are verified and retained, never overwritten.
    pub fn core_is_installed(&self) -> Result<bool> {
        let path = self.root.join(format!("bin/sing-box/{VERSION}"));
        if !install_io("CoreInstallInspectDestination", path.try_exists())? {
            return Ok(false);
        }
        let _parents = self.install_parents(false)?;
        let pin = security::directory(&path, false)?;
        security::verify(pin.as_raw_handle(), &self.sid, false)?;
        verify_receipt(&path, self.store_id, &self.sid)?;
        let _files = pin_members(&path, &self.sid)?;
        Ok(true)
    }

    pub fn prepare_core_install(&self, bytes: &[u8]) -> Result<InstallStage> {
        crate::identity::assert_ordinary_user()?;
        if bytes.len() != ARCHIVE_SIZE || digest(bytes) != ARCHIVE_SHA256 {
            return Err(Error::Invalid("CORE_ARCHIVE_DIGEST_MISMATCH"));
        }
        // Verify all names, types, lengths and digests before creating staging.
        let members = unpack(bytes, &MEMBERS)?;
        let parents = self.install_parents(true)?;
        let path = self
            .root
            .join(format!("bin/sing-box/.staging-{}", Uuid::new_v4()));
        install_io("CoreInstallCreateStaging", fs::create_dir(&path))?;
        let directory = security::directory(&path, false)?;
        security::verify(directory.as_raw_handle(), &self.sid, false)?;
        let mut stage = InstallStage {
            path,
            root: self.root.clone(),
            store_id: self.store_id,
            sid: self.sid.clone(),
            files: Vec::new(),
            directory: Some(directory),
            _parents: parents,
            validated: false,
            published: false,
            _owner: self.owner.clone(),
        };
        for (name, data) in members {
            install_store_io(
                "CoreInstallWriteMember",
                store::write_new(&stage.path.join(name), &data, &stage.sid),
            )?;
        }
        let receipt = Receipt {
            schema_version: 1,
            store_id: stage.store_id,
            version: VERSION.into(),
            source: URL.into(),
            archive_sha256: ARCHIVE_SHA256.into(),
        };
        install_store_io(
            "CoreInstallWriteReceipt",
            store::write_new(
                &stage.path.join("installation.json"),
                &store::encode(&receipt, 4096)?,
                &stage.sid,
            ),
        )?;
        install_store_io(
            "CoreInstallWriteCheck",
            store::write_new(&stage.path.join(".check.json"), CHECK, &stage.sid),
        )?;
        stage.files = pin_members(&stage.path, &stage.sid)?;
        Ok(stage)
    }

    pub fn publish_core_install(&self, mut stage: InstallStage) -> Result<()> {
        if !stage.validated || stage.store_id != self.store_id || stage.root != self.root {
            return Err(Error::Invalid("CORE_INSTALL_NOT_VALIDATED"));
        }
        if self.core_is_installed()? {
            return Ok(());
        }
        verify_receipt(&stage.path, stage.store_id, &stage.sid)?;
        // Release our own file/directory pins just for the directory rename.
        // Parent handles stay pinned, and publication never replaces a target.
        stage.files.clear();
        stage.directory.take();
        install_io(
            "CoreInstallRemoveCheck",
            fs::remove_file(stage.path.join(".check.json")),
        )?;
        install_io(
            "CoreInstallRenameDirectory",
            fs::rename(
                &stage.path,
                self.root.join(format!("bin/sing-box/{VERSION}")),
            ),
        )?;
        stage.published = true;
        if !self.core_is_installed()? {
            return Err(Error::Invalid("CORE_INSTALL_PUBLICATION_UNCONFIRMED"));
        }
        Ok(())
    }
}

impl InstallStage {
    /// Probe version and a harmless check config; actual proxy configs are still
    /// checked again before every core launch.
    pub async fn validate(&mut self) -> Result<()> {
        let binary = singbox_binary::inspect_managed(self.path.join("sing-box.exe")).await?;
        if binary.version() != VERSION {
            return Err(Error::Invalid("CORE_INSTALL_VERSION_MISMATCH"));
        }
        if install_store_io(
            "CoreInstallReadCheck",
            store::read_protected(&self.path.join(".check.json"), &self.sid, 4096),
        )? != CHECK
        {
            return Err(Error::Invalid("CORE_INSTALL_CHECK_CHANGED"));
        }
        binary.check_config(&self.path.join(".check.json")).await?;
        self.validated = true;
        Ok(())
    }
}

impl Drop for InstallStage {
    fn drop(&mut self) {
        self.files.clear();
        self.directory.take();
        if self.published {
            return;
        }
        // Only our fixed staging files are removed. Never recursively delete a
        // path supplied by an archive, and preserve any unexpected contents.
        if let Ok(pin) = security::directory(&self.path, false)
            && security::verify(pin.as_raw_handle(), &self.sid, false).is_ok()
        {
            for name in [
                "sing-box.exe",
                "libcronet.dll",
                "LICENSE",
                "installation.json",
                ".check.json",
            ] {
                let path = self.path.join(name);
                if security::no_reparse(&path).is_ok() {
                    let _ = fs::remove_file(path);
                }
            }
            drop(pin);
            let _ = fs::remove_dir(&self.path);
        }
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn unpack(bytes: &[u8], expected: &[Member]) -> Result<Vec<(&'static str, Vec<u8>)>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|_| Error::Invalid("CORE_ARCHIVE_INVALID"))?;
    if archive.len() != expected.len() {
        return Err(Error::Invalid("CORE_ARCHIVE_MEMBERS_INVALID"));
    }
    let mut found = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|_| Error::Invalid("CORE_ARCHIVE_INVALID"))?;
        let name = entry
            .name()
            .strip_prefix(PREFIX)
            .ok_or(Error::Invalid("CORE_ARCHIVE_PATH_INVALID"))?;
        let member = expected
            .iter()
            .find(|m| m.name == name)
            .ok_or(Error::Invalid("CORE_ARCHIVE_PATH_INVALID"))?;
        if found.iter().any(|(name, _)| *name == member.name)
            || entry.is_dir()
            || entry.is_symlink()
            || entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 != 0 && mode & 0o170000 != 0o100000)
            || entry.size() != member.size as u64
        {
            return Err(Error::Invalid("CORE_ARCHIVE_MEMBERS_INVALID"));
        }
        let mut data = Vec::new();
        entry
            .by_ref()
            .take(member.size as u64 + 1)
            .read_to_end(&mut data)
            .map_err(|_| Error::Invalid("CORE_ARCHIVE_INVALID"))?;
        if data.len() != member.size || digest(&data) != member.digest {
            return Err(Error::Invalid("CORE_MEMBER_DIGEST_MISMATCH"));
        }
        found.push((member.name, data));
    }
    Ok(found)
}

fn pin_members(path: &Path, sid: &str) -> Result<Vec<File>> {
    let mut files = Vec::new();
    for member in &MEMBERS {
        let path = path.join(member.name);
        security::no_reparse(&path)?;
        let mut file = install_io(
            "CoreInstallOpenMember",
            OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&path),
        )?;
        security::verify(file.as_raw_handle(), sid, false)?;
        let metadata = install_io("CoreInstallInspectMember", file.metadata())?;
        if !metadata.is_file() || metadata.len() != member.size as u64 {
            return Err(Error::Invalid("CORE_MEMBER_DIGEST_MISMATCH"));
        }
        let mut hash = Sha256::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            let n = install_io("CoreInstallReadMember", file.read(&mut buffer))?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
        }
        if format!("{:x}", hash.finalize()) != member.digest {
            return Err(Error::Invalid("CORE_MEMBER_DIGEST_MISMATCH"));
        }
        files.push(file);
    }
    Ok(files)
}

fn verify_receipt(path: &Path, id: Uuid, sid: &str) -> Result<()> {
    let receipt: Receipt = store::decode(&install_store_io(
        "CoreInstallReadReceipt",
        store::read_protected(&path.join("installation.json"), sid, 4096),
    )?)?;
    if receipt.schema_version != 1
        || receipt.store_id != id
        || receipt.version != VERSION
        || receipt.source != URL
        || receipt.archive_sha256 != ARCHIVE_SHA256
    {
        return Err(Error::Invalid("CORE_INSTALL_RECEIPT_MISMATCH"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn archive(name: &str, data: &[u8], symlink: bool) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        if symlink {
            writer.add_symlink(name, "target", options).unwrap();
        } else {
            writer.start_file(name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn archive_allowlist_rejects_traversal_aliases_links_extra_entries_and_bad_content() {
        let member = Member {
            name: "sing-box.exe",
            size: 0,
            digest: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        };
        let expected = [member];
        let name = format!("{PREFIX}sing-box.exe");
        assert!(unpack(&archive(&name, b"", false), &expected).is_ok());
        for bad in [
            "../sing-box.exe",
            "/sing-box.exe",
            "C:/sing-box.exe",
            "sing-box-1.14.1-windows-amd64/../sing-box.exe",
            "sing-box-1.14.1-windows-amd64/sing-box.exe:stream",
            "sing-box-1.14.1-windows-amd64/SING-BOX.EXE",
            "sing-box-1.14.1-windows-amd64/sing-box.exe.",
            "sing-box-1.14.1-windows-amd64/other.dll",
        ] {
            assert!(
                unpack(&archive(bad, b"", false), &expected).is_err(),
                "{bad}"
            );
        }
        assert!(unpack(&archive(&name, b"", true), &expected).is_err());
        assert!(unpack(&archive(&name, b"changed", false), &expected).is_err());
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for name in [name, format!("{PREFIX}other.dll")] {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
        }
        assert!(unpack(&writer.finish().unwrap().into_inner(), &expected).is_err());
    }

    #[test]
    fn invalid_archive_and_unowned_target_are_preserved_without_staging() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let store = Store::create(&root).unwrap();
        let installer = store.core_installation().unwrap();
        assert!(!installer.core_is_installed().unwrap());
        assert!(matches!(
            installer.prepare_core_install(b"invalid"),
            Err(Error::Invalid("CORE_ARCHIVE_DIGEST_MISMATCH"))
        ));
        assert!(!root.join("bin").exists());
        let target = root.join(format!("bin/sing-box/{VERSION}"));
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("user.txt"), b"keep").unwrap();
        assert!(installer.core_is_installed().is_err());
        assert_eq!(fs::read(target.join("user.txt")).unwrap(), b"keep");
    }

    #[test]
    fn installation_capability_retains_owner_lock_without_borrowing_the_store() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let store = Store::create(&root).unwrap();
        let installer = store.core_installation().unwrap();
        drop(store);
        assert!(matches!(
            Store::open(&root),
            Err(Error::Invalid("STORE_ALREADY_OWNED"))
        ));
        assert!(!installer.core_is_installed().unwrap());
        drop(installer);
        assert!(Store::open(&root).is_ok());
    }

    #[tokio::test]
    #[ignore = "requires APP_PROXY_TEST_SING_BOX_ZIP; injects a file lock only in a temporary store"]
    async fn official_bundle_publication_reports_native_file_lock_operation() {
        let archive = fs::read(
            std::env::var_os("APP_PROXY_TEST_SING_BOX_ZIP").expect("official fixture archive"),
        )
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let store = Store::create(&temp.path().join("store")).unwrap();
        let installer = store.core_installation().unwrap();
        let mut stage = installer.prepare_core_install(&archive).unwrap();
        stage.validate().await.unwrap();
        let path = stage.path.clone();
        let lock = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(path.join(".check.json"))
            .unwrap();
        let error = installer.publish_core_install(stage).unwrap_err();
        assert!(
            matches!(
                error,
                Error::Windows {
                    operation: "CoreInstallRemoveCheck",
                    code: 32
                }
            ),
            "{error:?}"
        );
        drop(lock);
        assert!(!installer.core_is_installed().unwrap());
        let mut retry = installer.prepare_core_install(&archive).unwrap();
        retry.validate().await.unwrap();
        installer.publish_core_install(retry).unwrap();
        assert!(installer.core_is_installed().unwrap());
    }

    #[tokio::test]
    #[ignore = "requires APP_PROXY_TEST_SING_BOX_ZIP; installs only into an isolated temporary store"]
    async fn official_bundle_validates_publishes_reuses_and_cleans_cancelled_staging() {
        let archive = fs::read(
            std::env::var_os("APP_PROXY_TEST_SING_BOX_ZIP").expect("official fixture archive"),
        )
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let store = Store::create(&root).unwrap();
        let installer = store.core_installation().unwrap();
        let stage = installer.prepare_core_install(&archive).unwrap();
        let cancelled = stage.path.clone();
        assert!(!installer.core_is_installed().unwrap());
        assert!(fs::write(stage.path.join("libcronet.dll"), b"overwrite").is_err());
        assert!(matches!(
            installer.publish_core_install(stage),
            Err(Error::Invalid("CORE_INSTALL_NOT_VALIDATED"))
        ));
        assert!(!cancelled.exists());
        let mut stage = installer.prepare_core_install(&archive).unwrap();
        let path = stage.path.clone();
        stage.validate().await.unwrap();
        installer.publish_core_install(stage).unwrap();
        assert!(!path.exists());
        assert!(installer.core_is_installed().unwrap());
        let target = root.join(format!("bin/sing-box/{VERSION}"));
        let image = crate::identity::file_identity(&target.join("sing-box.exe")).unwrap();
        let mut repeated = installer.prepare_core_install(&archive).unwrap();
        repeated.validate().await.unwrap();
        installer.publish_core_install(repeated).unwrap();
        assert_eq!(
            image,
            crate::identity::file_identity(&target.join("sing-box.exe")).unwrap()
        );
        drop(installer);
        drop(store);
        let store = Store::open(&root).unwrap();
        let installer = store.core_installation().unwrap();
        assert!(installer.core_is_installed().unwrap());
        let binary = singbox_binary::discover(&root).await.unwrap().unwrap();
        assert_eq!(binary.version(), VERSION);
        assert!(matches!(binary.source(), singbox_binary::Source::Managed));
        drop(binary);
        fs::write(target.join("LICENSE"), b"changed").unwrap();
        assert!(installer.core_is_installed().is_err());
        assert_eq!(fs::read(target.join("LICENSE")).unwrap(), b"changed");
    }
}
