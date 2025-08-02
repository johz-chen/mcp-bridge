use anyhow::Result;
use clap::{Parser, Subcommand};
use mcp_bridge::bridge::Bridge;
use mcp_bridge::config::BridgeConfig;
use std::path::PathBuf;
use tokio::signal;
use tracing::{error, info};

#[derive(Parser)]
#[command(name = "mcp-bridge")]
#[command(about = "MCP Protocol Bridge", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the bridge service
    Start {
        /// Config file path (YAML)
        #[arg(
            short,
            long,
            value_name = "YAML_FILE",
            default_value = "conf/config.yaml"
        )]
        config: PathBuf,

        /// Tools config file path (JSON)
        #[arg(
            short,
            long,
            value_name = "JSON_FILE",
            default_value = "conf/mcp_tools.json"
        )]
        tools_config: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    let cli = Cli::parse();
    match cli.command {
        Commands::Start {
            config,
            tools_config,
        } => {
            let cfg = match BridgeConfig::load_from_files(&config, &tools_config) {
                Ok(cfg) => cfg,
                Err(e) => {
                    error!("Failed to load config: {:#}", e);
                    return Ok(());
                }
            };

            let bridge = match Bridge::new(cfg).await {
                Ok(bridge) => bridge,
                Err(e) => {
                    error!("Failed to create bridge: {:#}", e);
                    return Ok(());
                }
            };

            // 设置优雅关闭
            let (_shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
            let bridge_handle = tokio::spawn(async move {
                if let Err(e) = bridge.run().await {
                    error!("Bridge exited with error: {:#}", e);
                }
            });

            // 监听关闭信号
            tokio::select! {
                _ = signal::ctrl_c() => {
                    info!("Received Ctrl+C, shutting down...");
                }
                _ = shutdown_rx => {
                    info!("Received shutdown signal");
                }
            }

            // 发送中止信号给桥接器
            bridge_handle.abort();
            if let Err(e) = bridge_handle.await {
                if !e.is_cancelled() {
                    error!("Bridge task error: {:#}", e);
                }
            }

            info!("Bridge shutdown complete");
            Ok(())
        }
    }
}
