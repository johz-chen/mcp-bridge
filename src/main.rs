use anyhow::Result;
use clap::{Parser, Subcommand}; // 添加 clap 导入
use mcp_bridge::bridge::Bridge; // 添加 Bridge 导入
use mcp_bridge::config::BridgeConfig; // 添加 BridgeConfig 导入
use std::path::PathBuf; // 添加 PathBuf 导入
use tracing::error; // 添加 tracing 宏导入 // 添加 Result 导入

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
            let cfg = BridgeConfig::load_from_files(&config, &tools_config)?;

            let bridge = match Bridge::new(cfg).await {
                Ok(bridge) => bridge,
                Err(e) => {
                    error!("Failed to create bridge: {:#}", e);
                    return Ok(());
                }
            };

            if let Err(e) = bridge.run().await {
                error!("Bridge exited with error: {:#}", e);
            }

            Ok(())
        }
    }
}
