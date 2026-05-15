//! Unified error types for the ZeroClaw plugin infrastructure.

/// Structured error enum covering all plugin infrastructure failure modes.
#[derive(Debug, thiserror::Error)]
pub enum ZeroClawError {
    /// A named component was requested but is not registered.
    #[error("Component '{name}' not found. Available: [{available}]")]
    ComponentNotFound { name: String, available: String },

    /// A dynamic library could not be loaded from disk.
    #[error("Failed to load dynamic library '{path}': {source}")]
    DynLibLoad {
        path: String,
        #[source]
        source: anyhow::Error,
    },

    /// A required symbol was absent from a loaded dynamic library.
    #[error("Missing symbol '{symbol}' in '{path}'")]
    MissingSymbol { path: String, symbol: String },

    /// The plugin's API version is incompatible with this host.
    #[error("Version mismatch: {message}")]
    VersionMismatch { message: String },

    /// A component factory returned an error during initialization.
    #[error("Failed to initialize component '{name}': {source}")]
    ComponentInit {
        name: String,
        #[source]
        source: anyhow::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_component_not_found_message() {
        let err = ZeroClawError::ComponentNotFound {
            name: "my-tool".to_string(),
            available: "foo, bar".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "Component 'my-tool' not found. Available: [foo, bar]"
        );
    }

    #[test]
    fn test_version_mismatch_message() {
        let err = ZeroClawError::VersionMismatch {
            message: "major version differs: host=1, plugin=2".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "Version mismatch: major version differs: host=1, plugin=2"
        );
    }

    #[test]
    fn test_into_anyhow() {
        let err = ZeroClawError::ComponentNotFound {
            name: "x".to_string(),
            available: String::new(),
        };
        let anyhow_err: anyhow::Error = err.into();
        assert!(anyhow_err.to_string().contains("not found"));
    }
}
