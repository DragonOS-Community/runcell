//! # 容器类型定义模块
//!
//! 本模块定义了容器运行时的核心类型、常量、trait 和数据结构。
//!
//! ## 主要内容
//! - **常量定义**: 环境变量名、文件描述符名称、错误消息等
//! - **状态管理**: `ContainerStatus` - 容器生命周期状态跟踪
//! - **Trait 定义**: `BaseContainer`、`Container` - 容器接口规范
//! - **全局配置**: Namespace 映射、默认设备列表等
//!
//! ## 容器生命周期
//! ```text
//! Created → Running → Paused → Stopped
//!    ↓         ↓        ↓
//!    └────────→ Stopped ←────┘
//! ```

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use anyhow::Result;
use async_trait::async_trait;
use libc::pid_t;
use nix::sched::CloneFlags;
use oci_spec::runtime::{self as oci, LinuxDevice, LinuxResources};
use protocols::agent::StatsContainerResponse;
use regex::Regex;
use runtime_spec::{ContainerState, State as OCIState};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::Config;
use crate::process::Process;

/// Namespace 类型的字符串别名
type NamespaceType = String;

// ============================================================================
// 常量定义：文件名和环境变量
// ============================================================================

/// Exec FIFO 文件名
///
/// 用于同步容器启动过程。父进程创建此 FIFO，子进程打开并等待，
/// 当父进程准备好后，打开 FIFO 写端，子进程即可继续执行。
pub const EXEC_FIFO_FILENAME: &str = "exec.fifo";

// ----------------------------------------------------------------------------
// 环境变量名称常量
// ----------------------------------------------------------------------------

/// 环境变量：标识是否为 init 进程
pub const INIT: &str = "INIT";

/// 环境变量：是否跳过 pivot_root（某些情况下无法使用 pivot_root）
pub const NO_PIVOT: &str = "NO_PIVOT";

/// 环境变量：子进程管道读端文件描述符
pub const CRFD_FD: &str = "CRFD_FD";

/// 环境变量：子进程管道写端文件描述符
pub const CWFD_FD: &str = "CWFD_FD";

/// 环境变量：子进程日志管道文件描述符
pub const CLOG_FD: &str = "CLOG_FD";

/// 环境变量：Exec FIFO 文件描述符
pub const FIFO_FD: &str = "FIFO_FD";

/// 环境变量：HOME 目录
pub const HOME_ENV_KEY: &str = "HOME";

/// 环境变量：PID namespace 文件描述符
pub const PIDNS_FD: &str = "PIDNS_FD";

/// 环境变量：是否启用 PID namespace
pub const PIDNS_ENABLED: &str = "PIDNS_ENABLED";

/// 环境变量：控制台套接字文件描述符（用于传递 pty master）
pub const CONSOLE_SOCKET_FD: &str = "CONSOLE_SOCKET_FD";

// ----------------------------------------------------------------------------
// 错误消息常量
// ----------------------------------------------------------------------------

/// 错误消息：缺少 Linux 配置
pub const MissingLinux: &str = "no linux config";

/// 错误消息：无效的 namespace 类型
pub const InvalidNamespace: &str = "invalid namespace type";

// ============================================================================
// 容器状态管理
// ============================================================================

/// 容器状态追踪器
///
/// 维护容器的当前状态和前一个状态，用于状态转换验证和审计。
///
/// # 状态枚举
/// - `Created`: 容器已创建但未启动
/// - `Running`: 容器正在运行
/// - `Paused`: 容器已暂停（使用 freezer cgroup）
/// - `Stopped`: 容器已停止
///
/// # 示例
/// ```rust
/// let mut status = ContainerStatus::new();
/// assert_eq!(status.status(), ContainerState::Created);
///
/// status.transition(ContainerState::Running);
/// assert_eq!(status.status(), ContainerState::Running);
/// assert_eq!(status.pre_status, ContainerState::Created);
/// ```
#[derive(Debug)]
pub struct ContainerStatus {
    /// 前一个状态
    pub(super) pre_status: ContainerState,
    /// 当前状态
    pub(super) cur_status: ContainerState,
}

impl ContainerStatus {
    /// 创建新的状态追踪器，初始状态为 Created
    pub fn new() -> Self {
        ContainerStatus {
            pre_status: ContainerState::Created,
            cur_status: ContainerState::Created,
        }
    }

