/// Configuration for the [`sqlx::query!()`] family of macros.
#[derive(Debug, serde::Deserialize)]
pub struct Config {
    /// Override the environment variable
    pub database_url_var: Option<String>,
}