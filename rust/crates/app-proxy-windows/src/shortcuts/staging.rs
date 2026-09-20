//! Capture exact ownership before publishing a desktop entry. The integration
//! journal must persist Staged before calling publish; this module has no journal.
use super::*;
use windows_sys::Win32::Storage::FileSystem::{FILE_RENAME_INFO, FileRenameInfo};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Staged {
    pub path: PathBuf,
    pub receipt: Receipt,
}

/// The non-launchable temporary file is in the destination directory, so rename
/// retains its recorded file identity. A crash before journaling may leave it;
/// such an unknown file must never be claimed or deleted by its name alone.
pub fn prepare(destination: &Path, spec: &Spec, bytes: &[u8]) -> Result<Staged> {
    identity::assert_ordinary_user()?;
    let _parents = parent(destination)?;
    vacant(destination)?;
    inspect_bytes(spec, bytes)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".app-proxy-stage-")
        .rand_bytes(16)
        .suffix(".tmp")
        .tempfile_in(destination.parent().unwrap())?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    let receipt = Receipt {
        file: file_id(&information(temporary.as_file())?),
        sha256: Sha256::digest(bytes).into(),
    };
    let (file, path) = temporary.keep().map_err(|e| Error::Io(e.error))?;
    drop(file);
    let staged = Staged { path, receipt };
    verify(&staged, spec)?;
    Ok(staged)
}

pub fn verify(staged: &Staged, spec: &Spec) -> Result<()> {
    identity::assert_ordinary_user()?;
    stage_path(&staged.path)?;
    let _parents = pin_parents(&staged.path)?;
    let (_file, actual, bytes) = read_pinned(&staged.path, false)?;
    verify_bytes(spec, &staged.receipt, &actual, &bytes)
}

pub(super) fn present(staged: &Staged, spec: &Spec) -> Result<bool> {
    stage_path(&staged.path)?;
    let _parents = pin_parents(&staged.path)?;
    match read_pinned(&staged.path, false) {
        Ok((_file, actual, bytes)) => {
            verify_bytes(spec, &staged.receipt, &actual, &bytes)?;
            Ok(true)
        }
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

pub fn publish(staged: &Staged, destination: &Path, spec: &Spec) -> Result<()> {
    identity::assert_ordinary_user()?;
    stage_path(&staged.path)?;
    if staged.path.parent() != destination.parent() {
        return Err(Error::Invalid("SHORTCUT_STAGE_DIRECTORY_CHANGED"));
    }
    let _parents = parent(destination)?;
    vacant(destination)?;
    let (file, actual, bytes) = read_pinned(&staged.path, true)?;
    verify_bytes(spec, &staged.receipt, &actual, &bytes)?;
    rename_handle(&file, destination)?;
    drop(file);
    super::verify(destination, spec, &staged.receipt)?;
    notify(destination);
    Ok(())
}

pub fn remove(staged: &Staged, spec: &Spec) -> Result<()> {
    identity::assert_ordinary_user()?;
    stage_path(&staged.path)?;
    let _parents = pin_parents(&staged.path)?;
    remove_verified(&staged.path, spec, &staged.receipt)
}

pub(super) fn stage_path(path: &Path) -> Result<()> {
    absolute(path)?;
    if !path
        .file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.starts_with(".app-proxy-stage-") && s.ends_with(".tmp"))
    {
        return Err(Error::Invalid("SHORTCUT_STAGE_PATH_INVALID"));
    }
    Ok(())
}
fn vacant(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
        Ok(_) => Err(Error::Invalid("SHORTCUT_PATH_OCCUPIED")),
    }
}
fn rename_handle(file: &File, destination: &Path) -> Result<()> {
    let name = wide(destination.as_os_str())?;
    let offset = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    let size = (offset + name.len() * 2).max(std::mem::size_of::<FILE_RENAME_INFO>());
    // u64 storage supplies the alignment required by the native structure on
    // supported Windows x64. Zero initializes flags, root handle and padding.
    let mut buffer = vec![0u64; size.div_ceil(8)];
    // SAFETY: aligned buffer has space for the header and entire UTF-16 name.
    // The same READ|DELETE handle was checked above and denies write/delete
    // sharing. ReplaceIfExists stays false: even a late occupant is preserved.
    unsafe {
        let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        (*info).FileNameLength = ((name.len() - 1) * 2) as u32;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            buffer.as_mut_ptr().cast::<u8>().add(offset).cast::<u16>(),
            name.len(),
        );
        if SetFileInformationByHandle(
            file.as_raw_handle(),
            FileRenameInfo,
            info.cast(),
            size as u32,
        ) == 0
        {
            return Err(last_error("ShortcutPublishRename"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_rename_refuses_an_occupant_arriving_after_precheck() {
        let (_root, spec, target) = super::super::tests::setup();
        let staged = prepare(&target, &spec, &encode(&spec).unwrap()).unwrap();
        let _parents = parent(&target).unwrap();
        vacant(&target).unwrap();
        let (file, actual, bytes) = read_pinned(&staged.path, true).unwrap();
        verify_bytes(&spec, &staged.receipt, &actual, &bytes).unwrap();
        std::fs::write(&target, b"late occupant").unwrap();
        assert!(rename_handle(&file, &target).is_err());
        drop(file);
        assert_eq!(std::fs::read(&target).unwrap(), b"late occupant");
        verify(&staged, &spec).unwrap();
    }

    #[test]
    fn journaled_file_identity_survives_handle_publication_and_late_occupants() {
        let (_root, spec, target) = super::super::tests::setup();
        let bytes = encode(&spec).unwrap();
        let staged = prepare(&target, &spec, &bytes).unwrap();
        assert!(!target.exists());
        assert_eq!(staged.path.extension().unwrap(), "tmp");
        let journal = serde_json::to_vec(&staged).unwrap();
        let restored: Staged = serde_json::from_slice(&journal).unwrap();
        verify(&restored, &spec).unwrap();
        std::fs::write(&target, b"user occupied target").unwrap();
        assert!(publish(&restored, &target, &spec).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"user occupied target");
        verify(&restored, &spec).unwrap();
        std::fs::remove_file(&target).unwrap();
        publish(&restored, &target, &spec).unwrap();
        assert!(!restored.path.exists());
        super::super::verify(&target, &spec, &restored.receipt).unwrap();
        super::super::remove(&target, &spec, &restored.receipt).unwrap();
    }

    #[test]
    fn changed_or_replaced_stage_and_wrong_directory_are_preserved() {
        let (root, spec, target) = super::super::tests::setup();
        let bytes = encode(&spec).unwrap();
        let staged = prepare(&target, &spec, &bytes).unwrap();
        std::fs::write(&staged.path, b"user change").unwrap();
        assert!(publish(&staged, &target, &spec).is_err());
        assert!(remove(&staged, &spec).is_err());
        assert_eq!(std::fs::read(&staged.path).unwrap(), b"user change");
        std::fs::rename(&staged.path, root.path().join("retained.tmp")).unwrap();
        std::fs::write(&staged.path, &bytes).unwrap();
        assert!(publish(&staged, &target, &spec).is_err());
        assert!(remove(&staged, &spec).is_err());
        let staged = prepare(&target, &spec, &bytes).unwrap();
        let other = root.path().join("other");
        std::fs::create_dir(&other).unwrap();
        assert!(publish(&staged, &other.join("entry.lnk"), &spec).is_err());
        assert!(!target.exists());
        remove(&staged, &spec).unwrap();
    }
}
