use super::*;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use windows_sys::Win32::{Foundation::HWND, UI::WindowsAndMessaging::*};

fn text(window: HWND) -> String {
    let mut buffer = vec![0u16; 4096];
    // SAFETY: bounded writable buffer; queried HWND is validated by Windows.
    let length = unsafe { GetWindowTextW(window, buffer.as_mut_ptr(), buffer.len() as i32) };
    String::from_utf16_lossy(&buffer[..length.max(0) as usize])
}
unsafe extern "system" fn own_dialog(window: HWND, state: isize) -> i32 {
    let mut pid = 0;
    // SAFETY: writable PID and synchronous callback context from EnumWindows.
    unsafe {
        GetWindowThreadProcessId(window, &mut pid);
        if pid == std::process::id()
            && IsWindowVisible(window) != 0
            && text(window) == "App Proxy：启动未完成"
        {
            *(state as *mut HWND) = window;
            return 0;
        }
    }
    1
}
unsafe extern "system" fn child_text(window: HWND, state: isize) -> i32 {
    // SAFETY: synchronous callback with a live Vec<String> context.
    unsafe {
        (&mut *(state as *mut Vec<String>)).push(text(window));
    }
    1
}

#[test]
#[ignore = "native dialog child, invoked only by notification_contract"]
fn notification_child() {
    let output =
        PathBuf::from(std::env::var_os("APP_PROXY_DIALOG_TEST_OUTPUT").expect("fixture output"));
    let launch = uuid::Uuid::new_v4();
    let core = uuid::Uuid::new_v4();
    let message =
        format!("结果未确认，应用会保留。\n启动请求：{launch}\n代理操作：{core}\n请先查询原编号。");
    let observer = std::thread::spawn(|| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let mut window: HWND = std::ptr::null_mut();
            // SAFETY: local callback context, filters strictly to this fixture PID.
            unsafe {
                EnumWindows(Some(own_dialog), (&mut window as *mut HWND) as isize);
            }
            if !window.is_null() {
                // Allow the system dialog's initialization/activation to finish
                // before exercising its actual confirmation button.
                std::thread::sleep(Duration::from_millis(250));
                let mut labels: Vec<String> = Vec::new();
                // SAFETY: read controls and close only our own fixture dialog.
                unsafe {
                    EnumChildWindows(
                        window,
                        Some(child_text),
                        (&mut labels as *mut Vec<String>) as isize,
                    );
                    // Windows may give the sole MB_OK button IDCANCEL. Exercise
                    // the actual Button control instead of assuming its ID.
                    let class = crate::wide(std::ffi::OsStr::new("Button")).unwrap();
                    let button = FindWindowExW(
                        window,
                        std::ptr::null_mut(),
                        class.as_ptr(),
                        std::ptr::null(),
                    );
                    assert!(!button.is_null());
                    assert_ne!(PostMessageW(button, BM_CLICK, 0, 0), 0);
                }
                return labels.join("\n");
            }
            assert!(
                Instant::now() < deadline,
                "native notification did not appear"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    notify_launch_failure(&message).unwrap();
    let shown = observer.join().unwrap();
    assert!(shown.contains(&launch.to_string()) && shown.contains(&core.to_string()));
    assert!(shown.contains("请先查询原编号"));
    std::fs::write(
        output,
        b"native notification displayed both request identifiers",
    )
    .unwrap();
}

#[test]
#[ignore = "opens and closes an owned native notification; explicit desktop validation"]
fn notification_contract() {
    use std::os::windows::process::CommandExt;
    let root = tempfile::tempdir().unwrap();
    let result = root.path().join("result.txt");
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "shell::console::tests::notification_child",
            "--nocapture",
        ])
        .env("APP_PROXY_DIALOG_TEST_OUTPUT", &result)
        .creation_flags(0x00000008)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let output = child.wait_with_output().unwrap();
            assert!(
                status.success(),
                "{status:?}\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!(
                "notification fixture timed out\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        std::fs::read(result).unwrap(),
        b"native notification displayed both request identifiers"
    );
}
