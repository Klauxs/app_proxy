//! Process exit codes and the CLI failure type. The categories follow
//! docs/06-product-and-protocol.md section 5; scripts rely on the numbers.
//!
//! Commands report failures with [`fail`] and one of the constants below.
//! [`for_code`] is the single table from stable error codes to categories.

/// Input, identifiers or references are invalid.
pub const INVALID: i32 = 2;
/// A dependency, installation, proxy or the coordinator is unavailable.
pub const UNAVAILABLE: i32 = 3;
/// Running with another configuration, or the configuration changed meanwhile.
pub const CONFLICT: i32 = 4;
/// A foreground decision or authorization is still required; nothing was retried.
pub const ACTION_REQUIRED: i32 = 5;
/// The result is unknown or still pending; query the original request ID.
pub const UNCONFIRMED: i32 = 6;
/// Internal failure or damaged storage.
pub const INTERNAL: i32 = 10;

#[derive(Debug)]
pub struct Failure {
    pub exit_code: i32,
    message: String,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Failure {}

pub(crate) fn fail(exit_code: i32, message: impl Into<String>) -> Failure {
    Failure {
        exit_code,
        message: message.into(),
    }
}

/// Category of a stable error code, or `None` when the code has no fixed
/// category and the calling command's own default applies.
pub fn for_code(code: &str) -> Option<i32> {
    Some(match code {
        "INVALID_LAUNCH_REQUEST"
        | "INVALID_REQUEST_ID"
        | "INSTANCE_NOT_FOUND"
        | "REQUEST_ID_CONFLICT"
        | "LAUNCH_ATTEMPT_NOT_FOUND" => INVALID,
        "APP_NOT_INSTALLED"
        | "INSTALLATION_CHECK_FAILED"
        | "INSTALLATION_ACCESS_DENIED"
        | "AMBIGUOUS_PACKAGE" => UNAVAILABLE,
        "STALE_MANIFEST_REVISION"
        | "DUPLICATE_ORIGINAL"
        | "DUPLICATE_PHYSICAL_ORIGINAL"
        | "DUPLICATE_PHYSICAL_APPLICATION"
        | "INSTANCE_RUNNING_WITH_OTHER_CONFIG"
        | "INSTANCE_RUNNING_IN_OTHER_SESSION"
        | "LAUNCH_CONFIG_CHANGED"
        | "INSTANCE_EXTERNALLY_RUNNING"
        | "INSTANCE_STILL_RUNNING"
        | "INSTANCE_RESOURCE_BUSY" => CONFLICT,
        "INTEGRATION_CLEANUP_REQUIRED" | "CORE_RECONFIGURATION_REQUIRED" => ACTION_REQUIRED,
        "LAUNCH_OPERATION_LIMIT" | "LAUNCH_INDETERMINATE" => UNCONFIRMED,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_map_to_documented_categories() {
        assert_eq!(for_code("INSTANCE_NOT_FOUND"), Some(INVALID));
        assert_eq!(for_code("APP_NOT_INSTALLED"), Some(UNAVAILABLE));
        assert_eq!(for_code("STALE_MANIFEST_REVISION"), Some(CONFLICT));
        assert_eq!(for_code("LAUNCH_CONFIG_CHANGED"), Some(CONFLICT));
        assert_eq!(
            for_code("CORE_RECONFIGURATION_REQUIRED"),
            Some(ACTION_REQUIRED)
        );
        assert_eq!(for_code("LAUNCH_INDETERMINATE"), Some(UNCONFIRMED));
        assert_eq!(for_code("SOME_OTHER_CODE"), None);
    }
}
