//! # 存储处理器
//!
//! 定义不同类型存储的处理器实现。

use std::{collections::HashMap, sync::Arc};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use slog::Logger;

use crate::{StorageConfig, device::StorageDevice};

/// 存储上下文
///
/// 在处理存储设备时传递的上下文信息。
#[derive(Debug)]
pub struct StorageContext<'a> {
    /// 容器 ID
    pub container_id: Option<String>,
    /// 日志记录器
    pub logger: &'a Logger,
}

/// 存储处理器 Trait
///
/// 定义处理特定类型存储的接口。
#[async_trait]
pub trait StorageHandler: Send + Sync {
    /// 创建存储设备
    ///
    /// 根据存储配置创建并初始化存储设备。
    ///
    /// # 参数
    /// - `storage`: 存储配置
    /// - `ctx`: 存储上下文
    ///
    /// # 返回
    /// 创建的存储设备
    async fn create_device(
        &self,
        storage: StorageConfig,
        ctx: &mut StorageContext<'_>,
    ) -> Result<Arc<dyn StorageDevice>>;

    /// 返回处理器支持的驱动类型
    ///
    /// # 返回
    /// 驱动类型字符串数组
    fn driver_types(&self) -> &[&str];
}

/// 存储处理器管理器
///
/// 管理所有注册的存储处理器，根据驱动类型分发请求。
pub struct StorageHandlerManager {
    /// 驱动类型到处理器的映射
    handlers: HashMap<String, Arc<dyn StorageHandler>>,
}

impl StorageHandlerManager {
    /// 创建新的管理器
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    /// 注册存储处理器
    ///
    /// # 参数
    /// - `driver_types`: 处理器支持的驱动类型列表
    /// - `handler`: 处理器实例
    pub fn add_handler(
        &mut self,
        driver_types: &[&str],
        handler: Arc<dyn StorageHandler>,
    ) -> Result<()> {
        for driver_type in driver_types {
            if self.handlers.contains_key(*driver_type) {
                return Err(anyhow!("Handler for {} already registered", driver_type));
            }
            self.handlers
                .insert(driver_type.to_string(), handler.clone());
        }
        Ok(())
    }

    /// 根据驱动类型获取处理器
    ///
    /// # 参数
    /// - `driver_type`: 驱动类型
    ///
    /// # 返回
    /// 对应的处理器，如果未找到返回 None
    pub fn handler(&self, driver_type: &str) -> Option<Arc<dyn StorageHandler>> {
        self.handlers.get(driver_type).cloned()
    }
}

impl Default for StorageHandlerManager {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// 具体的存储处理器实现
// ============================================================================

/// 本地存储处理器
///
/// 处理本地目录的绑定挂载。
#[derive(Debug)]
pub struct LocalHandler;

#[async_trait]
impl StorageHandler for LocalHandler {
    async fn create_device(
        &self,
        storage: StorageConfig,
        ctx: &mut StorageContext<'_>,
    ) -> Result<Arc<dyn StorageDevice>> {
        info!(ctx.logger, "Creating local storage device"; "source" => &storage.source, "target" => &storage.mount_point);

        // 执行绑定挂载
        crate::mount::bind_mount(&storage.source, &storage.mount_point, &storage.options)?;

        info!(ctx.logger, "Local storage mounted successfully"; "mount_point" => &storage.mount_point);

        crate::device::new_device(storage.mount_point)
    }

    fn driver_types(&self) -> &[&str] {
        &["local"]
    }
}

/// 块设备处理器
///
/// 处理块设备的挂载。
#[derive(Debug)]
pub struct BlockHandler;

#[async_trait]
impl StorageHandler for BlockHandler {
    async fn create_device(
        &self,
        storage: StorageConfig,
        ctx: &mut StorageContext<'_>,
    ) -> Result<Arc<dyn StorageDevice>> {
        info!(ctx.logger, "Creating block device storage"; "device" => &storage.source, "target" => &storage.mount_point);

        // 执行块设备挂载
        crate::mount::mount_device(
            &storage.source,
            &storage.mount_point,
            &storage.fstype,
            &storage.options,
        )?;

        info!(ctx.logger, "Block device mounted successfully"; "mount_point" => &storage.mount_point);

        crate::device::new_device(storage.mount_point)
    }

