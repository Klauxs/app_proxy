use super::*;
use std::cell::Cell;

fn fixture(root: &Path) -> (Fingerprint, Package) {
    let exe = root.join("fixture.exe");
    std::fs::write(&exe, b"not executed").unwrap();
    (
        Fingerprint {
            full_name: "Fixture_1".into(),
            directory: root.into(),
            manifest: [1; 32],
        },
        Package {
            family_name: "Fixture_family".into(),
            full_name: "Fixture_1".into(),
            app_id: "App".into(),
            exe,
            isolated_storage: true,
        },
    )
}

#[test]
fn metadata_cache_rechecks_registration_manifest_and_missing_executable() {
    let root = tempfile::tempdir().unwrap();
    let (mut fp, pkg) = fixture(root.path());
    let cache = Mutex::new(VecDeque::new());
    let parses = Cell::new(0);
    let parse = || {
        parses.set(parses.get() + 1);
        Ok(pkg.clone())
    };
    for _ in 0..2 {
        resolve_with(
            &cache,
            "sid",
            "Fixture_family",
            "App",
            || Ok(fp.clone()),
            parse,
        )
        .unwrap();
    }
    assert_eq!(parses.get(), 1);
    fp.manifest[0] = 2;
    resolve_with(
        &cache,
        "sid",
        "Fixture_family",
        "App",
        || Ok(fp.clone()),
        parse,
    )
    .unwrap();
    assert_eq!(parses.get(), 2);
    for code in ["APP_NOT_INSTALLED", "AMBIGUOUS_PACKAGE"] {
        assert!(
            matches!(resolve_with(&cache, "sid", "Fixture_family", "App", || Err(Error::Invalid(code)), parse), Err(Error::Invalid(c)) if c == code)
        );
    }
    std::fs::remove_file(pkg.exe).unwrap();
    assert!(matches!(
        resolve_with(
            &cache,
            "sid",
            "Fixture_family",
            "App",
            || Ok(fp.clone()),
            || panic!("must not parse cached missing executable")
        ),
        Err(Error::Invalid(
            app_proxy_core::error_code::APP_NOT_INSTALLED
        ))
    ));
}

#[test]
fn metadata_cache_rejects_package_update_during_parse_and_scopes_user_and_app() {
    let root = tempfile::tempdir().unwrap();
    let (fp, pkg) = fixture(root.path());
    let cache = Mutex::new(VecDeque::new());
    let calls = Cell::new(0);
    let result = resolve_with(
        &cache,
        "sid",
        "Fixture_family",
        "App",
        || {
            calls.set(calls.get() + 1);
            let mut value = fp.clone();
            if calls.get() > 1 {
                value.full_name = "Fixture_2".into();
            }
            Ok(value)
        },
        || Ok(pkg.clone()),
    );
    assert!(matches!(result, Err(Error::Invalid("PACKAGE_CHANGED"))));
    assert!(cache.lock().unwrap().is_empty());
    for (sid, app) in [("sid", "App"), ("other", "App"), ("sid", "Other")] {
        let mut package = pkg.clone();
        package.app_id = app.into();
        resolve_with(
            &cache,
            sid,
            "Fixture_family",
            app,
            || Ok(fp.clone()),
            || Ok(package),
        )
        .unwrap();
    }
    assert_eq!(cache.lock().unwrap().len(), 3);
    assert!(matches!(
        resolve_with(
            &cache,
            "new",
            "Fixture_family",
            "App",
            || Ok(fp.clone()),
            || {
                let mut wrong = pkg.clone();
                wrong.full_name = "wrong".into();
                Ok(wrong)
            }
        ),
        Err(Error::Invalid("PACKAGE_CHANGED"))
    ));
}

#[test]
fn metadata_cache_is_bounded_and_path_changes_invalidate() {
    let root = tempfile::tempdir().unwrap();
    let (mut fp, pkg) = fixture(root.path());
    let cache = Mutex::new(VecDeque::new());
    for index in 0..CACHE_LIMIT + 1 {
        resolve_with(
            &cache,
            &index.to_string(),
            "Fixture_family",
            "App",
            || Ok(fp.clone()),
            || Ok(pkg.clone()),
        )
        .unwrap();
    }
    assert_eq!(cache.lock().unwrap().len(), CACHE_LIMIT);
    fp.directory = root.path().join("replacement");
    assert!(matches!(
        resolve_with(
            &cache,
            "1",
            "Fixture_family",
            "App",
            || Ok(fp.clone()),
            || Err(Error::Invalid("parse required"))
        ),
        Err(Error::Invalid("parse required"))
    ));
}

#[test]
#[ignore = "read-only comparison against installed Codex and Claude packages"]
fn native_lookup_matches_bridge_for_installed_apps_and_reports_warm_latency() {
    for (family, app) in [
        ("OpenAI.Codex_2p2nqsd0c76g0", "App"),
        ("Claude_pzs8sxrjxfjjc", "Claude"),
    ] {
        let started = Instant::now();
        let fresh: Package = super::super::bridge(
            &serde_json::json!({"operation":"discover", "family_name":family, "app_id":app}),
        )
        .unwrap();
        let bridge_ms = started.elapsed().as_secs_f64() * 1000.0;
        resolve(family, app).unwrap();
        let started = Instant::now();
        let warm = resolve(family, app).unwrap();
        eprintln!(
            "{family}: bridge_ms={bridge_ms:.3} warm_native_ms={:.3}",
            started.elapsed().as_secs_f64() * 1000.0
        );
        assert_eq!(
            serde_json::to_value(fresh).unwrap(),
            serde_json::to_value(warm).unwrap()
        );
    }
}
