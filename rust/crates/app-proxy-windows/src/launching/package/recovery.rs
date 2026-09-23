//! Failure-only cleanup of obsolete Claude package descendants. Never infer
//! ownership from an executable name, parent PID, path prefix or a file lock.
use super::Package;
use crate::{Error, Result, identity, process_query};
use app_proxy_core::ProcessIdentity;
use serde::Serialize;
use std::{
    os::windows::io::{AsRawHandle, OwnedHandle},
    time::Instant,
};
use windows_sys::Win32::{Foundation::*, System::Threading::*};

#[derive(Default, Serialize)]
pub(crate) struct Report {
    pub current_package: String,
    pub candidates: Vec<Candidate>,
    pub stopped: Vec<u32>,
    pub status: String,
}

#[derive(Serialize)]
pub(crate) struct Candidate {
    pub process: ProcessIdentity,
    pub package: String,
}

fn version(full_name: &str) -> Option<[u16; 4]> {
    let fields: Vec<_> = full_name.split('_').collect();
    if fields.len() != 5 || fields[0] != "Claude" || fields[4] != "pzs8sxrjxfjjc" {
        return None;
    }
    let parts: Vec<_> = fields[1].split('.').map(str::parse::<u16>).collect();
    if parts.len() != 4 {
        return None;
    }
    Some([
        *parts[0].as_ref().ok()?,
        *parts[1].as_ref().ok()?,
        *parts[2].as_ref().ok()?,
        *parts[3].as_ref().ok()?,
    ])
}

fn older(candidate: &str, current: &str) -> bool {
    match (version(candidate), version(current)) {
        (Some(old), Some(new)) => old < new,
        _ => false,
    }
}

fn same_owner(process: &ProcessIdentity, caller: &ProcessIdentity) -> bool {
    process.pid != caller.pid
        && process.user_sid == caller.user_sid
        && process.session_id == caller.session_id
}

fn budget(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        Err(Error::Invalid("PACKAGE_RECOVERY_TIMEOUT"))
    } else {
        Ok(())
    }
}

/// All candidates are pinned and checked before the first termination. No
/// elevation, broad taskkill, process-tree traversal, or retry loop is used.
pub(crate) fn recover(
    package: &Package,
    deadline: Instant,
    report: &mut Report,
    mut persist: impl FnMut(&Report) -> Result<()>,
) -> Result<usize> {
    identity::assert_ordinary_user()?;
    if package.family_name != "Claude_pzs8sxrjxfjjc"
        || package.app_id != "Claude"
        || version(&package.full_name).is_none()
    {
        return Err(Error::Invalid("PACKAGE_RECOVERY_UNSUPPORTED"));
    }
    let caller = identity::current()?;
    let mut handles = Vec::new();
    for hint in process_query::snapshot()? {
        budget(deadline)?;
        // System/protected processes cannot belong to this ordinary user's
        // recoverable set. Failure to inspect an identified old package aborts.
        let query = match identity::open(hint.pid, PROCESS_QUERY_LIMITED_INFORMATION) {
            Ok(handle) => handle,
            Err(error) if hint.executable_name.eq_ignore_ascii_case("claude.exe") => {
                return Err(error);
            }
            Err(_) => continue,
        };
        // SAFETY: the query handle stays alive throughout both identity reads.
        let full_name = unsafe { identity::package_full_name_handle(query.as_raw_handle()) };
        let full_name = match full_name {
            Ok(Some(name)) => name,
            Err(error) if hint.executable_name.eq_ignore_ascii_case("claude.exe") => {
                return Err(error);
            }
            _ => continue,
        };
        if !older(&full_name, &package.full_name) {
            continue;
        }
        // SAFETY: query owns the exact process being inspected.
        let observed = unsafe { identity::inspect_handle(query.as_raw_handle())? };
        if !same_owner(&observed, &caller) {
            continue;
        }
        // An old Claude window or renderer is still active: leave that entire
        // old package alone, even if some other descendants look orphaned.
        if observed
            .image_path
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("claude.exe"))
        {
            return Err(Error::Invalid("PACKAGE_OLD_VERSION_RUNNING"));
        }
        let handle = identity::open(
            hint.pid,
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
        )?;
        // SAFETY: compare on the retained termination handle to reject PID reuse.
        let matches = unsafe {
            identity::inspect_handle(handle.as_raw_handle())? == observed
                && identity::package_full_name_handle(handle.as_raw_handle())?.as_deref()
                    == Some(&full_name)
        };
        if !matches {
            return Err(Error::IdentityMismatch);
        }
        report.candidates.push(Candidate {
            process: observed,
            package: full_name,
        });
        handles.push(handle);
    }
    if handles.is_empty() {
        return Err(Error::Invalid("PACKAGE_STALE_PROCESSES_NOT_FOUND"));
    }
    // Registration can change again while collecting candidates. Never clean
    // against a cached version from a previous launch plan.
    budget(deadline)?;
    if super::resolve(&package.family_name, &package.app_id)?.full_name != package.full_name {
        return Err(Error::Invalid("PACKAGE_CHANGED"));
    }
    report.status = "prepared".into();
    persist(report)?;
    stop_pinned(&handles, deadline, report, &mut persist)?;
    Ok(report.stopped.len())
}

