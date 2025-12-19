//! # Runcell CLI
//!
//! 容器运行时命令行工具

use anyhow::Result;
use clap::{Parser, Subcommand};
use slog::{Drain, Logger, o};

mod container_cmd;
mod storage_cmd;

/// Runcell - 轻量级容器运行时
#[derive(Parser)]
#[command(name = "runcell")]
#[command(about = "容器运行时工具", long_about = None)]
struct Cli {
    /// 启用详细日志
    #[arg(short, long)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// 存储和镜像管理命令
    #[command(subcommand)]
    Storage(StorageCommands),

    /// 容器管理命令
    #[command(subcommand, visible_alias = "ctr")]
    Container(ContainerCommands),
}

#[derive(Subcommand, Debug)]
enum ContainerCommands {
    /// 创建容器
    Create {
        /// 容器 ID
        #[arg(short, long)]
        id: String,

        /// Rootfs 路径
        #[arg(short, long)]
        rootfs: String,

        /// Bundle 目录（可选，默认在 /tmp/runcell/bundles/{id}）
        #[arg(short, long)]
        bundle: Option<String>,
    },

    /// 运行容器（创建并启动）
    Run {
        /// 容器 ID
        #[arg(long)]
        id: String,

        /// 镜像源（支持 file://, dir://, 或本地路径）
        #[arg(short = 'm', long)]
        image: String,

        /// 分配伪终端（TTY）
        #[arg(short = 't', long)]
        tty: bool,

        /// 保持 STDIN 打开（交互模式）
        #[arg(short = 'i', long)]
        interactive: bool,

        /// 后台运行（分离模式）
        #[arg(short = 'd', long)]
        detach: bool,

        /// 要执行的命令及其参数（放在最后）
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },

    /// 启动已创建的容器
    Start {
        /// 容器 ID
        #[arg(short, long)]
        id: String,
    },

    /// 删除容器
    #[command(visible_alias = "rm")]
    Delete {
        /// 容器 ID
        #[arg(short, long)]
        id: String,
    },

    /// 列出所有容器
    #[command(visible_alias = "ls")]
    List {
        /// 输出格式 (table, json)
        #[arg(short, long, default_value = "table")]
        format: String,

        /// 显示所有容器（包括已停止的）
        #[arg(short, long)]
        all: bool,
    },

    /// 在运行中的容器内执行命令
    Exec {
        /// 容器 ID
        #[arg(long)]
        id: String,

        /// 分配伪终端（TTY）
        #[arg(short = 't', long)]
        tty: bool,

        /// 保持 STDIN 打开（交互模式）
        #[arg(short = 'i', long)]
        interactive: bool,

        /// 要执行的命令及其参数（放在最后）
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
enum StorageCommands {
    /// 测试绑定挂载
    Mount {
        /// 源路径
        #[arg(short, long)]
        source: String,

        /// 目标挂载点
        #[arg(short, long)]
        target: String,

        /// 挂载选项 (ro, bind等)
        #[arg(short, long)]
        options: Vec<String>,
    },

    /// 测试卸载
    Umount {
        /// 挂载点路径
        #[arg(short, long)]
        target: String,
    },

    /// 测试镜像拉取
    Pull {
        /// 镜像源 (file://, dir://, 或本地路径)
        #[arg(short, long)]
        image: String,

        /// 容器 ID
        #[arg(short, long)]
        container_id: String,
    },

    /// 清理镜像
    Cleanup {
        /// 容器 ID
        #[arg(short, long)]
        container_id: String,
    },

    /// 测试完整存储流程
    Test {
        /// 测试场景 (local, tar, dir)
        #[arg(short, long, default_value = "local")]
        scenario: String,
    },
}

fn setup_logger(verbose: bool) -> Logger {
    let decorator = slog_term::TermDecorator::new().build();
    let drain = slog_term::FullFormat::new(decorator).build().fuse();
    let drain = slog_async::Async::new(drain).build().fuse();

    // 优先级：-v/--verbose 标志 > RUNCELL_LOG 环境变量 > 默认 warn
    let level = if verbose {
        slog::Level::Debug
    } else {
        match std::env::var("RUNCELL_LOG").ok().as_deref() {
            Some("trace") => slog::Level::Trace,
            Some("debug") => slog::Level::Debug,
            Some("info") => slog::Level::Info,
            Some("warn" | "warning") => slog::Level::Warning,
            Some("error") => slog::Level::Error,
            Some("critical") => slog::Level::Critical,
            _ => slog::Level::Warning, // 默认 warn
        }
    };

    Logger::root(
        drain.filter_level(level).fuse(),
        o!("version" => env!("CARGO_PKG_VERSION")),
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    // 检查是否是 init 子进程调用
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "init" {
        // 这是容器的 init 进程，直接调用 init_child
        celler::container::init_child();
        return Ok(());
    }

    let cli = Cli::parse();

    let logger = setup_logger(cli.verbose);
    let _guard = slog_scope::set_global_logger(logger.clone());

    slog::info!(logger, "Runcell starting"; "command" => format!("{:?}", cli.command));

    match cli.command {
        Commands::Storage(storage_cmd) => {
            storage_cmd::handle_storage_command(storage_cmd, &logger).await?;
        }
        Commands::Container(container_cmd) => {
            container_cmd::handle_container_command(container_cmd, &logger).await?;
        }
    }

    slog::info!(logger, "Command completed successfully");

    Ok(())
}
