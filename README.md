# MCP Bridge

[![CI](https://github.com/johz-chen/mcp-bridge/actions/workflows/ci.yaml/badge.svg)](https://github.com/johz-chen/mcp-bridge/actions/workflows/ci.yaml)
[![Release](https://github.com/johz-chen/mcp-bridge/actions/workflows/release.yaml/badge.svg)](https://github.com/johz-chen/mcp-bridge/actions/workflows/release.yaml)

MCP Bridge 是一个实现 MCP（Model Control Protocol）协议的桥接服务，用于连接 MCP 工具服务与客户端应用。它支持通过 WebSocket 或 MQTT 协议传输数据，并管理本地进程作为 MCP 工具服务。

目前主要用于为小智 AI 接入 MCP 服务。


## 使用指南

### 1. 安装

#### 预编译二进制
从 [GitHub Releases](https://github.com/johz-chen/mcp-bridge/releases) 下载对应平台的二进制文件。

#### 从源码构建
```bash
# 克隆仓库
git clone https://github.com/johz-chen/mcp-bridge.git
cd mcp-bridge

# 构建项目
cargo build --release

# 运行
./target/release/mcp-bridge start
```

### 2. 配置文件

#### `config.yaml` 示例
```yaml
websocket:
  enabled: true
  endpoint: wss://api.xiaozhi.me/mcp/?token=your-token

mqtt:
  enabled: false
  broker: "broker.example.com"
  port: 1883
  client_id: ""
  topic: "mcp/bridge"

connection:
  heartbeat:
    interval: 30000  # 心跳间隔(ms)
    timeout: 10000   # 超时时间(ms)
  reconnect:
    interval: 5000   # 重连间隔(ms)
    max_attempts: 10 # 最大重连尝试次数
```

#### `mcp_tools.json` 示例
```json
{
    "exchange-rate": {
        "command": "npx",
        "args": [
            "-y",
            "@karashiiro/exchange-rate-mcp"
        ]
    },
    "amap-maps": {
        "command": "npx",
        "args": [
            "-y",
            "@amap/amap-maps-mcp-server"
        ],
        "env": {
            "AMAP_MAPS_API_KEY": "your-key"
        }
    },
    "example-node-tool": {
        "command": "node",
        "args": [
            "/path/to/node-tool.js"
        ]
    },
    "example-python-tool": {
        "command": "python",
        "args": [
            "/path/to/python-tool.py"
        ]
    }
}

```

### 3. 启动服务

```bash
# 使用默认配置文件
./mcp-bridge start

# 指定自定义配置文件
./mcp-bridge start \
  --config /path/to/custom_config.yaml \
  --tools-config /path/to/custom_mcp_tools.json
```

### 4. 命令行选项

```
./mcp-bridge start [OPTIONS]

选项：
  -c, --config <YAML_FILE>        主配置文件路径 [默认: conf/config.yaml]
  -t, --tools-config <JSON_FILE>  工具配置文件路径 [默认: conf/mcp_tools.json]
```

### 5. 日志与监控

服务使用 `tracing` 框架记录日志，默认日志级别为 `DEBUG`。可通过环境变量控制日志级别：

```bash
RUST_LOG=warn ./mcp-bridge start
```

### 6. 服务管理

使用 `Ctrl+C` 优雅地停止服务，所有子进程将被终止。

### 7. 集成测试

项目包含全面的集成测试：

```bash
cargo test --test integration_tests
```

## 开发指南

### 构建项目

```bash
cargo build
```

### 运行测试

```bash
cargo test
```

### 代码格式化

```bash
cargo fmt
```

### 代码检查

```bash
cargo clippy
```

## 贡献

欢迎通过 Issues 提交问题，通过 Pull Requests 贡献代码吗，请确保所有更改都包含相应的测试。
