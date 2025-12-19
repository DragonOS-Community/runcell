//! 容器管理命令实现

use std::{
    fs,
    path::Path,
    sync::{Arc, RwLock},
    time::Duration,
};

use anyhow::{Context, Result};
use celler::{
    cgroups::DevicesCgroupInfo,
    container::{is_process_running, load_container_state, LinuxContainer},
    process::Process,
    specconf::CreateOpts,
};
use nix::{
    sys::signal::{self, Signal},
    unistd::Pid,
};
use oci_spec::runtime::Spec;
use slog::Logger;

use crate::ContainerCommands;

/// Bundle 基础目录
const BUNDLE_BASE: &str = "/tmp/runcell/bundles";

/// Container 状态目录  
const CONTAINER_STATE_BASE: &str = "/tmp/runcell/states";

/// 处理容器相关命令
pub async fn handle_container_command(cmd: ContainerCommands, logger: &Logger) -> Result<()> {
    match cmd {
        ContainerCommands::Create { id, rootfs, bundle } => {
            create_container(&id, &rootfs, bundle.as_deref(), logger).await?;
        }
        ContainerCommands::Run {
            id,
            image,
            command,
            args,
            tty,
            interactive,
            detach,
        } => {
            run_container(
                &id,
                &image,
                &command,
                &args,
                tty,
                interactive,
                detach,
                logger,
            )
            .await?;
        }
        ContainerCommands::Start { id } => {
            start_container(&id, logger).await?;
        }
        ContainerCommands::Delete { id } => {
            delete_container(&id, logger).await?;
        }
        ContainerCommands::List { format, all } => {
            list_containers(&format, all, logger).await?;
        }
        ContainerCommands::Exec {
            id,
            command,
            args,
            tty,
            interactive,
        } => {
            exec_in_container(&id, &command, &args, tty, interactive, logger).await?;
        }
    }

    Ok(())
}

/// 创建容器
async fn create_container(
    id: &str,
    rootfs: &str,
    bundle: Option<&str>,
    logger: &Logger,
) -> Result<()> {
    slog::info!(logger, "创建容器"; "id" => id, "rootfs" => rootfs);

    // 确定 bundle 目录
    let bundle_path = bundle
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{}/{}", BUNDLE_BASE, id));

    // 创建 bundle 目录
    fs::create_dir_all(&bundle_path)
        .with_context(|| format!("无法创建 bundle 目录: {}", bundle_path))?;

    // 生成最小化 OCI spec
    let spec = create_minimal_spec(rootfs, &["/bin/sh".to_string()], false)?;

    // 保存 config.json
    let config_path = format!("{}/config.json", bundle_path);
    spec.save(&config_path)
        .with_context(|| format!("无法保存 config.json 到 {}", config_path))?;

    slog::info!(logger, "容器配置已生成"; "config" => &config_path);

    Ok(())
}

/// 运行容器（创建+启动）
async fn run_container(
    id: &str,
    image: &str,
    command: &str,
    args: &[String],
    tty: bool,
    interactive: bool,
    detach: bool,
    logger: &Logger,
) -> Result<()> {
    slog::info!(logger, "运行容器"; "id" => id, "image" => image, "command" => command,
        "tty" => tty, "interactive" => interactive, "detach" => detach);

    // 1. 拉取镜像
    slog::info!(logger, "正在拉取镜像...");
    let rootfs = storage::image::pull_and_extract(image, id, logger).await?;
    slog::info!(logger, "镜像拉取成功"; "rootfs" => &rootfs);

    // 2. 确定 bundle 目录
    let bundle_path = format!("{}/{}", BUNDLE_BASE, id);
    fs::create_dir_all(&bundle_path)?;

    // 3. 生成 OCI spec（带TTY支持）
    let mut cmd_args = vec![command.to_string()];
    cmd_args.extend(args.iter().cloned());

    let spec = create_minimal_spec(&rootfs, &cmd_args, tty)?;

    // 4. 保存 config.json
    let config_path = format!("{}/config.json", bundle_path);
    spec.save(&config_path)?;

    slog::info!(logger, "OCI 配置已生成"; "config" => &config_path);

    // 5. 创建容器实例
    let create_opts = CreateOpts {
        cgroup_name: id.to_string(),
        use_systemd_cgroup: false,
        no_pivot_root: false,
        no_new_keyring: false,
        spec: Some(spec.clone()),
        rootless_euid: false,
        rootless_cgroup: false,
        container_name: id.to_string(),
    };

    let devcg_info = Some(Arc::new(RwLock::new(DevicesCgroupInfo::default())));

    slog::info!(logger, "正在创建容器实例...");

    let mut container =
        LinuxContainer::new(id, CONTAINER_STATE_BASE, devcg_info, create_opts, logger)?;

    slog::info!(logger, "容器创建成功！"; "id" => id);

    // 6. 创建并启动进程
    slog::info!(logger, "正在创建容器进程...");

    // 从 spec 中获取 process 配置
    let oci_process = spec
        .process()
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("OCI spec 中缺少 process 配置"))?
        .clone();

    // 创建 Process 实例
    let process = Process::new(
        logger,
        &oci_process,
        id,   // exec_id
        true, // init process
        0,    // pipe_size (0 = default)
        None, // proc_io (None for simple case)
    )
    .context("创建 Process 失败")?;

    slog::info!(logger, "正在启动容器...");

    // 启动容器（包括 start + exec）
    container
        .run_container(process)
        .await
        .context("启动容器失败")?;

    slog::info!(logger, "容器启动成功！"; "id" => id);

    if detach {
        slog::info!(logger, "容器正在后台运行"; "id" => id);
    } else {
        slog::info!(logger, "容器正在运行..."; "id" => id);
        // TODO: 如果是交互模式，这里应该等待容器退出
        // 目前简单返回，后续可以添加等待逻辑
    }

    Ok(())
}

