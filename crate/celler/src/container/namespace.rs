//! # Linux Namespace 管理模块
//!
//! 本模块负责容器 namespace 的创建、管理和协调工作。
//!
//! ## 主要功能
//! - 更新和配置 Linux namespace（pid, net, ipc, uts, mnt, user, cgroup）
//! - 父子进程之间的同步通信协议
//! - UID/GID 映射配置（user namespace）
//! - Cgroup 应用和资源限制设置
//! - OCI 钩子（prestart hooks）执行
//!
//! ## 工作流程
//! 1. 父进程创建子进程
//! 2. 通过管道进行同步通信
//! 3. 子进程设置 namespace
//! 4. 父进程配置 UID/GID 映射
//! 5. 父进程应用 cgroup
//! 6. 执行 prestart hooks
//! 7. 子进程执行容器命令

use std::{os::fd::RawFd, path::PathBuf};

use anyhow::{Result, anyhow};
use kata_sys_utils::hooks::HookStates;
use nix::{
    fcntl::{self, OFlag},
    sys::stat::Mode,
    unistd,
};
use oci_spec::runtime::{Linux, LinuxIdMapping, LinuxNamespace, Spec};
use runtime_spec::State as OCIState;
use slog::Logger;
use tokio::io::AsyncBufReadExt;

use super::types::TYPETONAME;
#[cfg(all(not(test), not(feature = "mock-cgroup")))]
use crate::cgroups::fs::Manager as FsManager;
#[cfg(any(test, feature = "mock-cgroup"))]
use crate::cgroups::mock::Manager as FsManager;
use crate::{
    cgroups::CgroupManager,
    pipe::{
        pipestream::PipeStream,
        sync::{SYNC_DATA, SYNC_SUCCESS},
        sync_with_async::{read_async, write_async},
    },
    process::Process,
};

/// 更新 OCI spec 中的 namespace 路径配置
///
/// 当容器需要加入已存在进程的 namespace 时（如 Kubernetes Pod 的 sandbox
/// 模式）， 需要将 namespace 路径指向该进程的 namespace 文件。
///
/// # 参数
/// - `logger`: 日志记录器
/// - `spec`: OCI 规范配置（可变引用）
/// - `init_pid`: 目标进程的 PID（容器将加入该进程的 namespace）
///
/// # 工作原理
/// 遍历 spec 中配置的所有 namespace，如果未指定路径，则自动设置为：
/// `/proc/{init_pid}/ns/{ns_type}`
///
/// 例如：`/proc/1234/ns/net` 表示加入 PID 1234 的网络 namespace
///
/// # 返回
/// - `Ok(())`: 成功更新 namespace 配置
/// - `Err(...)`: spec 缺少 linux 字段
pub fn update_namespaces(logger: &Logger, spec: &mut Spec, init_pid: RawFd) -> Result<()> {
    info!(logger, "updating namespaces for init pid fd {}", init_pid);
    let linux = spec
        .linux_mut()
        .as_mut()
        .ok_or_else(|| anyhow!("Spec didn't contain linux field"))?;

    if let Some(namespaces) = linux.namespaces_mut().as_mut() {
        for namespace in namespaces.iter_mut() {
            if TYPETONAME.contains_key(&namespace.typ()) {
                // 构造 namespace 文件路径：/proc/{pid}/ns/{type}
                let ns_path = format!(
                    "/proc/{}/ns/{}",
                    init_pid,
                    TYPETONAME.get(&namespace.typ()).unwrap()
                );

                // 如果未指定 namespace 路径，则使用目标进程的 namespace
                if namespace
                    .path()
                    .as_ref()
                    .is_none_or(|p| p.as_os_str().is_empty())
                {
                    namespace.set_path(Some(PathBuf::from(&ns_path)));
                }
            }
        }
    }

    Ok(())
}

