use mcp_bridge::{config::BridgeConfig, bridge};

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use anyhow::Result;
use tracing::error;

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
        /// Config file path
        #[arg(short, long, value_name = "FILE")]
        config: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    let cli = Cli::parse();
    match cli.command {
        Commands::Start { config } => {
            let cfg = BridgeConfig::load_from_file(config)?;
            
            // 创建并运行单个bridge实例
            let bridge = match bridge::Bridge::new(cfg).await {
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
