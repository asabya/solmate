use tracing::{info, Level};

pub fn get_env_var(key: &str) -> Result<String, String> {
    let span = tracing::span!(target: "env", Level::INFO, "get_env_var");
    let _enter = span.enter();
    let value = std::env::var(key).map_err(|_| format!("{} is not set", key))?;
    info!("{} is set", key);
    Ok(value)
}