/// 获取 PID namespace 配置信息
///
/// 从 OCI Linux 配置中提取 PID namespace 的相关信息，包括是否启用以及
/// namespace 文件描述符。
///
/// # 参数
/// - `logger`: 日志记录器
/// - `linux`: OCI Linux 配置
///
/// # 返回
/// `PidNs` 结构体，包含：
/// - `enabled`: PID namespace 是否启用
/// - `fd`: namespace 文件描述符（如果有路径则打开文件）
///
/// # PID Namespace 的三种情况
/// 1. **未配置 PID namespace**: 返回 `PidNs { enabled: false, fd: None }`
/// 2. **创建新的 PID namespace**: 配置了 pid 类型但无路径，返回 `PidNs {
///    enabled: true, fd: None }`
/// 3. **加入已存在的 PID namespace**: 指定了路径，打开文件并返回 `PidNs {
///    enabled: true, fd: Some(fd) }`
pub fn get_pid_namespace(logger: &Logger, linux: &Linux) -> Result<PidNs> {
    let linux_namespaces = linux.namespaces().clone().unwrap_or_default();
    for ns in &linux_namespaces {
        if &ns.typ().to_string() == "pid" {
            let fd = match ns.path() {
                // 情况 2：创建新的 PID namespace
                None => return Ok(PidNs::new(true, None)),
                // 情况 3：加入已存在的 PID namespace
                Some(ns_path) => fcntl::open(
                    ns_path.display().to_string().as_str(),
                    OFlag::O_RDONLY,
                    Mode::empty(),
                )
                .inspect_err(|e| {
                    error!(
                        logger,
                        "cannot open type: {} path: {}",
                        &ns.typ().to_string(),
                        ns_path.display()
                    );
                    error!(logger, "error is : {:?}", e)
                })?,
            };

            return Ok(PidNs::new(true, Some(fd)));
        }
    }

    // 情况 1：未配置 PID namespace
    Ok(PidNs::new(false, None))
}

/// 检查是否启用了 User Namespace
///
/// User namespace 允许容器内部使用不同的 UID/GID 映射，实现非 root
/// 用户运行容器。
///
/// # 判断条件
/// - 配置中存在类型为 "user" 的 namespace
/// - 且未指定路径（表示创建新的 user namespace，而非加入已存在的）
///
/// # 参数
/// - `linux`: OCI Linux 配置
///
/// # 返回
/// - `true`: 需要创建新的 user namespace
/// - `false`: 不启用 user namespace 或加入已存在的 user namespace
fn is_userns_enabled(linux: &Linux) -> bool {
    linux
        .namespaces()
        .clone()
        .unwrap_or_default()
        .iter()
        .any(|ns| &ns.typ().to_string() == "user" && ns.path().is_none())
}

/// 获取 namespace 列表的副本
///
/// 从 OCI Linux 配置中提取所有 namespace 配置。
///
/// # 参数
/// - `linux`: OCI Linux 配置
///
/// # 返回
/// namespace 列表（克隆的副本）
pub(super) fn get_namespaces(linux: &Linux) -> Vec<LinuxNamespace> {
    linux
        .namespaces()
        .clone()
        .unwrap_or_default()
        .iter()
        .map(|ns| {
            let mut namespace = LinuxNamespace::default();
            namespace.set_typ(ns.typ());
            namespace.set_path(ns.path().clone());

            namespace
        })
        .collect()
}

/// 设置子进程日志转发任务
///
/// 创建一个异步任务，从管道读取子进程的日志输出并转发到父进程的日志系统。
/// 这使得容器内部的日志可以通过父进程统一管理。
///
/// # 参数
/// - `fd`: 日志管道的文件描述符（读端）
/// - `child_logger`: 子进程专用的日志记录器
///
/// # 返回
/// Tokio 异步任务句柄，可用于等待任务完成
///
/// # 工作流程
/// 1. 将文件描述符包装为异步 PipeStream
/// 2. 使用 BufReader 按行读取
/// 3. 将每行内容通过 child_logger 记录
/// 4. 遇到错误或 EOF 时结束任务
pub fn setup_child_logger(fd: RawFd, child_logger: Logger) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let log_file_stream = PipeStream::from_fd(fd);
        let buf_reader_stream = tokio::io::BufReader::new(log_file_stream);
        let mut lines = buf_reader_stream.lines();

        loop {
            match lines.next_line().await {
                Err(e) => {
                    info!(child_logger, "read child process log error: {:?}", e);
                    break;
                }
                Ok(Some(line)) => {
                    // 转发日志内容
                    info!(child_logger, "{}", line);
                }
                Ok(None) => {
                    info!(child_logger, "read child process log end",);
                    break;
                }
            }
        }
    })
}

