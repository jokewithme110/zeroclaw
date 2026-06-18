//! zeroclaw-dt-nodes 可执行二进制入口
//!
//! 使用方式:
//! ```shell
//! # 初始化配置（生成 ~/.zeroclaw_node/config.toml）
//! zeroclaw-dt-nodes init
//!
//! # 使用自定义配置目录
//! ZEROCLAW_NODE_CONFIG_DIR=/path/to/config zeroclaw-dt-nodes init
//!
//! # 启动节点（从配置文件读取配置）
//! zeroclaw-dt-nodes start
//!
//! # 发送消息到 webchat channel（非流式）
//! zeroclaw-dt-nodes chat --message "你好"
//! ```

use clap::{Parser, Subcommand};
use tracing_subscriber::{EnvFilter, fmt};
use zeroclaw_dt_nodes::{chat, config, dt_nodes};

#[derive(Parser)]
#[command(name = "zeroclaw-dt-nodes")]
#[command(about = "ZeroClaw DT Nodes - 分布式节点运行时")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 初始化配置 - 生成配置文件到 $ZEROCLAW_NODE_CONFIG_DIR/.zeroclaw_node/
    Init,

    /// 启动节点 - 从配置文件读取配置并连接到 gateway
    Start {
        /// 交互式配置网关（可选，默认从配置文件读取）
        #[arg(long, default_value_t = false)]
        interactive: bool,

        /// 覆盖网关主机地址，仅静态连接模式使用
        #[arg(long)]
        host: Option<String>,

        /// 覆盖网关端口，仅静态连接模式使用
        #[arg(long)]
        port: Option<u16>,

        /// 覆盖网关 token，仅静态连接模式使用
        #[arg(long)]
        token: Option<String>,

        /// 覆盖节点显示名
        #[arg(long)]
        name: Option<String>,

        /// 覆盖自动发现开关为启用；启用后连接地址和 token 仅来自发现结果
        #[cfg(feature = "auto_discovery")]
        #[arg(long, conflicts_with = "no_auto_discovery")]
        auto_discovery: bool,

        /// 覆盖自动发现开关为禁用，回退到本地/CLI 静态网关配置
        #[cfg(feature = "auto_discovery")]
        #[arg(long = "no-auto-discovery", conflicts_with = "auto_discovery")]
        no_auto_discovery: bool,

        /// 覆盖 FIFO 所在目录，默认读取本地配置
        #[cfg(feature = "auto_discovery")]
        #[arg(long)]
        fifo_dir: Option<String>,

        /// 覆盖 FIFO 等待超时秒数，默认读取本地配置
        #[cfg(feature = "auto_discovery")]
        #[arg(long)]
        fifo_wait_timeout_secs: Option<u64>,
    },

    /// 发送消息到 webchat channel（非流式）
    Chat {
        /// 要发送的消息内容
        #[arg(long, short, required = true)]
        message: String,

        /// 图片文件路径列表，将转换为 base64 格式发送
        #[arg(long = "images", value_delimiter = ',', num_args = 1..)]
        images: Vec<String>,
    },

    /// 发送事件到所有订阅的 channel（非流式）
    EventEmit {
        /// 事件类型
        #[arg(long, short, required = true)]
        event: String,

        /// 要发送的消息内容
        #[arg(long, short, required = true)]
        message: String,

        /// 图片文件路径列表，将转换为 base64 格式发送
        #[arg(long = "images", value_delimiter = ',', num_args = 1..)]
        images: Vec<String>,
    },

    /// Manage emitted events
    #[cfg(feature = "agent-runtime")]
    Event {
        #[command(subcommand)]
        command: dt_nodes::EventCommands,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 初始化 tracing 日志
    let default_log_level = "info,zeroclaw_dt_nodes=debug";
    let subscriber = fmt::Subscriber::builder()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_log_level)),
        )
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let cli = Cli::parse();

    match cli.command {
        Commands::Init => {
            let config_path = config::NodeConfig::init_config_file()?;
            println!("配置文件已生成：{}", config_path);
            println!("请编辑此文件配置网关和其他设置。");
            println!("\n然后运行以下命令启动节点:");
            println!("  zeroclaw-dt-nodes start");
        }
        Commands::Start {
            interactive,
            host,
            port,
            token,
            name,
            #[cfg(feature = "auto_discovery")]
            auto_discovery,
            #[cfg(feature = "auto_discovery")]
            no_auto_discovery,
            #[cfg(feature = "auto_discovery")]
            fifo_dir,
            #[cfg(feature = "auto_discovery")]
            fifo_wait_timeout_secs,
        } => {
            // 加载配置
            let mut config = Box::pin(config::NodeConfig::load_or_init()).await?;
            #[cfg(feature = "auto_discovery")]
            let auto_discovery = if auto_discovery {
                Some(true)
            } else if no_auto_discovery {
                Some(false)
            } else {
                None
            };

            let start_options = dt_nodes::LocalStartOptions {
                interactive,
                host,
                port,
                name,
                token,
                #[cfg(feature = "auto_discovery")]
                auto_discovery,
                #[cfg(feature = "auto_discovery")]
                fifo_dir,
                #[cfg(feature = "auto_discovery")]
                fifo_wait_timeout_secs,
            };

            dt_nodes::run_node(&mut config, &start_options).await?;
        }
        Commands::Chat { message, images } => {
            let config = Box::pin(config::NodeConfig::load_or_init()).await?;
            let (gateway_url, token) = config.resolve_business_event_destination_target()?;
            chat::send_to_webchat(&gateway_url, &message, &images, token.as_deref()).await?;
        }
        Commands::EventEmit {
            event,
            message,
            images,
        } => {
            let config = Box::pin(config::NodeConfig::load_or_init()).await?;
            let (gateway_url, token) = config.resolve_business_event_destination_target()?;
            chat::emit_event(
                &gateway_url,
                &config.zeroclaw_node_dir,
                &event,
                &message,
                &images,
                token.as_deref(),
            )
            .await?;
        }
        #[cfg(feature = "agent-runtime")]
        Commands::Event { command } => {
            let config = Box::pin(config::NodeConfig::load_or_init()).await?;
            dt_nodes::handle_event_command(command, &config.zeroclaw_node_dir)?;
        }
        #[cfg(not(feature = "agent-runtime"))]
        Commands::Event { .. } => {
            eprintln!("Event commands are not available in this build");
        }
    }

    Ok(())
}
