use tracing_subscriber::{EnvFilter, prelude::*};

pub fn init(log_stderr: bool) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let subscriber = tracing_subscriber::registry().with(filter);

    #[cfg(target_os = "linux")]
    if !log_stderr {
        // Opening the layer probes the journal socket; absence or insufficient
        // permissions must not prevent startup on systems without journald.
        if let Ok(journal) = tracing_journald::layer() {
            subscriber
                .with(journal.with_syslog_identifier("sigen-server".into()))
                .init();
            return;
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = log_stderr;

    subscriber
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();
}
