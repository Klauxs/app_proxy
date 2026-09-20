use super::*;
use crate::process::{self, SpawnSpec, StartedProcess};
use app_proxy_core::EnvPatch;
use std::{fs, path::PathBuf};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

struct Fixture {
    child: StartedProcess,
    root: tempfile::TempDir,
}
impl Fixture {
    fn new(mode: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut environment = EnvPatch::default();
        environment.set.insert(
            "APP_PROXY_STOP_ROOT".into(),
            root.path().to_str().unwrap().into(),
        );
        environment
            .set
            .insert("APP_PROXY_STOP_MODE".into(), mode.into());
        let mut child = process::spawn(SpawnSpec {
            exe: std::env::current_exe().unwrap(),
            args: ["--ignored", "--exact", "process_stop::tests::window_child"]
                .map(Into::into)
                .to_vec(),
            cwd: root.path().into(),
            environment,
        })
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !root.path().join("ready").exists() {
            assert!(child.try_wait().unwrap().is_none());
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        Self { child, root }
    }
    fn closed(&self) -> bool {
        self.root.path().join("closed").exists()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.child.terminate();
    }
}

#[test]
fn graceful_stop_verifies_identity_and_only_closes_the_target_windows() {
    let target = Fixture::new("close");
    let unrelated = Fixture::new("close");
    for field in ["sid", "session", "image"] {
        let mut forged = target.child.identity.clone();
        match field {
            "sid" => forged.user_sid.push_str("-1"),
            "session" => forged.session_id += 1,
            _ => forged.image_file.file_index += 1,
        }
        assert!(matches!(
            stop_exact(&forged, true),
            Err(Error::IdentityMismatch)
        ));
        assert!(!target.closed());
        assert!(process::is_running_exact(&target.child.identity).unwrap());
    }
    let mut reused = target.child.identity.clone();
    reused.creation_time -= 1;
    assert_eq!(
        stop_exact(&reused, true).unwrap(),
        StopOutcome::AlreadyExited
    );
    assert!(!target.closed());
    assert_eq!(
        stop_exact(&target.child.identity, false).unwrap(),
        StopOutcome::Exited
    );
    assert!(target.closed());
    assert!(!unrelated.closed());
    assert!(process::is_running_exact(&unrelated.child.identity).unwrap());
    assert_eq!(
        stop_exact(&target.child.identity, true).unwrap(),
        StopOutcome::AlreadyExited
    );
    assert!(stop_exact(&identity::current().unwrap(), true).is_err());
}

#[test]
fn immediate_guard_policy_skips_window_close_and_preserves_other_processes() {
    let target = Fixture::new("ignore");
    let unrelated = Fixture::new("close");
    for field in ["sid", "session", "image"] {
        let mut forged = target.child.identity.clone();
        match field {
            "sid" => forged.user_sid.push_str("-1"),
            "session" => forged.session_id += 1,
            _ => forged.image_file.file_index += 1,
        }
        assert!(matches!(
            stop_immediately(&forged),
            Err(Error::IdentityMismatch)
        ));
        assert!(process::is_running_exact(&target.child.identity).unwrap());
    }
    let mut reused = target.child.identity.clone();
    reused.creation_time -= 1;
    assert_eq!(
        stop_immediately(&reused).unwrap(),
        StopOutcome::AlreadyExited
    );
    assert!(process::is_running_exact(&target.child.identity).unwrap());
    assert_eq!(
        stop_immediately(&target.child.identity).unwrap(),
        StopOutcome::Forced
    );
    assert!(!target.closed()); // no WM_CLOSE or graceful-exit wait on Guard path
    assert!(!process::is_running_exact(&target.child.identity).unwrap());
    assert!(process::is_running_exact(&unrelated.child.identity).unwrap());
    assert!(!unrelated.closed());
    assert_eq!(
        stop_immediately(&target.child.identity).unwrap(),
        StopOutcome::AlreadyExited
    );
    assert!(stop_immediately(&identity::current().unwrap()).is_err());
}

#[test]
fn ignored_close_preserves_process_until_force_is_explicit() {
    let target = Fixture::new("ignore");
    assert_eq!(
        stop_exact(&target.child.identity, false).unwrap(),
        StopOutcome::StillRunning
    );
    assert!(target.closed()); // receipt means WM_CLOSE was handled, not exit
    assert!(process::is_running_exact(&target.child.identity).unwrap());
    assert_eq!(
        stop_exact(&target.child.identity, true).unwrap(),
        StopOutcome::Forced
    );
    assert!(!process::is_running_exact(&target.child.identity).unwrap());
}

#[test]
fn hung_window_cannot_block_stop_indefinitely() {
    let target = Fixture::new("hung");
    let start = Instant::now();
    assert_eq!(
        stop_exact(&target.child.identity, true).unwrap(),
        StopOutcome::Forced
    );
    assert!(start.elapsed() < Duration::from_secs(7));
    assert!(!target.closed());
}

unsafe extern "system" fn window_proc(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if message == WM_CLOSE {
        if let Some(root) = std::env::var_os("APP_PROXY_STOP_ROOT") {
            let _ = fs::write(PathBuf::from(root).join("closed"), b"received");
        }
        // SAFETY: only this fixture creates the window and writes its user data.
        if unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } == 1 {
            return 0;
        }
    }
    if message == WM_DESTROY {
        // SAFETY: this fixture's own message loop receives the quit notification.
        unsafe {
            PostQuitMessage(0);
        }
        return 0;
    }
    // SAFETY: forward unhandled messages with their original parameters.
    unsafe { DefWindowProcW(window, message, w, l) }
}

#[test]
#[ignore = "native hidden-window fixture invoked by parent tests"]
fn window_child() {
    let root = PathBuf::from(std::env::var_os("APP_PROXY_STOP_ROOT").unwrap());
    let mode = std::env::var("APP_PROXY_STOP_MODE").unwrap();
    let name = crate::wide(std::ffi::OsStr::new("AppProxyRustStopFixture")).unwrap();
    // SAFETY: class/name/instance live through the entire native window loop;
    // the window is an invisible fixture, never a user application window.
    unsafe {
        let module = GetModuleHandleW(std::ptr::null());
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: module,
            lpszClassName: name.as_ptr(),
            ..std::mem::zeroed()
        };
        assert_ne!(RegisterClassW(&class), 0);
        let window = CreateWindowExW(
            0,
            name.as_ptr(),
            name.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            100,
            100,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            module,
            std::ptr::null(),
        );
        assert!(!window.is_null());
        if mode == "ignore" {
            SetWindowLongPtrW(window, GWLP_USERDATA, 1);
        }
        fs::write(root.join("ready"), b"ready").unwrap();
        if mode == "hung" {
            std::thread::sleep(Duration::from_secs(30));
            return;
        }
        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}
