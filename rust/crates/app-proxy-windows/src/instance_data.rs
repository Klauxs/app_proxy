//! Prepare only registered isolated data. Original application data is never adopted.
use crate::{
    Error, Result,
    package::Package,
    storage_security as security,
    store::{self, Store},
};
use app_proxy_core::model::{ApplicationLocator, InstanceData, StorageLocation};
use app_proxy_core::template::IsolatedPaths;
use serde::{Deserialize, Serialize};
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const MARKER: &str = ".app-proxy-rust-data.json";

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DataOwner {
    format: String,
    schema_version: u32,
    owner_sid: String,
    store_id: Option<Uuid>,
    instance_id: Option<Uuid>,
    package_family: Option<String>,
}

pub struct PreparedData {
    pub paths: IsolatedPaths,
    // Retain root and data directories against rename throughout launch preparation.
    _directories: Vec<OwnedHandle>,
}

pub(crate) struct PackageControlRoot {
    pub root: PathBuf,
    _directories: Vec<OwnedHandle>,
}

impl PreparedData {
    /// Physical directory identity, independent of path spelling. Preparation
    /// keeps this directory pinned against replacement while the key is used.
    pub fn physical_identity(&self) -> Result<app_proxy_core::FileIdentity> {
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let root = security::directory(&self.paths.root, false)?;
        // SAFETY: this C output structure contains only integer fields.
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: the owned directory handle and sized output live through the call.
        if unsafe { GetFileInformationByHandle(root.as_raw_handle(), &mut info) } == 0 {
            return Err(crate::last_error("InstanceDirectoryIdentity"));
        }
        Ok(app_proxy_core::FileIdentity {
            volume_serial: info.dwVolumeSerialNumber,
            file_index: ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
        })
    }
}

impl Store {
    /// Helper requests need the same shared LocalState as isolated package data.
    /// Only our marked namespace is created; the application's directory is not adopted.
    pub(crate) fn prepare_package_control(&self, package: &Package) -> Result<PackageControlRoot> {
        self.prepare_package_control_with(package, local_app_data)
    }

    fn prepare_package_control_with(
        &self,
        package: &Package,
        local_folder: impl FnOnce() -> Result<PathBuf>,
    ) -> Result<PackageControlRoot> {
        let manifest = self.load()?;
        let mut handles = vec![security::directory(self.root(), false)?];
        let base = if package.isolated_storage {
            app_proxy_core::model::safe_relative(Path::new(&package.family_name))
                .map_err(|e| Error::Invalid(e.0))?;
            if Path::new(&package.family_name).components().count() != 1 {
                return Err(Error::Invalid("INVALID_PACKAGE_LOCATOR"));
            }
            let local = local_folder()?
                .join("Packages")
                .join(&package.family_name)
                .join("LocalState");
            handles.push(security::directory(&local, false)?);
            let container = local.join("AppProxy");
            let mut owner = DataOwner {
                format: "app-proxy-rust-data".into(),
                schema_version: 1,
                owner_sid: manifest.owner_sid.clone(),
                store_id: None,
                instance_id: None,
                package_family: Some(package.family_name.clone()),
            };
            handles.push(claim(&container, &owner, true)?);
            let base = container.join(manifest.store_id.to_string());
            owner.store_id = Some(manifest.store_id);
            handles.push(claim(&base, &owner, true)?);
            base
        } else {
            self.root().to_owned()
        };
        let state = base.join("state");
        handles.push(directory(&state, &manifest.owner_sid, true)?);
        Ok(PackageControlRoot {
            root: state,
            _directories: handles,
        })
    }

    pub fn prepare_instance_data(
        &self,
        instance_id: Uuid,
        resolved_package: Option<&Package>,
    ) -> Result<Option<PreparedData>> {
        self.access_instance_data(instance_id, resolved_package, local_app_data, true)
    }

    /// Open and pin existing owned directories without creating or adopting data.
    pub fn inspect_instance_data(
        &self,
        instance_id: Uuid,
        resolved_package: Option<&Package>,
    ) -> Result<Option<PreparedData>> {
        self.access_instance_data(instance_id, resolved_package, local_app_data, false)
    }

    #[cfg(test)]
    fn prepare_data_with_local_folder(
        &self,
        instance_id: Uuid,
        resolved_package: Option<&Package>,
        local_folder: impl FnOnce() -> Result<PathBuf>,
    ) -> Result<Option<PreparedData>> {
        self.access_instance_data(instance_id, resolved_package, local_folder, true)
    }

