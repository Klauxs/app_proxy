//! Preserve every image in the executable's first icon group in a durable ICO.
use super::*;
use crate::store::Store;
use windows_sys::Win32::{
    Foundation::{FreeLibrary, HMODULE},
    System::LibraryLoader::*,
    UI::WindowsAndMessaging::{RT_GROUP_ICON, RT_ICON},
};

const ICON_LIMIT: usize = 16 * 1024 * 1024;
const IMAGE_LIMIT: usize = 8 * 1024 * 1024;
const COUNT_LIMIT: usize = 256;

/// Maps only resources; never executes the source or initializes its imports.
pub fn extract(executable: &Path) -> Result<Vec<u8>> {
    identity::assert_ordinary_user()?;
    absolute(executable)?;
    if !executable
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
    {
        return Err(Error::Invalid("ICON_EXE_REQUIRED"));
    }
    let source = open_source(executable)?;
    let (canonical, expected) = crate::installation::inspect_file(&source)?;
    absolute(&canonical)?;
    verify_source(&canonical, &expected)?;
    let name = wide(canonical.as_os_str())?;
    // SAFETY: absolute file held against writes/deletion, terminated name,
    // resource-only mapping flags. Source directories need no listing permission.
    let module = unsafe {
        LoadLibraryExW(
            name.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_AS_DATAFILE_EXCLUSIVE | LOAD_LIBRARY_AS_IMAGE_RESOURCE,
        )
    };
    if module.is_null() {
        return Err(last_error("IconLoadResources"));
    }
    let module = Module(module);
    let mut group: Option<Result<Vec<u8>>> = None;
    // SAFETY: callback/context live synchronously. Only the EXE's resources are
    // enumerated; no language sidecar search. Callback stops after first group.
    unsafe {
        EnumResourceNamesExW(
            module.0,
            RT_GROUP_ICON,
            Some(first_group),
            (&mut group as *mut Option<Result<Vec<u8>>>) as isize,
            RESOURCE_ENUM_LN,
            0,
        );
    }
    let group = group.ok_or(Error::Invalid("ICON_GROUP_MISSING"))??;
    let bytes = assemble(&group, |id| {
        // SAFETY: resource integer ID, module remains mapped until bytes copied.
        unsafe { resource(module.0, id as usize as *const u16, RT_ICON, IMAGE_LIMIT) }
    })?;
    // Icons are presentation data, never an execution/deletion identity proof.
    // Parent replacement is not an atomic snapshot; reject any observed change.
    verify_source(&canonical, &expected)?;
    verify_source(executable, &expected)?;
    Ok(bytes)
}

fn open_source(path: &Path) -> Result<File> {
    security::no_reparse(path)?;
    let source = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let info = information(&source)?;
    if info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
        return Err(Error::Invalid("ICON_SOURCE_INVALID"));
    }
    Ok(source)
}
fn verify_source(path: &Path, expected: &FileIdentity) -> Result<()> {
    let observed = open_source(path).and_then(|f| information(&f));
    match observed {
        Ok(info) if file_id(&info) == *expected => Ok(()),
        _ => Err(Error::Invalid("ICON_SOURCE_CHANGED")),
    }
}

struct Module(HMODULE);
impl Drop for Module {
    fn drop(&mut self) {
        // SAFETY: successful LoadLibraryExW mapping, released exactly once.
        unsafe {
            FreeLibrary(self.0);
        }
    }
}
unsafe extern "system" fn first_group(
    module: HMODULE,
    kind: *const u16,
    name: *const u16,
    context: isize,
) -> i32 {
    // SAFETY: only called by the synchronous enumeration above with this context.
    unsafe {
        let result = &mut *(context as *mut Option<Result<Vec<u8>>>);
        *result = Some(resource(module, name, kind, 6 + COUNT_LIMIT * 14));
    }
    0
}
unsafe fn resource(
    module: HMODULE,
    name: *const u16,
    kind: *const u16,
    limit: usize,
) -> Result<Vec<u8>> {
    // SAFETY: module and resource names originate from the loader or integer IDs;
    // resource pointers are used only during this mapping, with loader-provided size.
    unsafe {
        let entry = FindResourceW(module, name, kind);
        if entry.is_null() {
            return Err(last_error("IconFindResource"));
        }
        let size = SizeofResource(module, entry) as usize;
        if size == 0 || size > limit {
            return Err(Error::Invalid("ICON_RESOURCE_SIZE"));
        }
        let loaded = LoadResource(module, entry);
        if loaded.is_null() {
            return Err(last_error("IconLoadResource"));
        }
        let bytes = LockResource(loaded);
        if bytes.is_null() {
            return Err(last_error("IconLockResource"));
        }
        Ok(std::slice::from_raw_parts(bytes.cast::<u8>(), size).to_vec())
    }
}

