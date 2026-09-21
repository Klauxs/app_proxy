//! Stable error codes shared by every crate and by the coordinator protocol.
//!
//! Codes are UPPER_SNAKE ASCII. In-process errors carry them as `&'static str`;
//! the wire carries them as text. [`intern`] is the only bridge from wire text
//! back to a static code, so a code added on the server reaches the client's
//! `match` arms without a second hand-maintained list.
//!
//! [`certainty`] is the single place that decides whether a failed operation may
//! have taken effect. It reads the explicit registry below, never the spelling
//! of a code, so renaming a code cannot silently turn "unknown" into "failed".
//! A source scan in this module's tests keeps the registry and the naming
//! convention (`UNKNOWN` / `UNCONFIRMED` / `INDETERMINATE`) consistent.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

/// Declares each code as a constant whose value is its own name.
macro_rules! named {
    ($($name:ident),* $(,)?) => {
        $(pub const $name: &str = stringify!($name);)*
        /// Every code declared above.
        pub const NAMED: &[&str] = &[$($name),*];
    };
}

// Codes that some caller branches on. Producers and callers both use these
// constants, so renaming one is a compile error instead of a dead branch.
// Codes that are only reported stay plain literals where they are raised.
named! {
    APP_NOT_INSTALLED,
    GUARD_INSTALL_CANCELLED,
    GUARD_LISTENER_MISSING,
    GUARD_LOGIN_AUTHORIZATION_REQUIRED,
    GUARD_STOPPED_RECORD_FAILED,
    GUARD_TASK_MISSING,
    INSTANCE_EXTERNALLY_RUNNING,
    INSTANCE_RESOURCE_BUSY,
    INVALID_INSTANCE_EDIT_FILE,
    IPC_CONNECT_TIMEOUT,
    PACKAGE_REQUEST_BUSY,
    PACKAGE_RESULT_UNKNOWN,
    PROCESS_QUERY_BUSY,
    STORE_ALREADY_OWNED,
    STORE_NOT_EMPTY,
    SUBSCRIPTION_PREVIEW_EXPIRED,
    SUBSCRIPTION_STAGE_PENDING,
}

/// Longest code accepted from the wire.
pub const MAX_LEN: usize = 96;
/// Bounds memory retained for codes first seen on the wire.
const MAX_INTERNED: usize = 1024;

/// Whether a failed operation is known not to have taken effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Certainty {
    /// The operation did not take effect; retrying after a fix is safe.
    Definite,
    /// The operation may have taken effect; query the original request instead
    /// of retrying.
    Indeterminate,
}

/// Every code whose operation may have taken effect. Sorted; see the tests.
pub const INDETERMINATE: &[&str] = &[
    "COORDINATOR_IDENTITY_UNCONFIRMED",
    "CORE_CANCEL_RESULT_UNKNOWN",
    "CORE_COMMIT_STATE_UNKNOWN",
    "CORE_INSTALL_PUBLICATION_UNCONFIRMED",
    "CORE_JOB_MEMBERS_UNCONFIRMED",
    "CORE_JOB_PROCESS_UNCONFIRMED",
    "CORE_LISTENER_OWNER_UNCONFIRMED",
    "CORE_OPERATION_RESULT_UNKNOWN",
    "CORE_PROCESS_IDENTITY_UNCONFIRMED",
    "CORE_RESTORE_STATE_UNKNOWN",
    "CORE_START_RESULT_UNKNOWN",
    "CORE_STOP_UNCONFIRMED",
    "CORE_UPDATE_RESULT_UNKNOWN",
    "CORE_UPDATE_START_UNKNOWN",
    "GUARD_INSTALL_RESULT_UNKNOWN",
    "GUARD_LAUNCH_INDETERMINATE",
    "GUARD_LISTENER_CHECK_UNCONFIRMED",
    "GUARD_LOGIN_CHECK_UNCONFIRMED",
    "GUARD_LOGIN_OPERATION_UNCONFIRMED",
    "GUARD_LOGIN_REMOVE_UNCONFIRMED",
    "GUARD_PROXY_ARGUMENTS_UNKNOWN",
    "GUARD_SCAN_UNCONFIRMED",
    "GUARD_STATUS_UNCONFIRMED",
    "GUARD_TASK_INSTANCE_UNKNOWN",
    "GUARD_TASK_REMOVE_UNCONFIRMED",
    "ICON_CACHE_PUBLISH_UNCONFIRMED",
    "INSTANCE_PROCESS_UNKNOWN",
    "LAUNCH_INDETERMINATE",
    "PACKAGE_CHILD_IDENTITY_UNCONFIRMED",
    "PACKAGE_OPERATION_TIMEOUT_RESULT_UNKNOWN",
    "PACKAGE_RESULT_UNKNOWN",
    "PROBE_STOP_UNCONFIRMED",
    "PROCESS_STOP_UNCONFIRMED",
    "RESOURCE_SPAWN_RESULT_UNKNOWN",
    "SHORTCUT_PUBLISH_UNCONFIRMED",
    "SHORTCUT_REMOVE_UNCONFIRMED",
    "SPAWN_THREAD_PANICKED_RESULT_UNKNOWN",
    // "Unknown" below describes unrecognised input. These stay conservative
    // because they can surface while a started operation is being recorded.
    "UNKNOWN_APPLICATION",
    "UNKNOWN_CORE_REQUEST_FILE",
    "UNKNOWN_PATH_VARIABLE",
    "UNKNOWN_REQUEST_FILE",
];

