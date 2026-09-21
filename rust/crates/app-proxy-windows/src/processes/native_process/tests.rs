use super::*;
use std::{
    os::windows::process::CommandExt,
    process::{Child, Command, Stdio},
    time::Duration,
};

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "native command-line subprocess fixture"]
fn child() {
    std::thread::sleep(Duration::from_secs(60));
}

#[tokio::test]
async fn native_arguments_match_wmi_and_stop_retains_exact_object() {
    let args = [
        "--ignored",
        "--exact",
        "native_process::tests::child",
        "--skip",
        "--user-data-dir=C:\\space 中文\\profile",
        "--skip",
        "--proxy-server=http://127.0.0.1:12345",
    ];
    let child = ChildGuard(
        Command::new(std::env::current_exe().unwrap())
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut pinned = PinnedProcess::open(child.0.id()).unwrap();
    pinned.read_arguments().unwrap();
    let wmi = process_query::inspect(pinned.identity()).await.unwrap();
    assert_eq!(pinned.arguments(), wmi.arguments.as_deref());
    let mut wrong = pinned.identity().clone();
    wrong.creation_time += 1;
    assert!(matches!(
        pinned.terminate(&wrong),
        Err(Error::IdentityMismatch)
    ));
    pinned.verify().unwrap();
    assert_eq!(
        pinned.terminate(pinned.identity()).unwrap(),
        crate::process_stop::StopOutcome::Forced
    );
    assert!(matches!(
        pinned.verify(),
        Err(Error::Invalid("GUARD_TARGET_EXITED_BEFORE_STOP"))
    ));
}

#[test]
fn rejects_malformed_native_buffers_before_dereferencing() {
    let mut storage = vec![0usize; 16];
    let base = storage.as_mut_ptr();
    // SAFETY: aligned, sufficiently sized storage; no references alias the writes.
    unsafe {
        base.cast::<UnicodeString>().write(UnicodeString {
            length: 2,
            maximum_length: 2,
            buffer: base.cast::<u8>().add(size_of::<UnicodeString>()).cast(),
        });
    }
    assert_eq!(
        decode(&storage, std::mem::size_of_val(storage.as_slice())).unwrap(),
        vec![0]
    );
    assert!(decode(&storage, size_of::<UnicodeString>() - 1).is_err());
    assert!(decode(&storage, 9999).is_err());
    for pointer in [0, 1, base as usize - 2, base as usize + 1000] {
        // SAFETY: only change pointer bits in the aligned header; decode must reject them.
        unsafe {
            (*base.cast::<UnicodeString>()).buffer = pointer as *const u16;
        }
        assert!(decode(&storage, std::mem::size_of_val(storage.as_slice())).is_err());
    }
}
