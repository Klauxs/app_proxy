use super::*;
use crate::process::{self, CreationMode, SpawnSpec, StartedProcess};
use app_proxy_core::EnvPatch;
use std::sync::mpsc;

#[test]
fn dmtf_uses_microseconds_offsets_and_rejects_partial_or_invalid_dates() {
    let parse = |s: &str| dmtf_creation_time(&s.encode_utf16().collect::<Vec<_>>());
    let utc = parse("20260920123045.123456+000").unwrap();
    assert_eq!(utc % 10_000_000, 1_234_560);
    assert_eq!(utc, parse("20260920203045.123456+480").unwrap());
    assert_eq!(utc, parse("20260920053045.123456-420").unwrap());
    assert_eq!(utc + 10, parse("20260920123045.123457+000").unwrap());
    for bad in [
        "",
        "20260920123045.******+000",
        "20260920123045.123456+***",
        "20260229123045.123456+000",
        "20260920123045.123456:000",
        "20260920123045.123456+000x",
        "16010101000000.000000+001",
        "20260920243045.123456+000",
    ] {
        assert!(parse(bad).is_err(), "invalid timestamp accepted");
    }
}

#[test]
fn windows_arguments_preserve_quotes_spaces_empty_unicode_and_backslashes() {
    let parsed = parse_arguments(&r#""C:\应用\app.exe" "" "two words" "literal\"quote" "C:\trailing\\" --user-data-dir="C:\分身 1""#.encode_utf16().collect::<Vec<_>>()).unwrap().unwrap();
    let expected: Vec<OsString> = [
        r"C:\应用\app.exe",
        "",
        "two words",
        "literal\"quote",
        "C:\\trailing\\",
        "--user-data-dir=C:\\分身 1",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    assert_eq!(parsed, expected);
    assert!(parse_arguments(&[]).unwrap().is_none());
    assert!(parse_arguments(&[0]).is_err());
    assert!(parse_arguments(&vec![b'a' as u16; 32768]).is_err());
    assert!(parse_arguments(&"  --something".encode_utf16().collect::<Vec<_>>()).is_err());
}

#[tokio::test]
async fn timeout_and_cancel_keep_native_work_bounded_until_it_actually_finishes() {
    static SLOT: AtomicBool = AtomicBool::new(false);
    let (release, wait) = mpsc::channel();
    let result = query_with(&SLOT, Duration::from_millis(25), move |_| {
        wait.recv().unwrap();
        Ok(())
    })
    .await;
    assert!(matches!(
        result,
        Err(Error::Invalid("PROCESS_QUERY_TIMEOUT"))
    ));
    assert!(matches!(
        query_with(&SLOT, QUERY_BUDGET, |_| Ok(())).await,
        Err(Error::Invalid("PROCESS_QUERY_BUSY"))
    ));
    release.send(()).unwrap();
    wait_free(&SLOT).await;
    let (entered, entry) = tokio::sync::oneshot::channel();
    let (release, wait) = mpsc::channel();
    let task = tokio::spawn(query_with(&SLOT, QUERY_BUDGET, move |_| {
        let _ = entered.send(());
        wait.recv().unwrap();
        Ok(())
    }));
    entry.await.unwrap();
    task.abort();
    let _ = task.await;
    assert!(matches!(
        query_with(&SLOT, QUERY_BUDGET, |_| Ok(())).await,
        Err(Error::Invalid("PROCESS_QUERY_BUSY"))
    ));
    release.send(()).unwrap();
    wait_free(&SLOT).await;
    assert_eq!(query_with(&SLOT, QUERY_BUDGET, |_| Ok(7)).await.unwrap(), 7);
}

async fn wait_free(slot: &AtomicBool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while slot.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn worker_failure_releases_slot_without_fabricating_an_observation() {
    static SLOT: AtomicBool = AtomicBool::new(false);
    let result = query_with::<()>(&SLOT, QUERY_BUDGET, |_| {
        panic!("fixture worker interruption")
    })
    .await;
    assert!(matches!(
        result,
        Err(Error::Invalid("PROCESS_QUERY_INTERRUPTED"))
    ));
    wait_free(&SLOT).await;
    assert!(query_with(&SLOT, QUERY_BUDGET, |_| Ok(())).await.is_ok());
}

struct Child(StartedProcess);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.terminate();
    }
}

#[tokio::test]
async fn older_package_image_with_same_name_is_never_treated_as_vacant() {
    let _query = QUERY_TEST_LOCK.lock().await;
    let temp = tempfile::tempdir().unwrap();
    let current = identity::current().unwrap();
    let name = current.image_path.file_name().unwrap().to_owned();
    let newer = temp.path().join(&name);
    std::fs::copy(&current.image_path, &newer).unwrap();
    let new_image = identity::file_identity(&newer).unwrap();
    assert_ne!(new_image, current.image_file);
    let package = candidates_for(new_image.clone(), name.clone(), true)
        .await
        .unwrap();
    assert!(package.iter().any(|p| p == &current));
    let plain = candidates_for(new_image, name, false).await.unwrap();
    assert!(!plain.iter().any(|p| p == &current));
}

#[tokio::test]
async fn native_wmi_observation_is_bound_to_exact_child_and_never_stops_it() {
    let _query = QUERY_TEST_LOCK.lock().await;
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("ready");
    let exe = std::env::current_exe().unwrap();
    let words = [
        "--ignored",
        "--exact",
        "process_query::tests::query_child",
        "--nocapture",
        "--skip",
        "fixture private value \"引号\" C:\\folder\\",
    ];
    let mut environment = EnvPatch::default();
    environment.set.insert(
        "APP_PROXY_QUERY_FIXTURE".into(),
        marker.to_str().unwrap().into(),
    );
    let mut child = Child(
        process::spawn(SpawnSpec {
            exe: exe.clone(),
            args: words.iter().map(Into::into).collect(),
            cwd: temp.path().to_owned(),
            environment,
            mode: CreationMode::Normal,
        })
        .unwrap(),
    );
    let until = Instant::now() + Duration::from_secs(3);
    while !marker.exists() {
        assert!(Instant::now() < until, "child did not become ready");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let native = snapshot().unwrap();
    let hint = native
        .iter()
        .find(|p| p.pid == child.0.identity.pid)
        .unwrap();
    assert_eq!(hint.parent_pid, std::process::id());
    assert_eq!(hint.executable_name, exe.file_name().unwrap());
    let observed = inspect(&child.0.identity).await.unwrap();
    assert_eq!(observed.identity, child.0.identity);
    assert_eq!(observed.parent_pid, std::process::id());
    let actual = observed.arguments.unwrap();
    assert_eq!(actual[0], exe.as_os_str());
    let expected: Vec<OsString> = words.iter().map(Into::into).collect();
    assert!(actual[1..] == expected, "argument roundtrip mismatch");
    assert!(process::is_running_exact(&child.0.identity).unwrap());
    wait_free(&QUERY_BUSY).await;
    for change in 0..4 {
        let mut forged = child.0.identity.clone();
        match change {
            0 => forged.creation_time += 10,
            1 => forged.user_sid.push('x'),
            2 => forged.session_id += 1,
            _ => forged.image_file.file_index += 1,
        }
        assert!(matches!(
            inspect(&forged).await,
            Err(Error::IdentityMismatch)
        ));
        wait_free(&QUERY_BUSY).await;
    }
    let child_identity = child.0.identity.clone();
    let during_finish = inspect_with(&child.0.identity, move |_| {
        process::terminate_exact(&child_identity)?;
        Ok(())
    })
    .await;
    assert!(matches!(
        during_finish,
        Err(Error::Invalid("PROCESS_EXITED_DURING_INSPECTION"))
    ));
    child.0.terminate().unwrap();
    assert!(inspect(&child.0.identity).await.is_err());
    wait_free(&QUERY_BUSY).await;
    let missing = std::thread::spawn(|| wmi_row(u32::MAX, Instant::now() + QUERY_BUDGET))
        .join()
        .unwrap();
    assert!(matches!(
        missing,
        Err(Error::Invalid("PROCESS_QUERY_NOT_FOUND"))
    ));
}

#[test]
#[ignore = "subprocess fixture invoked by native WMI parent test"]
fn query_child() {
    std::fs::write(
        std::env::var_os("APP_PROXY_QUERY_FIXTURE").unwrap(),
        b"ready",
    )
    .unwrap();
    std::thread::sleep(Duration::from_secs(30));
}
