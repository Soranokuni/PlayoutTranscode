use playout_transcode::app::{self, ShutdownToken};
use playout_transcode::{bootstrap, config, paths};

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "PlayoutTranscode",
    version,
    about = "Broadcast media transcoding service"
)]
struct Cli {
    /// Directory holding `config.toml`, the asset registry, logs and the
    /// downloaded toolchain. Overrides `PLAYOUT_TRANSCODE_DATA`.
    #[arg(long, value_name = "PATH", global = true)]
    data_dir: Option<String>,

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
    #[command(
        name = "service-run",
        about = "Windows Service entry point. Used by the SCM only; use `run` from a console",
        hide = true
    )]
    ServiceRun {
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

    // Must happen before anything reads `config.toml`, opens the registry or
    // audits the toolchain: every one of those resolves through
    // `paths::data_dir()` and re-resolving mid-run would split the service's
    // state across two directories (T2-2).
    let data_dir = paths::resolve_data_dir_from_env(cli.data_dir.as_deref());
    let data_dir = match paths::set_data_dir(data_dir.clone()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{} ({})", e, data_dir.display());
            std::process::exit(1);
        }
    };

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
        Some(Commands::ServiceRun { config }) => {
            if let Err(e) = run_as_windows_service(config) {
                eprintln!("{}", e);
                std::process::exit(1);
            }
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
            println!("Data dir: {}", data_dir.display());
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
    let (mut app_config, config_path) = config::AppConfig::load(config_path_override.as_deref())
        .map_err(|e| anyhow::anyhow!("Failed to load configuration: {}", e))?;

    let token = config::gen_api_token();
    app_config.server.api_token = token.clone();
    app_config
        .save_to(&config_path)
        .map_err(|e| anyhow::anyhow!("Failed to save configuration: {}", e))?;

    println!(
        "
API token written to {}
",
        config_path.display()
    );
    println!(
        "  {}
",
        token
    );
    println!("This is the only time it is shown. Set it in:");
    println!("  - PlayOut  : settings.ingestorApiToken");
    println!("  - Web UI   : the token prompt on first load");
    println!(
        "
Restart the service for it to take effect.
"
    );
    Ok(())
}

/// Hand the process to the Service Control Manager (T2-1).
///
/// Only valid when the SCM launched us; from a console it fails immediately
/// rather than half-starting, and the message points at `run`.
#[cfg(windows)]
fn run_as_windows_service(config_path_override: Option<String>) -> Result<()> {
    playout_transcode::win_service::run(config_path_override).map_err(|e| anyhow::anyhow!(e))
}

#[cfg(not(windows))]
fn run_as_windows_service(_config_path_override: Option<String>) -> Result<()> {
    Err(anyhow::anyhow!(
        "`service-run` is the Windows Service Control Manager entry point and is \
         only available on Windows. Use `PlayoutTranscode run`."
    ))
}

fn run_headless(config_path_override: Option<String>) {
    // `Runtime::new().unwrap()` panicked with a backtrace and no explanation
    // when the process could not get a thread pool (F-30). Say what failed.
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("Failed to create the Tokio runtime: {}", e);
            std::process::exit(1);
        }
    };
    rt.block_on(async move {
        if let Err(e) = app::run_service(config_path_override, ShutdownToken::new()).await {
            eprintln!("Service error: {}", e);
            std::process::exit(1);
        }
    });
}
