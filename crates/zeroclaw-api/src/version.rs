//! API version constants and compatibility checking for the zeroclaw-api plugin interface.
//!
//! The version is encoded as a `u32` using the scheme `major * 10_000 + minor * 100 + patch`.
//! This allows compile-time encoding/decoding via `const fn`.

/// Human-readable API version string.
pub const API_VERSION: &str = "0.1.0";

/// Machine-readable API version encoded as `major * 10_000 + minor * 100 + patch`.
pub const API_VERSION_U32: u32 = 100;

/// Major version component of the API.
pub const API_VERSION_MAJOR: u32 = 0;

/// Minor version component of the API.
pub const API_VERSION_MINOR: u32 = 1;

/// Patch version component of the API.
pub const API_VERSION_PATCH: u32 = 0;

/// Encode a (major, minor, patch) triple into a `u32`.
///
/// Not part of the public API — used only in unit tests.
#[cfg(test)]
const fn encode_version(major: u32, minor: u32, patch: u32) -> u32 {
    major * 10_000 + minor * 100 + patch
}

/// Decode a `u32` produced by [`encode_version`] back into `(major, minor, patch)`.
///
/// ```
/// # use zeroclaw_api::version::decode_version;
/// assert_eq!(decode_version(100), (0, 1, 0));
/// ```
pub const fn decode_version(version: u32) -> (u32, u32, u32) {
    let major = version / 10_000;
    let minor = (version % 10_000) / 100;
    let patch = version % 100;
    (major, minor, patch)
}

/// Check whether a plugin's API version is compatible with this host.
///
/// Compatibility rules:
/// - Major versions must match exactly.
/// - Plugin minor version must be ≤ host minor version (forward-compatible host).
/// - Patch version is ignored.
///
/// # Errors
///
/// Returns `Err(String)` with a diagnostic message when the versions are incompatible.
pub fn check_compatibility(plugin_version: u32) -> Result<(), String> {
    let (p_major, p_minor, _) = decode_version(plugin_version);
    let host_major = API_VERSION_MAJOR;
    let host_minor = API_VERSION_MINOR;

    if p_major != host_major {
        return Err(format!(
            "zeroclaw-api major version mismatch: host={host_major}, plugin={p_major}"
        ));
    }

    if p_minor > host_minor {
        return Err(format!(
            "zeroclaw-api plugin minor version ({p_minor}) is newer than host ({host_minor})"
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip() {
        assert_eq!(decode_version(encode_version(1, 23, 4)), (1, 23, 4));
        assert_eq!(decode_version(encode_version(0, 1, 0)), (0, 1, 0));
        assert_eq!(decode_version(API_VERSION_U32), (0, 1, 0));
    }

    #[test]
    fn test_exact_match() {
        assert!(check_compatibility(API_VERSION_U32).is_ok());
    }

    #[test]
    fn test_major_mismatch() {
        let plugin_v = encode_version(1, 0, 0); // major=1, host major=0
        let result = check_compatibility(plugin_v);
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("major version mismatch"), "got: {msg}");
    }

    #[test]
    fn test_plugin_minor_too_high() {
        let plugin_v = encode_version(0, 2, 0); // minor=2, host minor=1
        let result = check_compatibility(plugin_v);
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("newer than host"), "got: {msg}");
    }

    #[test]
    fn test_plugin_minor_lower_ok() {
        let plugin_v = encode_version(0, 0, 0); // minor=0 < host minor=1
        assert!(check_compatibility(plugin_v).is_ok());
    }
}