fn assemble(group: &[u8], mut image: impl FnMut(u16) -> Result<Vec<u8>>) -> Result<Vec<u8>> {
    if group.len() < 6 || group[..4] != [0, 0, 1, 0] {
        return Err(Error::Invalid("ICON_GROUP_INVALID"));
    }
    let count = u16::from_le_bytes([group[4], group[5]]) as usize;
    if count == 0 || count > COUNT_LIMIT || group.len() != 6 + count * 14 {
        return Err(Error::Invalid("ICON_GROUP_INVALID"));
    }
    let mut ico = vec![0; 6 + count * 16];
    ico[..6].copy_from_slice(&group[..6]);
    for (index, entry) in group[6..].as_chunks::<14>().0.iter().enumerate() {
        let size = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
        if entry[3] != 0 || size == 0 || size > IMAGE_LIMIT || ico.len() + size > ICON_LIMIT {
            return Err(Error::Invalid("ICON_RESOURCE_SIZE"));
        }
        let bytes = image(u16::from_le_bytes([entry[12], entry[13]]))?;
        if bytes.len() != size {
            return Err(Error::Invalid("ICON_RESOURCE_SIZE"));
        }
        let start = 6 + index * 16;
        let offset = ico.len() as u32;
        ico[start..start + 12].copy_from_slice(&entry[..12]);
        ico[start + 12..start + 16].copy_from_slice(&offset.to_le_bytes());
        ico.extend_from_slice(&bytes);
    }
    Ok(ico)
}

/// The coordinator owns Store. Existing cache entries must match exactly; a
/// changed file is retained for explicit maintenance, never silently overwritten.
pub fn cache(store: &Store, executable: &Path) -> Result<PathBuf> {
    let bytes = extract(executable)?;
    cache_bytes(store, &bytes)
}
fn cache_bytes(store: &Store, bytes: &[u8]) -> Result<PathBuf> {
    let sid = store.load()?.owner_sid;
    // A flat namespace under existing protected state avoids a separate directory
    // creation transaction. The content hash makes package updates independent.
    let hash = format!("{:x}", Sha256::digest(bytes));
    let path = store.root().join("state").join(format!("icon-{hash}.ico"));
    let _parents = pin_parents(&path)?;
    let parent = path.parent().unwrap();
    let directory = security::directory(parent, false)?;
    security::verify(directory.as_raw_handle(), &sid, false)?;
    match std::fs::symlink_metadata(&path) {
        Ok(_) => return verify_cache(&path, &sid, bytes).map(|()| path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    security::verify(temporary.as_file().as_raw_handle(), &sid, false)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(&path) {
        Ok(file) => drop(file),
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(Error::Invalid("ICON_CACHE_PUBLISH_UNCONFIRMED")),
    }
    verify_cache(&path, &sid, bytes)?;
    Ok(path)
}
fn verify_cache(path: &Path, sid: &str, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let info = information(&file)?;
    if info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0
        || info.nNumberOfLinks != 1
    {
        return Err(Error::Invalid("ICON_CACHE_CONFLICT"));
    }
    security::verify(file.as_raw_handle(), sid, false)?;
    let mut actual = Vec::new();
    (&mut file)
        .take(ICON_LIMIT as u64 + 1)
        .read_to_end(&mut actual)?;
    if actual != bytes {
        return Err(Error::Invalid("ICON_CACHE_CONFLICT"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
