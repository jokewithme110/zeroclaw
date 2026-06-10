//! Early `--config-dir` bootstrap helpers for the CLI entrypoint.
//!
//! This file parses the minimal subset of CLI arguments that must be resolved
//! before i18n initialization, without pulling the full clap parser into the
//! process-start bootstrap path.

#[cfg(feature = "agent-runtime")]
pub fn apply_early_config_dir_override_from_args() {
    if let Some(config_dir) = early_config_dir_override_from_args(std::env::args_os().skip(1)) {
        // SAFETY: called at process start before any threads are spawned.
        unsafe { std::env::set_var("ZEROCLAW_CONFIG_DIR", config_dir) };
    }
}

#[cfg(feature = "agent-runtime")]
pub fn early_config_dir_override_from_args(
    args: impl IntoIterator<Item = std::ffi::OsString>,
) -> Option<std::ffi::OsString> {
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == std::ffi::OsStr::new("--") {
            break;
        }

        if arg == std::ffi::OsStr::new("--config-dir") {
            if let Some(value) = args.next()
                && !value.is_empty()
            {
                return Some(value);
            }
            break;
        }

        if let Some(arg) = arg.to_str()
            && let Some(value) = arg.strip_prefix("--config-dir=")
            && !value.trim().is_empty()
        {
            return Some(std::ffi::OsString::from(value));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn early_config_dir_override_parses_equals_form() {
        let value = super::early_config_dir_override_from_args([std::ffi::OsString::from(
            "--config-dir=/tmp/zeroclaw-profile",
        )]);
        assert_eq!(
            value.as_deref(),
            Some(std::ffi::OsStr::new("/tmp/zeroclaw-profile"))
        );
    }

    #[test]
    fn early_config_dir_override_parses_split_form() {
        let value = super::early_config_dir_override_from_args([
            std::ffi::OsString::from("--config-dir"),
            std::ffi::OsString::from("/tmp/zeroclaw-profile"),
        ]);
        assert_eq!(
            value.as_deref(),
            Some(std::ffi::OsStr::new("/tmp/zeroclaw-profile"))
        );
    }

    #[test]
    fn early_config_dir_override_ignores_blank_equals_form() {
        let value = super::early_config_dir_override_from_args([std::ffi::OsString::from(
            "--config-dir=   ",
        )]);
        assert!(value.is_none());
    }
}
