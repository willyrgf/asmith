use anyhow::Result;
use tracing_subscriber::EnvFilter;

/// Initialize logging with the appropriate filter level based on debug setting
pub fn init_logging(app_name: &str, debug: bool) -> Result<()> {
    // Create the filter based on debug flag
    let filter = if debug {
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(format!("{},matrix_sdk=debug", app_name)))
    } else {
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(format!("{},matrix_sdk=info", app_name)))
    };

    // Initialize the tracing subscriber with the filter
    tracing_subscriber::fmt()
        .with_target(true)
        .with_env_filter(filter)
        .init();

    Ok(())
}