/// 加入 namespace 并完成容器初始化协调
///
/// 这是容器启动过程中最核心的函数，负责父子进程之间的同步协调，
/// 完成 namespace 设置、cgroup 应用、UID/GID 映射等关键步骤。
///
/// # 参数
/// - `logger`: 日志记录器
/// - `spec`: OCI 规范配置
/// - `p`: 进程信息（包含 PID、是否为 init 进程等）
/// - `cm`: Cgroup 管理器
/// - `use_systemd_cgroup`: 是否使用 systemd cgroup
/// - `st`: OCI 状态信息
/// - `pipe_w`: 管道写端（父进程向子进程发送数据）
/// - `pipe_r`: 管道读端（父进程从子进程接收确认）
///
/// # 同步协议流程
///
/// ```text
/// 父进程                          子进程
///   |                              |
///   |-- [1] 发送 OCI Spec -------->|
///   |<- [2] 确认接收 Spec ---------|
///   |                              |
///   |-- [3] 发送 Process 配置 ---->|
///   |<- [4] 确认接收 Process ------|
///   |                              |
///   |-- [5] 发送 OCI State ------->|
///   |<- [6] 确认接收 State --------|
///   |                              |
///   |-- [7] 发送 Cgroup 管理器 --->|
///   |<- [8] 子进程设置 user ns ----|
///   |                              |
///   |-- [9] 配置 UID/GID 映射 ---->| (仅在启用 user namespace 时)
///   |                              |
///   |-- [10] 应用 cgroup 限制 ---->|
///   |-- [11] 设置 cgroup 资源 ---->|
///   |                              |
///   |-- [12] 通知子进程继续 ------>|
///   |<- [13] 子进程准备就绪 -------|
///   |                              |
///   |-- [14] 执行 Prestart Hook -->| (仅 init 进程)
///   |                              |
///   |-- [15] Hook 执行完成 ------->|
///   |<- [16] 准备执行容器命令 -----|
/// ```
///
/// # 关键操作
///
/// ## 1. User Namespace 设置
/// - 检测是否启用 user namespace
/// - 父进程写入 `/proc/{pid}/uid_map` 和 `/proc/{pid}/gid_map`
/// - 实现容器内外不同的 UID/GID 映射
///
/// ## 2. Cgroup 应用顺序
/// - **FsManager**: apply 和 set 的顺序无关紧要
/// - **SystemdManager**: 必须先 apply 再 set（因为需要先创建 systemd unit）
///
/// ## 3. Prestart Hook 执行
/// - 仅在 init 进程时执行
/// - 在容器命令执行前运行
/// - 在父进程（agent）的 namespace 中执行
/// - 可用于容器启动前的准备工作（如网络配置）
///
/// # 返回
/// - `Ok(())`: 成功完成所有初始化步骤
/// - `Err(...)`: 任何步骤失败
///
/// # 错误处理
/// 任何步骤失败都会导致整个函数返回错误，调用方应清理资源。
#[allow(clippy::too_many_arguments)]
pub(super) async fn join_namespaces(
    logger: &Logger,
    spec: &Spec,
    p: &Process,
    cm: &(dyn CgroupManager + Send + Sync),
    use_systemd_cgroup: bool,
    st: &OCIState,
    pipe_w: &mut PipeStream,
    pipe_r: &mut PipeStream,
) -> Result<()> {
    let logger = logger.new(o!("action" => "join-namespaces"));

    let linux = spec
        .linux()
        .as_ref()
        .ok_or_else(|| anyhow!("Spec didn't contain linux field"))?;
    let res = linux.resources().as_ref();

    let userns = is_userns_enabled(linux);

    // === 步骤 1-2: 发送 OCI Spec ===
    info!(logger, "try to send spec from parent to child");
    let spec_str = serde_json::to_string(spec)?;
    write_async(pipe_w, SYNC_DATA, spec_str.as_str()).await?;

    info!(logger, "wait child received oci spec");
    read_async(pipe_r).await?;

    // === 步骤 3-4: 发送 Process 配置 ===
    info!(logger, "send oci process from parent to child");
    let process_str = serde_json::to_string(&p.oci)?;
    write_async(pipe_w, SYNC_DATA, process_str.as_str()).await?;

    info!(logger, "wait child received oci process");
    read_async(pipe_r).await?;

    // === 步骤 5-6: 发送 OCI State ===
    info!(logger, "try to send state from parent to child");
    let state_str = serde_json::to_string(st)?;
    write_async(pipe_w, SYNC_DATA, state_str.as_str()).await?;

    info!(logger, "wait child received oci state");
    read_async(pipe_r).await?;

    // === 步骤 7: 发送 Cgroup 管理器 ===
    let cm_str = if use_systemd_cgroup {
        todo!("systemd cgroup manager is not supported yet")
    } else {
        serde_json::to_string(cm.as_any()?.downcast_ref::<FsManager>().unwrap())
    }?;
    write_async(pipe_w, SYNC_DATA, cm_str.as_str()).await?;

    // === 步骤 8: 等待子进程设置 user namespace ===
    info!(logger, "wait child setup user namespace");
    read_async(pipe_r).await?;

    // === 步骤 9: 配置 UID/GID 映射（仅在启用 user namespace 时）===
    if userns {
        info!(logger, "setup uid/gid mappings");
        let uid_mappings = linux.uid_mappings().clone().unwrap_or_default();
        let gid_mappings = linux.gid_mappings().clone().unwrap_or_default();
        // setup uid/gid mappings
        write_mappings(&logger, &format!("/proc/{}/uid_map", p.pid), &uid_mappings)?;
        write_mappings(&logger, &format!("/proc/{}/gid_map", p.pid), &gid_mappings)?;
    }

    // === 步骤 10-11: 应用 cgroups ===
    // For FsManger, it's no matter about the order of apply and set.
    // For SystemdManger, apply must be precede set because we can only create a
    // systemd unit with specific processes(pids).
    if res.is_some() {
        info!(logger, "apply processes to cgroups!");
        cm.apply(p.pid)?;
    }

    if p.init && res.is_some() {
        info!(logger, "set properties to cgroups!");
        cm.set(res.unwrap(), false)?;
    }

    // === 步骤 12: 通知子进程继续 ===
    info!(logger, "notify child to continue");
    // notify child to continue
    write_async(pipe_w, SYNC_SUCCESS, "").await?;

    // === 步骤 13-15: 执行 Prestart Hook（仅 init 进程）===
    if p.init {
        info!(logger, "notify child parent ready to run prestart hook!");
        read_async(pipe_r).await?;

        info!(logger, "get ready to run prestart hook!");

        // guest Prestart hook
        // * should be executed during the start operation, and before the container
        //   command is executed
        // * the executable file is in agent namespace
        // * should also be executed in agent namespace.
        if let Some(hooks) = spec.hooks().as_ref() {
            info!(logger, "guest Prestart hook");
            let mut hook_states = HookStates::new();
            hook_states.execute_hooks(
                hooks.prestart().clone().unwrap_or_default().as_slice(),
                Some(st.clone()),
            )?;
        }

        // notify child run prestart hooks completed
        info!(logger, "notify child run prestart hook completed!");
        write_async(pipe_w, SYNC_SUCCESS, "").await?;
    }

    // === 步骤 16: 等待子进程准备执行容器命令 ===
    info!(logger, "wait for child process ready to run exec");
    read_async(pipe_r).await?;

    Ok(())
}