    fn access_instance_data(
        &self,
        instance_id: Uuid,
        resolved_package: Option<&Package>,
        local_folder: impl FnOnce() -> Result<PathBuf>,
        create: bool,
    ) -> Result<Option<PreparedData>> {
        let manifest = self.load()?;
        let instance = manifest
            .instances
            .iter()
            .find(|i| i.id == instance_id)
            .ok_or(Error::Invalid("INSTANCE_NOT_FOUND"))?;
        let InstanceData::Isolated { location } = &instance.data else {
            return Ok(None);
        };
        let app = manifest
            .applications
            .iter()
            .find(|a| a.id == instance.application_id)
            .ok_or(Error::Invalid("APPLICATION_NOT_FOUND"))?;
        match (&app.locator, resolved_package) {
            (ApplicationLocator::Exe { .. }, None) => {}
            (
                ApplicationLocator::Msix {
                    family_name,
                    app_id,
                },
                Some(package),
            ) if &package.family_name == family_name && &package.app_id == app_id => {}
            _ => return Err(Error::Invalid("INSTANCE_PACKAGE_MISMATCH")),
        }
        if matches!(location, StorageLocation::Store { .. })
            && resolved_package.is_some_and(|p| p.isolated_storage)
        {
            return Err(Error::Invalid("PACKAGE_STORAGE_MODE_CHANGED"));
        }
        let mut handles = Vec::new();
        let mut owner = DataOwner {
            format: "app-proxy-rust-data".into(),
            schema_version: 1,
            owner_sid: manifest.owner_sid,
            store_id: Some(manifest.store_id),
            instance_id: None,
            package_family: None,
        };
        let (base, relative) = match location {
            StorageLocation::Store { relative_path } => {
                let root = security::directory(self.root(), false)?;
                security::verify(root.as_raw_handle(), &owner.owner_sid, true)?;
                handles.push(root);
                (self.root().to_owned(), relative_path)
            }
            StorageLocation::PackageLocalState {
                family_name,
                namespace,
                relative_path,
            } => {
                let local_state = local_folder()?
                    .join("Packages")
                    .join(family_name)
                    .join("LocalState");
                // LocalState belongs to the installed application. Never change its ACL or adopt its content.
                handles.push(security::directory(&local_state, false)?);
                let container = local_state.join("AppProxy");
                let container_owner = DataOwner {
                    store_id: None,
                    instance_id: None,
                    package_family: Some(family_name.clone()),
                    format: owner.format.clone(),
                    schema_version: 1,
                    owner_sid: owner.owner_sid.clone(),
                };
                handles.push(claim(&container, &container_owner, create)?);
                let base = container.join(namespace);
                owner.package_family = Some(family_name.clone());
                handles.push(claim(&base, &owner, create)?);
                (base, relative_path)
            }
        };
        // Model validation has fixed this relative path to instances/<instance UUID>.
        let parent = base.join("instances");
        handles.push(directory(&parent, &owner.owner_sid, create)?);
        let root = base.join(relative);
        owner.instance_id = Some(instance_id);
        handles.push(claim(&root, &owner, create)?);
        let paths = IsolatedPaths {
            user_data: root.join("user-data"),
            app_home: root.join("app-home"),
            root,
        };
        handles.push(directory(&paths.user_data, &owner.owner_sid, create)?);
        handles.push(directory(&paths.app_home, &owner.owner_sid, create)?);
        Ok(Some(PreparedData {
            paths,
            _directories: handles,
        }))
    }
}

pub(crate) fn verify_package_control(
    base: &Path,
    store_id: Uuid,
    sid: &str,
    family: &str,
) -> Result<()> {
    let container = base
        .parent()
        .ok_or(Error::Invalid("PACKAGE_CONTROL_PATH_INVALID"))?;
    let local = container
        .parent()
        .ok_or(Error::Invalid("PACKAGE_CONTROL_PATH_INVALID"))?;
    if base.file_name() != Some(std::ffi::OsStr::new(&store_id.to_string()))
        || container.file_name() != Some(std::ffi::OsStr::new("AppProxy"))
        || local.file_name() != Some(std::ffi::OsStr::new("LocalState"))
        || local.parent().and_then(Path::file_name) != Some(std::ffi::OsStr::new(family))
    {
        return Err(Error::Invalid("PACKAGE_CONTROL_PATH_INVALID"));
    }
    for (path, id) in [(container, None), (base, Some(store_id))] {
        let handle = security::directory(path, false)?;
        security::verify(handle.as_raw_handle(), sid, true)?;
        let actual: DataOwner =
            store::decode(&store::read_protected(&path.join(MARKER), sid, 4096)?)?;
        let expected = DataOwner {
            format: "app-proxy-rust-data".into(),
            schema_version: 1,
            owner_sid: sid.into(),
            store_id: id,
            instance_id: None,
            package_family: Some(family.into()),
        };
        if actual != expected {
            return Err(Error::Invalid("PACKAGE_CONTROL_OWNER_MISMATCH"));
        }
    }
    Ok(())
}