    /// 获取当前状态
    pub fn status(&self) -> ContainerState {
        self.cur_status
    }

    /// 状态转换
    ///
    /// 将当前状态保存为前一个状态，然后更新为新状态。
    ///
    /// # 参数
    /// - `to`: 目标状态
    pub fn transition(&mut self, to: ContainerState) {
        self.pre_status = self.status();
        self.cur_status = to;
    }
}

impl Default for ContainerStatus {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// 容器状态持久化结构
// ============================================================================

/// 容器基础状态
///
/// 包含容器的基本信息，可序列化保存到磁盘。
#[derive(Serialize, Deserialize, Debug)]
pub struct BaseState {
    /// 容器 ID
    #[serde(default, skip_serializing_if = "String::is_empty")]
    id: String,
    /// Init 进程的 PID
    #[serde(default)]
    init_process_pid: i32,
    /// Init 进程启动时间戳
    #[serde(default)]
    init_process_start: u64,
}

/// 容器完整状态
///
/// 扩展的容器状态信息，包含 namespace、cgroup 路径等详细信息。
/// 用于容器重启后的状态恢复。
#[derive(Serialize, Deserialize, Debug)]
pub struct State {
    /// 基础状态
    base: BaseState,
    /// 是否为 rootless 容器（非 root 用户运行）
    #[serde(default)]
    rootless: bool,
    /// Cgroup 路径映射（subsystem → 路径）
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    cgroup_paths: HashMap<String, String>,
    /// Namespace 路径映射（类型 → 路径）
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    namespace_paths: HashMap<NamespaceType, String>,
    /// 外部文件描述符列表
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    external_descriptors: Vec<String>,
    /// Intel RDT（Resource Director Technology）路径
    #[serde(default, skip_serializing_if = "String::is_empty")]
    intel_rdt_path: String,
}

/// 父子进程同步通信的数据结构
///
/// 用于在管道中传递进程 PID 信息。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SyncPc {
    /// 进程 PID
    #[serde(default)]
    pid: pid_t,
}

// ============================================================================
// 容器接口 Trait 定义
// ============================================================================

/// 容器基础接口
///
/// 定义所有容器实现必须提供的基本操作。
/// 使用 `async_trait` 支持异步方法。
///
/// # 生命周期管理方法
/// - `start()`: 启动容器进程
/// - `run()`: 创建并运行容器（start 的便捷封装）
/// - `exec()`: 在运行中的容器内执行新进程
/// - `destroy()`: 销毁容器并清理资源
///
/// # 信息查询方法
/// - `id()`: 获取容器 ID
/// - `status()`: 获取当前状态
/// - `state()`: 获取完整状态信息
/// - `oci_state()`: 获取 OCI 标准状态
/// - `config()`: 获取容器配置
/// - `processes()`: 获取所有进程 PID 列表
/// - `stats()`: 获取资源使用统计
///
/// # 资源管理方法
/// - `set_resources()`: 更新资源限制
#[async_trait]
pub trait BaseContainer {
    /// 获取容器 ID
    fn id(&self) -> String;

    /// 获取容器当前状态
    fn status(&self) -> ContainerState;

    /// 获取容器完整状态（用于持久化）
    fn state(&self) -> Result<State>;

    /// 获取 OCI 标准格式的状态信息
    fn oci_state(&self) -> Result<OCIState>;

    /// 获取容器配置
    fn config(&self) -> Result<&Config>;

    /// 获取容器中所有进程的 PID 列表
    fn processes(&self) -> Result<Vec<i32>>;

    /// 根据执行 ID 获取进程的可变引用
    ///
    /// # 参数
    /// - `eid`: 执行 ID（exec ID），用于标识容器内的特定进程
    fn get_process_mut(&mut self, eid: &str) -> Result<&mut Process>;

    /// 获取容器资源使用统计信息
    ///
    /// 包括 CPU、内存、块 I/O、网络等指标。
    fn stats(&self) -> Result<StatsContainerResponse>;

    /// 更新容器资源限制
    ///
    /// # 参数
    /// - `config`: 新的资源配置
    fn set_resources(&mut self, config: LinuxResources) -> Result<()>;

    /// 启动容器进程
    ///
    /// # 参数
    /// - `p`: 进程配置信息
    async fn start(&mut self, p: Process) -> Result<()>;

