mod bootstrap;
mod config;
mod encoder;
mod identity;
mod jobs;
mod logging;
mod probe;
mod profiles;
mod processor;
mod server;
mod service_handle;
mod watcher;

use anyhow::Result;
use clap::{Parser, Subcommand};
use service_handle::ServiceHandle;

#[derive(Parser, Debug)]
#[command(name = "PlayoutTranscode", version, about = "Broadcast media transcoding service")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    #[command(about = "Run as headless background service (no GUI)")]
    Run {
        #[arg(long, value_name = "PATH")]
        config: Option<String>,
    },
    #[command(about = "Run interactive configuration wizard")]
    Wizard,
    #[command(about = "Check for FFmpeg updates")]
    CheckUpdate,
    #[command(about = "Show current toolchain status")]
    Status,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None => {
            run_headless(None);
        }
        Some(Commands::Wizard) => {
            match config::AppConfig::run_wizard() {
                Ok(_) => println!("Wizard complete. Visit the web UI to configure and start the service."),
                Err(e) => eprintln!("Wizard error: {}", e),
            }
        }
        Some(Commands::Run { config }) => {
            run_headless(config);
        }
        Some(Commands::CheckUpdate) => {
            let result = bootstrap::check_ffmpeg_update();
            println!("Current version: {:?}", result.current_version);
            if let Some(w) = result.warning {
                println!("\n{}\n", w);
            }
        }
        Some(Commands::Status) => {
            let (_, status) = bootstrap::audit_toolchain();
            println!("FFmpeg found: {}", status.ffmpeg_found);
            println!("FFprobe found: {}", status.ffprobe_found);
            println!("FFmpeg version: {:?}", status.ffmpeg_version);
            println!("Bin dir: {}", status.bin_dir);
        }
    }

    Ok(())
}

fn run_headless(config_path_override: Option<String>) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async move {
        if let Err(e) = run_service(config_path_override).await {
            eprintln!("Service error: {}", e);
            std::process::exit(1);
        }
    });
}

async fn run_service(config_path_override: Option<String>) -> Result<()> {
    use std::path::PathBuf;

    let (app_config, _config_path) = config::AppConfig::load(config_path_override.as_deref())
        .map_err(|e| anyhow::anyhow!("Failed to load configuration: {}", e))?;

    logging::init_logging(&app_config.logging.level);

    let port = app_config.server.web_port;
    let bind_addr = app_config.server.bind_address.clone();
    let url = format!("http://{}:{}", bind_addr, port);
    println!("\n  PlayoutTranscode web UI starting at {}\n", url);
    tracing::info!("PlayoutTranscode starting on {}", url);

    let (_, toolchain_status) = bootstrap::audit_toolchain();
    tracing::info!("FFmpeg: {:?}", toolchain_status.ffmpeg_version);

    let (event_tx, _rx) = tokio::sync::broadcast::channel::<String>(256);
    let job_queue = jobs::JobQueue::new(event_tx);
    let service_handle = ServiceHandle::new();

    let watch_root = PathBuf::from(&app_config.paths.watch_folder);
    let target_root = PathBuf::from(&app_config.paths.target_folder);
    let _ = std::fs::create_dir_all(&target_root);

    let config_initialized = app_config.initialized;

    let server_cfg = app_config.clone();
    let bind_addr = server_cfg.server.bind_address.clone();
    let port = server_cfg.server.web_port;
    let sh = service_handle.clone();
    let jq = job_queue.clone();

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    let web_ui_dir = exe_dir.join("web-ui").join("dist");

    let server_task = tokio::spawn(async move {
        server::run_server(
            port, &bind_addr, jq,
            server_cfg,
            toolchain_status.clone(), sh,
            web_ui_dir,
        ).await
    });

    if config_initialized
        && !watch_root.to_string_lossy().is_empty()
        && !target_root.to_string_lossy().is_empty()
        && app_config.validate().is_ok()
    {
        service_handle.add_log("info", "Auto-starting service with configured watch folder");
        if let Ok(tools) = bootstrap::ensure_toolchain() {
            let _ = service_handle::start_processing_loop(
                &service_handle, &app_config, &job_queue, &tools,
            );
        } else {
            service_handle.add_log("warn", "FFmpeg not found. Download from the web UI.");
        }
    }

    tokio::select! {
        result = server_task => {
            if let Err(e) = result {
                tracing::error!("Server task failed: {}", e);
            }
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Shutting down...");
            service_handle::stop_processing(&service_handle);
        }
    }

    Ok(())
}