fn directory(path: &Path, sid: &str, create: bool) -> Result<OwnedHandle> {
    if create && !path.try_exists()? {
        security::no_reparse(
            path.parent()
                .ok_or(Error::Invalid("DATA_PARENT_REQUIRED"))?,
        )?;
        std::fs::create_dir(path)?;
    }
    let handle = security::directory(path, false)?;
    security::verify(handle.as_raw_handle(), sid, false)?;
    Ok(handle)
}

fn claim(path: &Path, expected: &DataOwner, create: bool) -> Result<OwnedHandle> {
    let created = if create && !path.try_exists()? {
        security::no_reparse(
            path.parent()
                .ok_or(Error::Invalid("DATA_PARENT_REQUIRED"))?,
        )?;
        std::fs::create_dir(path)?;
        true
    } else {
        false
    };
    let handle = security::directory(path, created)?;
    if created {
        security::protect(&handle, &expected.owner_sid)?;
        store::write_new(
            &path.join(MARKER),
            &store::encode(expected, 4096)?,
            &expected.owner_sid,
        )?;
    } else {
        security::verify(handle.as_raw_handle(), &expected.owner_sid, true)?;
        let actual: DataOwner = store::decode(&store::read_protected(
            &path.join(MARKER),
            &expected.owner_sid,
            4096,
        )?)?;
        if actual != *expected {
            return Err(Error::Invalid("INSTANCE_DATA_OWNER_MISMATCH"));
        }
    }
    Ok(handle)
}