/// Classifies a code. Codes absent from [`INDETERMINATE`] are definite.
pub fn certainty(code: &str) -> Certainty {
    if INDETERMINATE.binary_search(&code).is_ok() {
        Certainty::Indeterminate
    } else {
        Certainty::Definite
    }
}

/// True for text that has the shape of a code; says nothing about its meaning.
pub fn well_formed(code: &str) -> bool {
    let bytes = code.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_LEN
        && bytes[0].is_ascii_uppercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || *b == b'_')
}

/// Returns the static form of a code received as text.
///
/// `None` means the text is not a well-formed code, or the process has already
/// retained [`MAX_INTERNED`] distinct codes. Each distinct code is retained
/// once for the life of the process; the shape check and the cap bound that.
pub fn intern(code: &str) -> Option<&'static str> {
    if !well_formed(code) {
        return None;
    }
    if let Ok(index) = INDETERMINATE.binary_search(&code) {
        return Some(INDETERMINATE[index]);
    }
    static SEEN: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut seen = SEEN
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(known) = seen.get(code) {
        return Some(known);
    }
    if seen.len() >= MAX_INTERNED {
        return None;
    }
    let retained: &'static str = Box::leak(code.to_owned().into_boxed_str());
    seen.insert(retained);
    Some(retained)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::Path;

    /// Spellings that mark a code as "result not known" by convention.
    const MARKERS: [&str; 3] = ["UNKNOWN", "UNCONFIRMED", "INDETERMINATE"];

    #[test]
    fn registry_is_sorted_unique_and_well_formed() {
        assert!(INDETERMINATE.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(INDETERMINATE.iter().all(|code| well_formed(code)));
    }

    #[test]
    fn named_codes_equal_their_names_and_follow_the_registry() {
        assert!(NAMED.iter().all(|code| well_formed(code)));
        assert_eq!(PACKAGE_RESULT_UNKNOWN, "PACKAGE_RESULT_UNKNOWN");
        // The source scan below skips this file, so check named codes here.
        for code in NAMED {
            if MARKERS.iter().any(|marker| code.contains(marker)) {
                assert_eq!(certainty(code), Certainty::Indeterminate, "{code}");
            }
        }
    }

    #[test]
    fn classification_is_by_registry_not_spelling() {
        assert_eq!(certainty("CORE_STOP_UNCONFIRMED"), Certainty::Indeterminate);
        assert_eq!(certainty("LAUNCH_INDETERMINATE"), Certainty::Indeterminate);
        assert_eq!(certainty("CORE_DOWNLOAD_FAILED"), Certainty::Definite);
        // Not registered, so its spelling alone must not classify it.
        assert_eq!(certainty("SOMETHING_UNKNOWN_ELSE"), Certainty::Definite);
    }

    #[test]
    fn intern_returns_one_static_copy_and_rejects_other_text() {
        let first = intern("EXAMPLE_WIRE_CODE_7").unwrap();
        let second = intern(&String::from("EXAMPLE_WIRE_CODE_7")).unwrap();
        assert!(std::ptr::eq(first, second));
        for text in ["", "lower_case", "WITH SPACE", "_LEADING", "7DIGIT", "A:B"] {
            assert_eq!(intern(text), None, "{text}");
        }
        assert_eq!(intern(&"A".repeat(MAX_LEN + 1)), None);
    }

    fn literals(dir: &Path, found: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                literals(&path, found);
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            // Test sources use throwaway codes; this file holds the registry.
            if !name.ends_with(".rs") || name.contains("tests") || name == "error_code.rs" {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            // Inline `mod tests { .. }` blocks sit at the end of a file by
            // convention; a `mod tests;` declaration may appear anywhere.
            let text = text.replace("\r\n", "\n");
            let production = text.split("#[cfg(test)]\nmod tests {").next().unwrap();
            // A code literal is a quote, a run of code characters, and a quote.
            // Scanning from every quote is immune to escapes and char literals.
            for (start, _) in production.match_indices('"') {
                let rest = &production[start + 1..];
                if let Some(end) =
                    rest.find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
                    && rest[end..].starts_with('"')
                    && well_formed(&rest[..end])
                {
                    found.insert(rest[..end].to_owned());
                }
            }
        }
    }

    /// Guards both directions of drift: a code spelled as "result unknown" must
    /// be registered, and a registered code must still exist in the sources.
    #[test]
    fn registry_matches_the_codes_used_by_every_crate() {
        let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let mut found = BTreeSet::new();
        for name in [
            "app-proxy-core",
            "app-proxy-windows",
            "app-proxy-app",
            "app-proxy-setup",
        ] {
            literals(&crates.join(name).join("src"), &mut found);
        }
        let unregistered: Vec<_> = found
            .iter()
            .filter(|code| {
                MARKERS.iter().any(|marker| code.contains(marker))
                    && !MARKERS.contains(&code.as_str())
                    && certainty(code) == Certainty::Definite
            })
            .collect();
        assert!(
            unregistered.is_empty(),
            "register in INDETERMINATE: {unregistered:?}"
        );
        let stale: Vec<_> = INDETERMINATE
            .iter()
            .filter(|code| !found.contains(**code) && !NAMED.contains(code))
            .collect();
        assert!(stale.is_empty(), "no longer used anywhere: {stale:?}");
    }
}
