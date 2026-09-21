//! Cache only parsed manifest metadata. Every use checks the current user's
//! package registration, install path and manifest bytes through native APIs.
//! Process arguments/identities and live installation handles are never cached.
use super::*;
use sha2::{Digest, Sha256};
use std::{collections::VecDeque, fs::File, path::PathBuf, ptr, sync::Mutex};
use windows_sys::Win32::{Foundation::*, Storage::Packaging::Appx::*};

const CACHE_LIMIT: usize = 16;
const MANIFEST_LIMIT: u64 = 1024 * 1024;
static CACHE: Mutex<VecDeque<Entry>> = Mutex::new(VecDeque::new());

#[derive(Clone, PartialEq, Eq)]
struct Fingerprint {
    full_name: String,
    directory: PathBuf,
    manifest: [u8; 32],
}
struct Entry {
    sid: String,
    family: String,
    app: String,
    fingerprint: Fingerprint,
    package: Package,
}

pub(super) fn resolve(family: &str, app: &str) -> Result<Package> {
    crate::identity::assert_ordinary_user()?;
    let sid = crate::identity::current()?.user_sid;
    resolve_with(
        &CACHE,
        &sid,
        family,
        app,
        || fingerprint(family),
        || {
            super::bridge(
                &serde_json::json!({"operation":"discover", "family_name":family, "app_id":app}),
            )
        },
    )
}

fn resolve_with(
    cache: &Mutex<VecDeque<Entry>>,
    sid: &str,
    family: &str,
    app: &str,
    mut inspect: impl FnMut() -> Result<Fingerprint>,
    parse: impl FnOnce() -> Result<Package>,
) -> Result<Package> {
    // An absent/ambiguous package or an unreadable manifest must fail even if a
    // previous lookup succeeded. No time-based stale-result fallback.
    let before = inspect()?;
    let cached = {
        let entries = cache.lock().unwrap_or_else(|p| p.into_inner());
        entries
            .iter()
            .find(|e| e.sid == sid && e.family == family && e.app == app && e.fingerprint == before)
            .map(|e| e.package.clone())
    };
    if let Some(package) = cached {
        if !package.exe.is_file() {
            return Err(Error::Invalid(
                app_proxy_core::error_code::APP_NOT_INSTALLED,
            ));
        }
        return Ok(package);
    }
    let package = parse()?;
    if package.full_name != before.full_name
        || !package.family_name.eq_ignore_ascii_case(family)
        || !package.app_id.eq_ignore_ascii_case(app)
        || inspect()? != before
    {
        return Err(Error::Invalid("PACKAGE_CHANGED"));
    }
    let mut entries = cache.lock().unwrap_or_else(|p| p.into_inner());
    entries.retain(|e| !(e.sid == sid && e.family == family && e.app == app));
    if entries.len() == CACHE_LIMIT {
        entries.pop_front();
    }
    entries.push_back(Entry {
        sid: sid.into(),
        family: family.into(),
        app: app.into(),
        fingerprint: before,
        package: package.clone(),
    });
    Ok(package)
}

fn fingerprint(family: &str) -> Result<Fingerprint> {
    let full_name = registered(family)?;
    let name = wide(&full_name);
    let mut len = 0;
    // SAFETY: NUL-terminated input and valid output length; first call sizes only.
    let code = unsafe { GetPackagePathByFullName(name.as_ptr(), &mut len, ptr::null_mut()) };
    if code != ERROR_INSUFFICIENT_BUFFER {
        return Err(native_error(code, "GetPackagePath"));
    }
    if !(2..=32768).contains(&len) {
        return Err(Error::Invalid("INVALID_PACKAGE_PATH"));
    }
    let mut path = vec![0u16; len as usize];
    // SAFETY: path has exactly the capacity returned by the sizing call.
    let code = unsafe { GetPackagePathByFullName(name.as_ptr(), &mut len, path.as_mut_ptr()) };
    if code != ERROR_SUCCESS {
        return Err(native_error(code, "GetPackagePath"));
    }
    let directory = PathBuf::from(string(&path)?);
    if !directory.is_absolute() {
        return Err(Error::Invalid("INVALID_PACKAGE_PATH"));
    }
    let file = File::open(directory.join("AppxManifest.xml"))?;
    let mut bytes = Vec::new();
    file.take(MANIFEST_LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MANIFEST_LIMIT {
        return Err(Error::Invalid("PACKAGE_MANIFEST_TOO_LARGE"));
    }
    Ok(Fingerprint {
        full_name,
        directory,
        manifest: Sha256::digest(bytes).into(),
    })
}

fn registered(family: &str) -> Result<String> {
    let family = wide(family);
    let mut count = 0;
    let mut len = 0;
    let filters = PACKAGE_FILTER_HEAD | PACKAGE_FILTER_DIRECT;
    // SAFETY: sizing call with no output buffers and live NUL-terminated family.
    let code = unsafe {
        FindPackagesByPackageFamily(
            family.as_ptr(),
            filters,
            &mut count,
            ptr::null_mut(),
            &mut len,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    if code != ERROR_SUCCESS && code != ERROR_INSUFFICIENT_BUFFER {
        return Err(native_error(code, "FindRegisteredPackage"));
    }
    if count == 0 {
        return Err(Error::Invalid(
            app_proxy_core::error_code::APP_NOT_INSTALLED,
        ));
    }
    if count != 1 {
        return Err(Error::Invalid("AMBIGUOUS_PACKAGE"));
    }
    if !(2..=32768).contains(&len) {
        return Err(Error::Invalid("INVALID_PACKAGE_IDENTITY"));
    }
    let mut buffer = vec![0u16; len as usize];
    let mut name = ptr::null_mut();
    // SAFETY: one pointer output and the exact sized string buffer. An update
    // that increases the required capacity fails rather than using old data.
    let code = unsafe {
        FindPackagesByPackageFamily(
            family.as_ptr(),
            filters,
            &mut count,
            &mut name,
            &mut len,
            buffer.as_mut_ptr(),
            ptr::null_mut(),
        )
    };
    if code != ERROR_SUCCESS {
        return Err(native_error(code, "FindRegisteredPackage"));
    }
    if count != 1 {
        return Err(Error::Invalid("PACKAGE_CHANGED"));
    }
    let offset = (name as usize)
        .checked_sub(buffer.as_ptr() as usize)
        .filter(|offset| offset % 2 == 0 && *offset / 2 < buffer.len())
        .ok_or(Error::Invalid("INVALID_PACKAGE_IDENTITY"))?
        / 2;
    string(&buffer[offset..])
}
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
fn string(value: &[u16]) -> Result<String> {
    let end = value
        .iter()
        .position(|v| *v == 0)
        .filter(|n| *n > 0)
        .ok_or(Error::Invalid("INVALID_PACKAGE_IDENTITY"))?;
    String::from_utf16(&value[..end]).map_err(|_| Error::Invalid("INVALID_PACKAGE_IDENTITY"))
}
fn native_error(code: u32, operation: &'static str) -> Error {
    Error::Windows { operation, code }
}

#[cfg(test)]
mod tests;