    fn driver_types(&self) -> &[&str] {
        &["block", "virtio-blk"]
    }
}

/// OverlayFS 处理器
///
/// 处理 OverlayFS 联合挂载。
#[derive(Debug)]
pub struct OverlayHandler;

#[async_trait]
impl StorageHandler for OverlayHandler {
    async fn create_device(
        &self,
        storage: StorageConfig,
        ctx: &mut StorageContext<'_>,
    ) -> Result<Arc<dyn StorageDevice>> {
        info!(ctx.logger, "Creating overlay storage device"; "target" => &storage.mount_point);

        // 从 driver_options 中提取 overlay 参数
        // 格式: lowerdir=/path1:/path2,upperdir=/path3,workdir=/path4
        let mut lower = String::new();
        let mut upper = String::new();
        let mut work = String::new();

        for opt in &storage.driver_options {
            if let Some((key, value)) = opt.split_once('=') {
                match key {
                    "lowerdir" => lower = value.to_string(),
                    "upperdir" => upper = value.to_string(),
                    "workdir" => work = value.to_string(),
                    _ => {}
                }
            }
        }

        if lower.is_empty() || upper.is_empty() || work.is_empty() {
            return Err(anyhow!("Overlay requires lowerdir, upperdir, and workdir"));
        }

        // 执行 overlay 挂载
        crate::mount::mount_overlay(
            &lower,
            &upper,
            &work,
            &storage.mount_point,
            &storage.options,
        )?;

        info!(ctx.logger, "Overlay storage mounted successfully"; "mount_point" => &storage.mount_point);

        crate::device::new_device(storage.mount_point)
    }

    fn driver_types(&self) -> &[&str] {
        &["overlay", "overlayfs"]
    }
}

/// 镜像拉取处理器
///
/// 处理容器镜像的拉取和挂载。
#[derive(Debug)]
pub struct ImagePullHandler;

#[async_trait]
impl StorageHandler for ImagePullHandler {
    async fn create_device(
        &self,
        storage: StorageConfig,
        ctx: &mut StorageContext<'_>,
    ) -> Result<Arc<dyn StorageDevice>> {
        info!(ctx.logger, "Creating image pull storage device"; "image" => &storage.source);

        let container_id = ctx
            .container_id
            .as_ref()
            .ok_or_else(|| anyhow!("Container ID is required for image pull"))?;

        // 调用镜像拉取模块
        let bundle_path =
            crate::image::pull_and_extract(&storage.source, container_id, ctx.logger).await?;

        info!(ctx.logger, "Image pulled successfully"; "bundle-path" => &bundle_path);

        crate::device::new_device(bundle_path)
    }

    fn driver_types(&self) -> &[&str] {
        &["image", "image-pull"]
    }
}

// ============================================================================
// 全局存储处理器管理器
// ============================================================================

lazy_static::lazy_static! {
    /// 全局存储处理器管理器
    ///
    /// 包含所有注册的存储处理器。
    pub static ref STORAGE_HANDLERS: StorageHandlerManager = {
        let mut manager = StorageHandlerManager::new();

        let handlers: Vec<Arc<dyn StorageHandler>> = vec![
            Arc::new(LocalHandler),
            Arc::new(BlockHandler),
            Arc::new(OverlayHandler),
            Arc::new(ImagePullHandler),
        ];

        for handler in &handlers {
            manager.add_handler(handler.driver_types(), handler.clone()).unwrap();
        }

        manager
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handler_manager() {
        let manager = &*STORAGE_HANDLERS;

        assert!(manager.handler("local").is_some());
        assert!(manager.handler("block").is_some());
        assert!(manager.handler("overlay").is_some());
        assert!(manager.handler("image").is_some());
        assert!(manager.handler("unknown").is_none());
    }

    #[test]
    fn test_driver_types() {
        let local_handler = LocalHandler;
        assert_eq!(local_handler.driver_types(), &["local"]);

        let block_handler = BlockHandler;
        assert_eq!(block_handler.driver_types(), &["block", "virtio-blk"]);
    }
}
