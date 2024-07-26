//! Support for reading and caching configuration data from `sqlx.toml`

use std::fmt::Debug;
use std::path::PathBuf;

#[cfg(feature = "config-macros")]
pub mod macros;

#[cfg(feature = "config-migrate")]
pub mod migrate;

/// The parsed structure of a `sqlx.toml` file.
#[derive(Debug, serde::Deserialize)] 
pub struct Config {
    /// Configuration for the [`sqlx::query!()`] family of macros.
    /// 
    /// See type documentation for details.
    #[cfg_attr(docsrs, doc(cfg(any(feature = "config-all", feature = "config-macros"))))]
    #[cfg(feature = "config-macros")]
    pub macros: macros::Config,

    /// Configuration for migrations when executed using `sqlx::migrate!()` or through `sqlx-cli`.
    /// 
    /// See type documentation for details.
    #[cfg_attr(docsrs, doc(cfg(any(feature = "config-all", feature = "config-migrate"))))]
    #[cfg(feature = "config-migrate")]
    pub migrate: migrate::Config,
}

#[derive(thiserror::Error, Debug)]
pub enum ConfigError {
    #[error("CARGO_MANIFEST_DIR must be set and valid")]
    Env(#[from] #[source] std::env::VarError),

    #[error("error reading config file {path:?}")]
    Read {
        path: PathBuf,
        #[source] error: std::io::Error,
    },

    #[error("error parsing config file {path:?}")]
    Parse {
        path: PathBuf,
        #[source] error: toml::de::Error,
    }
}

#[doc(hidden)]
#[allow(clippy::result_large_err)]
impl Config {
    pub fn get() -> &'static Self {
        Self::try_get().unwrap()
    }

    pub fn try_get() -> Result<&'static Self, ConfigError> {
        Self::try_get_with(|| { 
            let mut path = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
            path.push("sqlx.toml");
            Ok(path)
        })
    }

    pub fn try_get_with(make_path: impl FnOnce() -> Result<PathBuf, ConfigError>) -> Result<&'static Self, ConfigError> {
        // `std::sync::OnceLock` doesn't have a stable `.get_or_try_init()`
        // because it's blocked on a stable `Try` trait.
        use once_cell::sync::OnceCell;

        static CACHE: OnceCell<Config> = OnceCell::new();

        CACHE.get_or_try_init(|| {
            let path = make_path()?;
            Self::read_from(path)
        })
    }
    
    fn read_from(path: PathBuf) -> Result<Self, ConfigError> {
        // The `toml` crate doesn't provide an incremental reader.
        let toml_s = match std::fs::read_to_string(&path) {
            Ok(toml) => toml,
            Err(error) => {
                return Err(ConfigError::Read {
                    path,
                    error
                });
            }
        };
        
        tracing::debug!("read config TOML from {path:?}:\n{toml_s}");
        
        toml::from_str(&toml_s).map_err(|error| {
            ConfigError::Parse { 
                path,
                error,
            }
        })
    }
}