pub fn local_app_data() -> Result<PathBuf> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath};
    // SAFETY: current-user known folder output is CoTaskMem allocated and freed on every path.
    unsafe {
        let mut value = std::ptr::null_mut();
        let code =
            SHGetKnownFolderPath(&FOLDERID_LocalAppData, 0, std::ptr::null_mut(), &mut value);
        if code < 0 {
            return Err(Error::Windows {
                operation: "GetLocalAppData",
                code: code as u32,
            });
        }
        let mut length = 0;
        while *value.add(length) != 0 {
            length += 1;
        }
        let text = String::from_utf16(std::slice::from_raw_parts(value, length));
        CoTaskMemFree(value.cast());
        let path = PathBuf::from(text.map_err(|_| Error::Invalid("NON_UNICODE_LOCAL_APP_DATA"))?);
        if !path.is_absolute() {
            return Err(Error::Invalid("LOCAL_APP_DATA_NOT_ABSOLUTE"));
        }
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use app_proxy_core::model::Manifest;

    #[test]
    fn package_control_namespaces_are_shared_marked_and_separate_between_stores() {
        let temp = tempfile::tempdir().unwrap();
        let local = temp.path().join("local");
        std::fs::create_dir_all(local.join("Packages/Fixture_publisher/LocalState")).unwrap();
        let package = Package {
            family_name: "Fixture_publisher".into(),
            full_name: "Fixture_1_x64__publisher".into(),
            app_id: "App".into(),
            exe: std::env::current_exe().unwrap(),
            isolated_storage: true,
        };
        let first = Store::create(&temp.path().join("first")).unwrap();
        let second = Store::create(&temp.path().join("second")).unwrap();
        let a = first
            .prepare_package_control_with(&package, || Ok(local.clone()))
            .unwrap();
        let b = second
            .prepare_package_control_with(&package, || Ok(local.clone()))
            .unwrap();
        assert_ne!(a.root, b.root);
        let header = first.load().unwrap();
        verify_package_control(
            a.root.parent().unwrap(),
            header.store_id,
            &header.owner_sid,
            &package.family_name,
        )
        .unwrap();
        assert!(
            verify_package_control(
                b.root.parent().unwrap(),
                header.store_id,
                &header.owner_sid,
                &package.family_name
            )
            .is_err()
        );
        let again = first
            .prepare_package_control_with(&package, || Ok(local.clone()))
            .unwrap();
        assert_eq!(a.root, again.root);
        drop(again);
        drop(a);
        let foreign = local
            .join("Packages/Fixture_publisher/LocalState/AppProxy")
            .join(header.store_id.to_string())
            .join(MARKER);
        std::fs::write(&foreign, b"{}").unwrap();
        assert!(
            first
                .prepare_package_control_with(&package, || Ok(local))
                .is_err()
        );
    }

    fn populated(root: &Path) -> (Store, Uuid) {
        let mut store = Store::create(root).unwrap();
        let header = store.load().unwrap();
        let mut manifest: Manifest =
            serde_json::from_str(include_str!("../../../examples/manifest.json")).unwrap();
        manifest.store_id = header.store_id;
        manifest.owner_sid = header.owner_sid;
        let instance_id = manifest.instances[1].id;
        manifest.instances[1].data = InstanceData::Isolated {
            location: StorageLocation::PackageLocalState {
                family_name: "OpenAI.Codex_2p2nqsd0c76g0".into(),
                namespace: manifest.store_id.to_string(),
                relative_path: PathBuf::from("instances").join(instance_id.to_string()),
            },
        };
        store.commit(1, manifest).unwrap();
        (store, instance_id)
    }
    fn package() -> Package {
        Package {
            family_name: "OpenAI.Codex_2p2nqsd0c76g0".into(),
            full_name: "fixture-version".into(),
            app_id: "App".into(),
            exe: r"C:\fixture\Codex.exe".into(),
            isolated_storage: true,
        }
    }

    #[test]
    fn package_namespaces_separate_stores_and_preserve_local_state() {
        let fixture = tempfile::tempdir().unwrap();
        let local = fixture.path().join("local-app-data");
        let package = package();
        let local_state = local
            .join("Packages")
            .join(&package.family_name)
            .join("LocalState");
        std::fs::create_dir_all(&local_state).unwrap();
        std::fs::write(local_state.join("existing-app-data"), "keep").unwrap();
        let (first, id) = populated(&fixture.path().join("store-a"));
        let prepared = first
            .prepare_data_with_local_folder(id, Some(&package), || Ok(local.clone()))
            .unwrap()
            .unwrap();
        let first_store_id = first.load().unwrap().store_id;
        assert_eq!(
            prepared.paths.root,
            local_state
                .join("AppProxy")
                .join(first_store_id.to_string())
                .join("instances")
                .join(id.to_string())
        );
        let (second, other_id) = populated(&fixture.path().join("store-b"));
        let other = second
            .prepare_data_with_local_folder(other_id, Some(&package), || Ok(local.clone()))
            .unwrap()
            .unwrap();
        assert_ne!(prepared.paths.root, other.paths.root);
        assert!(!first.root().join("instances").exists());
        assert!(!second.root().join("instances").exists());
        assert_eq!(
            std::fs::read_to_string(local_state.join("existing-app-data")).unwrap(),
            "keep"
        );
        drop(prepared);
        let mut updated = package;
        updated.isolated_storage = false;
        let same = first
            .prepare_data_with_local_folder(id, Some(&updated), || Ok(local))
            .unwrap()
            .unwrap();
        assert!(same.paths.root.starts_with(local_state));
    }

    #[test]
    fn inspecting_package_data_never_claims_missing_localstate_namespaces() {
        let fixture = tempfile::tempdir().unwrap();
        let local = fixture.path().join("local");
        let package = package();
        let local_state = local
            .join("Packages")
            .join(&package.family_name)
            .join("LocalState");
        std::fs::create_dir_all(&local_state).unwrap();
        let (store, id) = populated(&fixture.path().join("store"));
        assert!(
            store
                .access_instance_data(id, Some(&package), || Ok(local.clone()), false)
                .is_err()
        );
        assert!(!local_state.join("AppProxy").exists());
        let prepared = store
            .prepare_data_with_local_folder(id, Some(&package), || Ok(local.clone()))
            .unwrap()
            .unwrap();
        let expected = prepared.paths.root.clone();
        drop(prepared);
        let inspected = store
            .access_instance_data(id, Some(&package), || Ok(local), false)
            .unwrap()
            .unwrap();
        assert_eq!(inspected.paths.root, expected);
    }

    #[test]
    fn package_mismatch_and_store_virtualization_fail_before_creating_data() {
        let fixture = tempfile::tempdir().unwrap();
        let (mut store, id) = populated(&fixture.path().join("store"));
        let mut wrong = package();
        wrong.app_id = "different".into();
        assert!(
            store
                .prepare_data_with_local_folder(id, Some(&wrong), || panic!("must not access data"))
                .is_err()
        );
        assert!(
            store
                .prepare_data_with_local_folder(id, None, || panic!("must not access data"))
                .is_err()
        );
        let mut manifest = store.load().unwrap();
        manifest.instances[1].data = InstanceData::Isolated {
            location: StorageLocation::Store {
                relative_path: PathBuf::from("instances").join(id.to_string()),
            },
        };
        store.commit(2, manifest).unwrap();
        assert_eq!(
            store
                .prepare_data_with_local_folder(id, Some(&package()), || panic!())
                .err()
                .unwrap()
                .to_string(),
            "PACKAGE_STORAGE_MODE_CHANGED"
        );
        assert!(!store.root().join("instances").exists());
    }
}