/// 启动已创建的容器
async fn start_container(id: &str, logger: &Logger) -> Result<()> {
    slog::info!(logger, "启动容器"; "id" => id);
    slog::warn!(logger, "start 命令暂未完全实现");

    Ok(())
}

/// 删除容器
///
/// 执行以下步骤：
/// 1. 读取 state.json 获取容器 PID
/// 2. 如果进程仍在运行，发送 SIGKILL 信号
/// 3. 清理 bundle 目录
/// 4. 清理状态目录
/// 5. 清理镜像
async fn delete_container(id: &str, logger: &Logger) -> Result<()> {
    slog::info!(logger, "删除容器"; "id" => id);

    // 1. 尝试读取状态并 kill 进程
    match load_container_state(CONTAINER_STATE_BASE, id) {
        Ok(state) => {
            if state.init_process_pid > 0 {
                if is_process_running(state.init_process_pid) {
                    slog::info!(logger, "正在终止容器进程";
                        "pid" => state.init_process_pid);

                    // 发送 SIGKILL 信号
                    match signal::kill(Pid::from_raw(state.init_process_pid), Signal::SIGKILL) {
                        Ok(_) => {
                            slog::info!(logger, "SIGKILL 信号已发送";
                                "pid" => state.init_process_pid);
                            // 等待进程退出
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        Err(e) => {
                            slog::warn!(logger, "发送 SIGKILL 失败";
                                "pid" => state.init_process_pid,
                                "error" => format!("{:?}", e));
                        }
                    }
                } else {
                    slog::info!(logger, "容器进程已不存在";
                        "pid" => state.init_process_pid);
                }
            }
        }
        Err(e) => {
            slog::debug!(logger, "无法读取容器状态（可能容器从未启动）";
                "error" => format!("{:?}", e));
        }
    }

    // 2. 清理 bundle
    let bundle_path = format!("{}/{}", BUNDLE_BASE, id);
    if Path::new(&bundle_path).exists() {
        fs::remove_dir_all(&bundle_path)?;
        slog::info!(logger, "Bundle 已删除"; "path" => &bundle_path);
    }

    // 3. 清理容器状态
    let state_path = format!("{}/{}", CONTAINER_STATE_BASE, id);
    if Path::new(&state_path).exists() {
        fs::remove_dir_all(&state_path)?;
        slog::info!(logger, "容器状态已删除"; "path" => &state_path);
    }

    // 4. 清理镜像
    storage::image::cleanup_image(id, logger)?;

    slog::info!(logger, "容器删除完成"; "id" => id);

    Ok(())
}

/// 创建最小化的 OCI Spec
///
/// 这是一个简化版本，用于快速测试容器创建流程
fn create_minimal_spec(rootfs: &str, args: &[String], terminal: bool) -> Result<Spec> {
    // 从文件加载默认 spec 或创建一个基础的
    // 这里我们使用 oci_spec 的 builder 模式

    use oci_spec::runtime::{ProcessBuilder, RootBuilder, SpecBuilder};

    let process = ProcessBuilder::default()
        .args(args.to_vec())
        .terminal(terminal)  // 设置 TTY
        .build()?;

    let root = RootBuilder::default().path(rootfs).build()?;

    let spec = SpecBuilder::default()
        .version("1.0.0")
        .process(process)
        .root(root)
        .build()?;

    Ok(spec)
}

/// 列出所有容器
///
/// 遍历状态目录，读取每个容器的 state.json 文件，
/// 验证进程状态并格式化输出。
async fn list_containers(format: &str, show_all: bool, logger: &Logger) -> Result<()> {
    slog::info!(logger, "列出容器"; "format" => format, "all" => show_all);

    let state_dir = Path::new(CONTAINER_STATE_BASE);
    if !state_dir.exists() {
        // 状态目录不存在，输出空列表
        if format == "json" {
            println!("[]");
        } else {
            println!(
                "{:<20} {:<8} {:<10} {:<20} {}",
                "CONTAINER ID", "PID", "STATUS", "CREATED", "ROOTFS"
            );
        }
        return Ok(());
    }

    let mut containers = Vec::new();

    // 遍历状态目录
    for entry in fs::read_dir(state_dir)? {
        let entry = entry?;
        let container_id = entry.file_name().to_string_lossy().to_string();

        // 尝试读取状态文件
        match load_container_state(CONTAINER_STATE_BASE, &container_id) {
            Ok(state) => {
                // 验证实际进程状态
                let actual_status = if is_process_running(state.init_process_pid) {
                    "Running"
                } else {
                    "Stopped"
                };

                // 如果不是 --all，则跳过已停止的容器
                if !show_all && actual_status == "Stopped" {
                    continue;
                }

                // 格式化创建时间
                let created = chrono::DateTime::from_timestamp(state.created as i64, 0)
                    .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
                    .unwrap_or_else(|| "Unknown".to_string());

                containers.push((
                    container_id,
                    state.init_process_pid,
                    actual_status.to_string(),
                    created,
                    state.rootfs,
                ));
            }
            Err(e) => {
                slog::debug!(logger, "跳过无效容器";
                    "container" => &container_id,
                    "error" => format!("{:?}", e));
            }
        }
    }

    // 输出结果
    if format == "json" {
        let json_output: Vec<serde_json::Value> = containers
            .iter()
            .map(|(id, pid, status, created, rootfs)| {
                serde_json::json!({
                    "id": id,
                    "pid": pid,
                    "status": status,
                    "created": created,
                    "rootfs": rootfs,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json_output)?);
    } else {
        // 表格输出
        println!(
            "{:<20} {:<8} {:<10} {:<20} {}",
            "CONTAINER ID", "PID", "STATUS", "CREATED", "ROOTFS"
        );
        for (id, pid, status, created, rootfs) in &containers {
            // 截断过长的 ID 和 rootfs
            let id_display = if id.len() > 18 { &id[..18] } else { id };
            let rootfs_display = if rootfs.len() > 40 {
                format!("...{}", &rootfs[rootfs.len() - 37..])
            } else {
                rootfs.clone()
            };
            println!(
                "{:<20} {:<8} {:<10} {:<20} {}",
                id_display, pid, status, created, rootfs_display
            );
        }
    }

    slog::info!(logger, "找到容器"; "count" => containers.len());

    Ok(())
}

/// 在运行中的容器内执行命令
///
/// 通过进入容器的 namespace 来执行指定命令。
async fn exec_in_container(
    id: &str,
    command: &str,
    args: &[String],
    tty: bool,
    interactive: bool,
    logger: &Logger,
) -> Result<()> {
    slog::info!(logger, "在容器内执行命令";
        "id" => id, "command" => command, "tty" => tty, "interactive" => interactive);

    // 1. 读取容器状态
    let state = load_container_state(CONTAINER_STATE_BASE, id)
        .with_context(|| format!("容器 '{}' 不存在或未运行", id))?;

    // 2. 验证容器正在运行
    if !is_process_running(state.init_process_pid) {
        return Err(anyhow::anyhow!(
            "容器 '{}' 未运行 (PID {} 不存在)",
            id,
            state.init_process_pid
        ));
    }

    slog::info!(logger, "找到运行中的容器";
        "pid" => state.init_process_pid);

    // 3. 构建 nsenter 命令进入容器
    // 使用 nsenter 是最简单可靠的方式进入容器 namespace
    let pid = state.init_process_pid;

    let mut nsenter_args = vec![
        format!("--target={}", pid),
        "--mount".to_string(),
        "--uts".to_string(),
        "--ipc".to_string(),
        "--net".to_string(),
        "--pid".to_string(),
    ];

    // 添加要执行的命令
    nsenter_args.push("--".to_string());
    nsenter_args.push(command.to_string());
    nsenter_args.extend(args.iter().cloned());

    slog::info!(logger, "执行 nsenter"; "args" => format!("{:?}", nsenter_args));

    // 4. 执行 nsenter
    let status = std::process::Command::new("nsenter")
        .args(&nsenter_args)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .context("执行 nsenter 失败")?;

    if !status.success() {
        return Err(anyhow::anyhow!("命令执行失败，退出码: {:?}", status.code()));
    }

    slog::info!(logger, "命令执行完成");

    Ok(())
}
