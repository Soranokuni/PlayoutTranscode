use tracing_subscriber::{fmt, prelude::*, EnvFilter};

pub fn init_logging(level: &str) {
    let filter = EnvFilter::try_new(format!("playout_transcode={}", level))
        .unwrap_or_else(|_| EnvFilter::new("playout_transcode=info"));

    let fmt_layer = fmt::layer()
        .with_target(false)
        .with_thread_ids(false)
        .with_thread_names(false)
        .with_level(true);

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .init();
}
