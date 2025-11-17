//! # Confidential Data Hub 集成
//!
//! 提供与 Confidential Data Hub (CDH) 服务的集成,用于安全镜像拉取。
//!
//! CDH 是一个运行在 guest 内的服务,提供资源相关的 API。
//! 更多信息: https://github.com/confidential-containers/guest-components/tree/main/confidential-data-hub

use std::path::Path;

use anyhow::{Result, bail};
use slog::Logger;

/// 通过 CDH 拉取镜像
///
/// 使用 Confidential Data Hub 服务拉取远程镜像。
///
/// # 参数
/// - `image`: 镜像 URL (例如: docker://registry/image:tag)
/// - `bundle_path`: Bundle 目录路径
/// - `logger`: 日志记录器
///
/// # 返回
/// 成功时返回 Ok(())
///
/// # 注意
/// 此功能需要启用 `cdh` feature 并配置 CDH 服务。
#[cfg(feature = "cdh")]
pub async fn pull_image_via_cdh(image: &str, bundle_path: &Path, logger: &Logger) -> Result<()> {
    info!(logger, "Pulling image via CDH"; "image" => image, "bundle" => bundle_path.display().to_string());

    // TODO: 实现 CDH 客户端集成
    // 1. 连接到 CDH socket (通常是 unix:///run/confidential-containers/cdh.sock)
    // 2. 调用 ImagePullService 的 pull_image 方法
    // 3. 等待镜像拉取完成
    // 4. 验证 rootfs 和 config.json 已创建

    bail!("CDH support not yet implemented")
}

/// CDH 客户端配置
#[cfg(feature = "cdh")]
#[derive(Debug, Clone)]
pub struct CDHConfig {
    /// CDH socket 路径
    pub socket_path: String,
    /// API 调用超时 (秒)
    pub timeout_secs: u64,
}

#[cfg(feature = "cdh")]
impl Default for CDHConfig {
    fn default() -> Self {
        Self {
            socket_path: "unix:///run/confidential-containers/cdh.sock".to_string(),
            timeout_secs: 300, // 5 minutes
        }
    }
}

#[cfg(not(feature = "cdh"))]
pub async fn pull_image_via_cdh(_image: &str, _bundle_path: &Path, _logger: &Logger) -> Result<()> {
    bail!("CDH feature is not enabled")
}