/// 写入 UID/GID 映射配置到 procfs
///
/// 在 user namespace 中，需要配置容器内外的 UID/GID 映射关系。
/// 这个函数将映射规则写入到 `/proc/{pid}/uid_map` 或 `/proc/{pid}/gid_map`。
///
/// # 参数
/// - `logger`: 日志记录器
/// - `path`: 映射文件路径（`/proc/{pid}/uid_map` 或 `/proc/{pid}/gid_map`）
/// - `maps`: ID 映射规则列表
///
/// # 映射格式
/// 每个映射规则包含三个字段（空格分隔）：
/// ```text
/// <container_id> <host_id> <size>
/// ```
///
/// 例如：
/// ```text
/// 0 1000 1     # 容器内的 UID 0 映射到主机的 UID 1000，范围为 1 个 ID
/// 1 100000 65536  # 容器内的 UID 1-65536 映射到主机的 UID 100000-165535
/// ```
///
/// # 约束条件
/// - 只能写入一次（写入后文件会变为只读）
/// - 必须在子进程加入 user namespace 后，但在执行任何命令前完成
/// - size 为 0 的映射会被忽略
///
/// # 返回
/// - `Ok(())`: 成功写入映射
/// - `Err(...)`: 打开文件或写入失败
fn write_mappings(logger: &Logger, path: &str, maps: &[LinuxIdMapping]) -> Result<()> {
    // 构造映射数据字符串
    let data = maps
        .iter()
        .filter(|m| m.size() != 0) // 忽略 size 为 0 的映射
        .map(|m| format!("{} {} {}\n", m.container_id(), m.host_id(), m.size()))
        .collect::<Vec<_>>()
        .join("");

    info!(logger, "mapping: {}", data);
    if !data.is_empty() {
        let fd = fcntl::open(path, OFlag::O_WRONLY, Mode::empty())?;
        defer!(unistd::close(fd).unwrap()); // 确保文件描述符被关闭
        unistd::write(fd, data.as_bytes())
            .inspect_err(|_| info!(logger, "cannot write mapping"))?;
    }
    Ok(())
}

