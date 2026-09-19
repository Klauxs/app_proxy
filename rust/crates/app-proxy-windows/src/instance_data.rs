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

impl Store {
    pub fn prepare_instance_data(
        &self,
        instance_id: Uuid,
        resolved_package: Option<&Package>,
    ) -> Result<Option<PreparedData>> {
        self.prepare_data_with_local_folder(instance_id, resolved_package, local_app_data)
    }

    fn prepare_data_with_local_folder(
        &self,
        instance_id: Uuid,
        resolved_package: Option<&Package>,
        local_folder: impl FnOnce() -> Result<PathBuf>,
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
                let container = local_state.join("AppProxyRust");
                let container_owner = DataOwner {
                    store_id: None,
                    instance_id: None,
                    package_family: Some(family_name.clone()),
                    format: owner.format.clone(),
                    schema_version: 1,
                    owner_sid: owner.owner_sid.clone(),
                };
                handles.push(claim(&container, &container_owner)?);
                let base = container.join(namespace);
                owner.package_family = Some(family_name.clone());
                handles.push(claim(&base, &owner)?);
                (base, relative_path)
            }
        };
        // Model validation has fixed this relative path to instances/<instance UUID>.
        let parent = base.join("instances");
        handles.push(directory(&parent, &owner.owner_sid)?);
        let root = base.join(relative);
        owner.instance_id = Some(instance_id);
        handles.push(claim(&root, &owner)?);
        let paths = IsolatedPaths {
            user_data: root.join("user-data"),
            app_home: root.join("app-home"),
            root,
        };
        handles.push(directory(&paths.user_data, &owner.owner_sid)?);
        handles.push(directory(&paths.app_home, &owner.owner_sid)?);
        Ok(Some(PreparedData {
            paths,
            _directories: handles,
        }))
    }
}

fn directory(path: &Path, sid: &str) -> Result<OwnedHandle> {
    if !path.try_exists()? {
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

fn claim(path: &Path, expected: &DataOwner) -> Result<OwnedHandle> {
    let created = if !path.try_exists()? {
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
                .join("AppProxyRust")
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