    /// 创建并运行容器（便捷方法）
    ///
    /// # 参数
    /// - `p`: 进程配置信息
    async fn run(&mut self, p: Process) -> Result<()>;

    /// 销毁容器并清理所有资源
    ///
    /// 包括停止进程、删除 cgroup、卸载文件系统等。
    async fn destroy(&mut self) -> Result<()>;

    /// 在运行中的容器内执行新进程
    async fn exec(&mut self) -> Result<()>;
}

/// 容器扩展接口
///
/// 提供容器的暂停/恢复功能（使用 freezer cgroup）。
///
/// # 使用场景
/// - 容器迁移前暂停
/// - 资源不足时临时冻结低优先级容器
/// - 调试时冻结容器状态
pub trait Container: BaseContainer {
    /// 暂停容器（冻结所有进程）
    ///
    /// 使用 freezer cgroup 将容器内所有进程设置为 FROZEN 状态。
    fn pause(&mut self) -> Result<()>;

    /// 恢复容器（解冻所有进程）
    ///
    /// 将 freezer cgroup 状态设置为 THAWED。
    fn resume(&mut self) -> Result<()>;
}

// ============================================================================
// 全局静态配置
// ============================================================================

lazy_static! {
    /// 进程等待锁
    ///
    /// 确保子进程退出信号被正确的接收者捕获，防止竞争条件。
    /// 在 fork 子进程并等待其退出时使用此锁进行同步。
    pub static ref WAIT_PID_LOCKER: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));

    /// Namespace 名称到 Clone Flag 的映射
    ///
    /// 将字符串形式的 namespace 名称映射到对应的 `clone()` 系统调用标志。
    ///
    /// # 支持的 Namespace
    /// | 名称 | Clone Flag | 功能 |
    /// |------|-----------|------|
    /// | user | CLONE_NEWUSER | 用户和组 ID 隔离 |
    /// | ipc | CLONE_NEWIPC | IPC 对象隔离 |
    /// | pid | CLONE_NEWPID | 进程 ID 隔离 |
    /// | net | CLONE_NEWNET | 网络栈隔离 |
    /// | mnt | CLONE_NEWNS | 挂载点隔离 |
    /// | uts | CLONE_NEWUTS | 主机名和域名隔离 |
    /// | cgroup | CLONE_NEWCGROUP | Cgroup 根目录隔离 |
    pub static ref NAMESPACES: HashMap<&'static str, CloneFlags> = {
        let mut m = HashMap::new();
        m.insert("user", CloneFlags::CLONE_NEWUSER);
        m.insert("ipc", CloneFlags::CLONE_NEWIPC);
        m.insert("pid", CloneFlags::CLONE_NEWPID);
        m.insert("net", CloneFlags::CLONE_NEWNET);
        m.insert("mnt", CloneFlags::CLONE_NEWNS);    // 注意：MNT 使用的是 NEWNS
        m.insert("uts", CloneFlags::CLONE_NEWUTS);
        m.insert("cgroup", CloneFlags::CLONE_NEWCGROUP);
        m
    };

    /// OCI Namespace 类型到字符串名称的映射
    ///
    /// 将 OCI 规范中的 `LinuxNamespaceType` 枚举转换为
    /// `/proc/{pid}/ns/` 下的文件名。
    ///
    /// # 示例
    /// ```rust
    /// use oci_spec::runtime::LinuxNamespaceType;
    /// let name = TYPETONAME.get(&LinuxNamespaceType::Network).unwrap();
    /// assert_eq!(*name, "net");
    /// // 对应文件路径: /proc/{pid}/ns/net
    /// ```
    pub static ref TYPETONAME: HashMap<oci::LinuxNamespaceType, &'static str> = {
        let mut m = HashMap::new();
        m.insert(oci::LinuxNamespaceType::Ipc, "ipc");
        m.insert(oci::LinuxNamespaceType::User, "user");
        m.insert(oci::LinuxNamespaceType::Pid, "pid");
        m.insert(oci::LinuxNamespaceType::Network, "net");
        m.insert(oci::LinuxNamespaceType::Mount, "mnt");
        m.insert(oci::LinuxNamespaceType::Cgroup, "cgroup");
        m.insert(oci::LinuxNamespaceType::Uts, "uts");
        m
    };

    /// 默认设备列表
    ///
    /// 容器内必须存在的基本设备节点，符合 OCI 规范要求。
    ///
    /// # 设备清单
    /// | 设备 | 主设备号 | 次设备号 | 权限 | 说明 |
    /// |------|---------|---------|------|------|
    /// | /dev/null | 1 | 3 | 0666 | 空设备，丢弃所有写入 |
    /// | /dev/zero | 1 | 5 | 0666 | 零设备，读取返回 0 |
    /// | /dev/full | 1 | 7 | 0666 | 满设备，写入总是返回 ENOSPC |
    /// | /dev/tty | 5 | 0 | 0666 | 控制终端 |
    /// | /dev/urandom | 1 | 9 | 0666 | 非阻塞随机数生成器 |
    /// | /dev/random | 1 | 8 | 0666 | 阻塞随机数生成器 |
    ///
    /// # UID/GID 说明
    /// 所有设备的 UID 和 GID 都设置为 `0xffffffff`（-1），
    /// 表示保留原有的所有权，不进行修改。
    pub static ref DEFAULT_DEVICES: Vec<LinuxDevice> = {
        vec![
            // /dev/null - 空设备
            oci::LinuxDeviceBuilder::default()
                .path(PathBuf::from("/dev/null"))
                .typ(oci::LinuxDeviceType::C)  // 字符设备
                .major(1)
                .minor(3)
                .file_mode(0o666_u32)
                .uid(0xffffffff_u32)  // 保留原有 UID
                .gid(0xffffffff_u32)  // 保留原有 GID
                .build()
                .unwrap(),
            // /dev/zero - 零设备
            oci::LinuxDeviceBuilder::default()
                .path(PathBuf::from("/dev/zero"))
                .typ(oci::LinuxDeviceType::C)
                .major(1)
                .minor(5)
                .file_mode(0o666_u32)
                .uid(0xffffffff_u32)
                .gid(0xffffffff_u32)
                .build()
                .unwrap(),
            // /dev/full - 满设备
            oci::LinuxDeviceBuilder::default()
                .path(PathBuf::from("/dev/full"))
                .typ(oci::LinuxDeviceType::C)
                .major(1)
                .minor(7)
                .file_mode(0o666_u32)
                .uid(0xffffffff_u32)
                .gid(0xffffffff_u32)
                .build()
                .unwrap(),
            // /dev/tty - 控制终端
            oci::LinuxDeviceBuilder::default()
                .path(PathBuf::from("/dev/tty"))
                .typ(oci::LinuxDeviceType::C)
                .major(5)
                .minor(0)
                .file_mode(0o666_u32)
                .uid(0xffffffff_u32)
                .gid(0xffffffff_u32)
                .build()
                .unwrap(),
            // /dev/urandom - 非阻塞随机数生成器
            oci::LinuxDeviceBuilder::default()
                .path(PathBuf::from("/dev/urandom"))
                .typ(oci::LinuxDeviceType::C)
                .major(1)
                .minor(9)
                .file_mode(0o666_u32)
                .uid(0xffffffff_u32)
                .gid(0xffffffff_u32)
                .build()
                .unwrap(),
            // /dev/random - 阻塞随机数生成器
            oci::LinuxDeviceBuilder::default()
                .path(PathBuf::from("/dev/random"))
                .typ(oci::LinuxDeviceType::C)
                .major(1)
                .minor(8)
                .file_mode(0o666_u32)
                .uid(0xffffffff_u32)
                .gid(0xffffffff_u32)
                .build()
                .unwrap(),
        ]
    };

    /// Systemd Cgroup 路径格式正则表达式
    ///
    /// 用于验证 systemd cgroup 路径是否符合格式：`slice:scope:name`
    ///
    /// # 格式说明
    /// - `slice`: Systemd slice 单元（如 `system.slice`）
    /// - `scope`: Scope 单元（可选）
    /// - `name`: 容器名称
    ///
    /// # 示例
    /// - ✅ `system.slice:docker:abc123`
    /// - ✅ `user.slice::container-1`
    /// - ❌ `invalid_format`
    pub static ref SYSTEMD_CGROUP_PATH_FORMAT:Regex = Regex::new(r"^[\w\-.]*:[\w\-.]*:[\w\-.]*$").unwrap();
}