/// PID Namespace 配置信息
///
/// 用于存储 PID namespace 的状态和文件描述符。
///
/// # 字段
/// - `enabled`: 是否启用 PID namespace
/// - `fd`: namespace 文件描述符（加入已存在的 PID namespace 时使用）
///
/// # 使用场景
///
/// ## 场景 1: 未使用 PID namespace
/// ```rust
/// PidNs {
///     enabled: false,
///     fd: None,
/// }
/// ```
/// 容器与主机共享 PID namespace，可以看到主机上的所有进程。
///
/// ## 场景 2: 创建新的 PID namespace
/// ```rust
/// PidNs {
///     enabled: true,
///     fd: None,
/// }
/// ```
/// 容器拥有独立的 PID 空间，init 进程的 PID 为 1。
///
/// ## 场景 3: 加入已存在的 PID namespace
/// ```rust
/// PidNs {
///     enabled: true,
///     fd: Some(fd),
/// }
/// ```
/// 容器加入另一个进程的 PID namespace（如 Kubernetes Pod 的 sandbox）。
#[derive(Debug, Clone)]
pub struct PidNs {
    /// 是否启用 PID namespace
    pub(super) enabled: bool,
    /// namespace 文件描述符（可选）
    pub(super) fd: Option<i32>,
}

impl PidNs {
    /// 创建新的 PidNs 实例
    ///
    /// # 参数
    /// - `enabled`: 是否启用 PID namespace
    /// - `fd`: namespace 文件描述符（如果要加入已存在的 namespace）
    pub fn new(enabled: bool, fd: Option<i32>) -> Self {
        Self { enabled, fd }
    }
}
