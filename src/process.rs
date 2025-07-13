use crate::config::ProcessConfig;
use anyhow::Context;
use tokio::process::{Command, Child, ChildStdin};
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::{error, info, debug};
use std::process::Stdio;
use tokio::sync::mpsc;

pub struct ManagedProcess {
    config: ProcessConfig,
    child: Option<Child>,
    pub stdin: Option<ChildStdin>, // 改为 pub 以便外部访问
}

impl ManagedProcess {
    pub fn new(config: &ProcessConfig) -> anyhow::Result<Self> {
        Ok(Self {
            config: config.clone(),
            child: None,
            stdin: None,
        })
    }
    
    pub async fn start(&mut self, output_tx: mpsc::Sender<(String, String)>, server_name: String) -> anyhow::Result<()> {
        info!("Starting process: {}", self.config.command);
        
        let mut command = Command::new(&self.config.command);
        
        // Set arguments
        command.args(&self.config.args);
        
        // Set environment variables
        for (key, value) in &self.config.env {
            command.env(key, value);
        }
        
        // 设置工作目录为项目根目录
        let current_dir = std::env::current_dir()?;
        command.current_dir(current_dir);
        
        // Configure stdio
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        
        // Spawn the child process
        let mut child = command.spawn().context("Failed to spawn child process")?;
        
        // Take ownership of stdin and stdout
        let stdin = child.stdin.take().context("Failed to open stdin")?;
        let stdout = child.stdout.take().context("Failed to open stdout")?;
        
        // Create buffered reader for stdout
        let stdout = BufReader::new(stdout);
        
        // Start output reader task
        let mut reader = stdout;
        tokio::spawn(async move {
            let mut buffer = String::new();
            loop {
                match reader.read_line(&mut buffer).await {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let line = buffer.trim().to_string();
                        if !line.is_empty() {
                            debug!("Process output: {}", line);
                            if let Err(e) = output_tx.send((server_name.clone(), line)).await {
                                error!("Failed to send output: {}", e);
                                break;
                            }
                        }
                        buffer.clear();
                    }
                    Err(e) => {
                        error!("Error reading stdout: {}", e);
                        break;
                    }
                }
            }
        });
        
        self.child = Some(child);
        self.stdin = Some(stdin); // 存储 stdin
        
        Ok(())
    }
    
    pub async fn stop(&mut self) -> anyhow::Result<()> {
        if let Some(mut child) = self.child.take() {
            info!("Stopping process");
            child.kill().await?;
        }
        self.stdin = None;
        Ok(())
    }
}
