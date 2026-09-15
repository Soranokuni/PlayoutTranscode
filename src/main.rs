use playout_transcode::{bootstrap, config, db, jobs, logging, profiles, server, service_handle};

use anyhow::Result;
use clap::{Parser, Subcommand};
use service_handle::ServiceHandle;
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(
    name = "PlayoutTranscode",
    version,
    about = "Broadcast media transcoding service"
)]
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
    #[command(
        name = "gen-token",
        about = "Generate an API token, write it to config.toml, and print it once"
    )]
    GenToken {
        #[arg(long, value_name = "PATH")]
        config: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None => {
            run_headless(None);
        }
        Some(Commands::Wizard) => match config::AppConfig::run_wizard() {
            Ok(_) => {
                println!("Wizard complete. Visit the web UI to configure and start the service.")
            }
            Err(e) => eprintln!("Wizard error: {}", e),
        },
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
        Some(Commands::GenToken { config }) => {
            if let Err(e) = gen_token(config) {
                eprintln!("gen-token error: {}", e);
                std::process::exit(1);
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

/// Generate a fresh API token, persist it, and print it once.
///
/// Printed to stdout and never logged: the operator copies it into PlayOut's
/// settings and into the web UI, and it is redacted from `GET /api/config`.
fn gen_token(config_path_override: Option<String>) -> Result<()> {
    let (mut app_config, config_path) =
        config::AppConfig::load(config_path_override.as_deref())
            .map_err(|e| anyhow::anyhow!("Failed to load configuration: {}", e))?;

    let token = config::gen_api_token();
    app_config.server.api_token = token.clone();
    app_config
        .save_to(&config_path)
        .map_err(|e| anyhow::anyhow!("Failed to save configuration: {}", e))?;

    println!("
API token written to {}
", config_path.display());
    println!("  {}
", token);
    println!("This is the only time it is shown. Set it in:");
    println!("  - PlayOut  : settings.ingestorApiToken");
    println!("  - Web UI   : the token prompt on first load");
    println!("
Restart the service for it to take effect.
");
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

    profiles::validate_color_constants()
        .map_err(|e| anyhow::anyhow!("Color constant misconfiguration: {}", e))?;

    let port = app_config.server.web_port;
    let bind_addr = app_config.server.bind_address.clone();
    let url = format!("http://{}:{}", bind_addr, port);
    println!("\n  PlayoutTranscode web UI starting at {}\n", url);
    tracing::info!("PlayoutTranscode starting on {}", url);

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));

    let pool = db::init_pool(&exe_dir.join("media_assets.db"))
        .await
        .map_err(|e| anyhow::anyhow!("Database init failed: {}", e))?;
    let pool = Arc::new(pool);
    tracing::info!("Asset database ready");

    let (_, toolchain_status) = bootstrap::audit_toolchain();
    tracing::info!("FFmpeg: {:?}", toolchain_status.ffmpeg_version);

    let (event_tx, _rx) = tokio::sync::broadcast::channel::<String>(256);
    let job_queue = jobs::JobQueue::new(event_tx, Some(pool.clone()));
    if let Ok(report) = db::recover_stale_jobs(&pool).await {
        if report.requeued > 0 || report.failed_exhausted > 0 {
            tracing::info!(
                "Startup crash recovery: {} job(s) re-queued, {} job(s) marked failed",
                report.requeued,
                report.failed_exhausted
            );
        }
    }
    if let Ok(existing_jobs) = db::load_all_durable_jobs(&pool).await {
        job_queue.populate(existing_jobs);
    }
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
    let server_pool = pool.clone();

    let web_ui_dir = if exe_dir
        .join("web-ui")
        .join("dist")
        .join("index.html")
        .exists()
    {
        exe_dir.join("web-ui").join("dist")
    } else if let Ok(cwd) = std::env::current_dir() {
        if cwd.join("web-ui").join("dist").join("index.html").exists() {
            cwd.join("web-ui").join("dist")
        } else {
            exe_dir.join("web-ui").join("dist")
        }
    } else {
        exe_dir.join("web-ui").join("dist")
    };

    let server_task = tokio::spawn(async move {
        server::run_server(
            port,
            &bind_addr,
            server::ServerDeps {
                jobs: jq,
                config: server_cfg,
                toolchain_status: toolchain_status.clone(),
                service_handle: sh,
                web_ui_dir,
                pool: server_pool,
            },
        )
        .await
    });

    if config_initialized
        && !watch_root.to_string_lossy().is_empty()
        && !target_root.to_string_lossy().is_empty()
        && app_config.validate().is_ok()
    {
        service_handle.add_log("info", "Auto-starting service with configured watch folder");
        if let Ok(tools) = bootstrap::ensure_toolchain() {
            let _ = service_handle::start_processing_loop(
                &service_handle,
                &app_config,
                &job_queue,
                &tools,
                pool.clone(),
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