fn stop_pinned(
    handles: &[OwnedHandle],
    deadline: Instant,
    report: &mut Report,
    persist: &mut impl FnMut(&Report) -> Result<()>,
) -> Result<()> {
    for (handle, candidate) in handles.iter().zip(&report.candidates) {
        budget(deadline)?;
        // SAFETY: only the checked, retained process object can be terminated.
        unsafe {
            match WaitForSingleObject(handle.as_raw_handle(), 0) {
                WAIT_OBJECT_0 => continue,
                WAIT_TIMEOUT => {}
                _ => return Err(crate::last_error("ObserveStalePackageProcess")),
            }
            if identity::inspect_handle(handle.as_raw_handle())? != candidate.process
                || identity::package_full_name_handle(handle.as_raw_handle())?.as_deref()
                    != Some(&candidate.package)
            {
                return Err(Error::IdentityMismatch);
            }
            if TerminateProcess(handle.as_raw_handle(), 1) == 0 {
                return Err(crate::last_error("StopStalePackageProcess"));
            }
        }
        report.stopped.push(candidate.process.pid);
    }
    report.status = "stopping".into();
    persist(report)?;
    for handle in handles {
        let millis = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(2000) as u32;
        // SAFETY: every handle has SYNCHRONIZE rights and remains retained.
        match unsafe { WaitForSingleObject(handle.as_raw_handle(), millis) } {
            WAIT_OBJECT_0 => {}
            WAIT_TIMEOUT => return Err(Error::Invalid("PACKAGE_STALE_PROCESS_EXIT_TIMEOUT")),
            _ => return Err(crate::last_error("WaitStalePackageProcess")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "explicit live recovery: stops obsolete Claude package descendants after reproducing the activation conflict"]
    fn recover_live_claude_container_conflict() {
        let helper = std::path::PathBuf::from(
            std::env::var_os("APP_PROXY_RECOVERY_TEST_HOST")
                .expect("set the absolute path of a built app-proxy-host.exe"),
        );
        let log = std::path::PathBuf::from(
            std::env::var_os("APP_PROXY_RECOVERY_TEST_LOG")
                .expect("set an explicit recovery evidence output path"),
        );
        let package = crate::package::discover("claude").unwrap();
        let temp = tempfile::tempdir().unwrap();
        // No probe request exists: even if activation unexpectedly succeeds,
        // this helper cannot launch Claude or any application.
        let activation =
            crate::package::activate_probe(&package, &helper, &temp.path().join("request.json"));
        assert!(
            matches!(
                activation,
                Err(Error::Invalid("PACKAGE_CONTAINER_CONFLICT"))
            ),
            "no proven container conflict; leave all processes intact: {activation:?}"
        );
        let mut report = Report {
            current_package: package.full_name.clone(),
            ..Default::default()
        };
        let persist = |report: &Report| -> Result<()> {
            std::fs::write(&log, serde_json::to_vec_pretty(report)?)?;
            Ok(())
        };
        let result = recover(
            &package,
            Instant::now() + std::time::Duration::from_secs(3),
            &mut report,
            persist,
        );
        report.status = match &result {
            Ok(_) => "completed".into(),
            Err(e) => e.to_string(),
        };
        persist(&report).unwrap();
        let count = result.unwrap();
        assert!(count > 0);
        println!(
            "Recovered {count} obsolete package process(es); evidence: {}",
            log.display()
        );
        crate::package::activate_probe(&package, &helper, &temp.path().join("request.json"))
            .unwrap();
    }

    #[test]
    fn recovery_requires_strictly_older_exact_claude_package_identity() {
        let current = "Claude_2.2553.13.0_x64__pzs8sxrjxfjjc";
        assert!(older("Claude_2.2553.1.0_x64__pzs8sxrjxfjjc", current));
        for other in [
            current,
            "Claude_2.2553.14.0_x64__pzs8sxrjxfjjc",
            "Claude_2.2553.1.0_x64__other",
            "Other_2.2553.1.0_x64__pzs8sxrjxfjjc",
            "Claude_2.1_x64__pzs8sxrjxfjjc",
            "Claude_2.1.0.0_x64__pzs8sxrjxfjjc_extra",
        ] {
            assert!(!older(other, current), "{other}");
        }
        let caller = identity::current().unwrap();
        let mut process = caller.clone();
        assert!(!same_owner(&process, &caller));
        process.pid += 1;
        assert!(same_owner(&process, &caller));
        process.session_id += 1;
        assert!(!same_owner(&process, &caller));
        process.session_id = caller.session_id;
        process.user_sid.push_str("-other");
        assert!(!same_owner(&process, &caller));
    }
}
