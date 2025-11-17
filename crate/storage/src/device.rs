//! # 存储设备抽象
//!
//! 定义存储设备的统一接口和通用实现。

use std::{fs, path::Path};

use anyhow::{Result, anyhow};

/// 存储设备 Trait
///
/// 所有存储设备的统一接口，提供路径访问和清理功能。
pub trait StorageDevice: Send + Sync + std::fmt::Debug {
    /// 获取存储设备的路径
    ///
    /// # 返回
    /// - `Some(path)`: 设备路径
    /// - `None`: 设备没有路径（如某些虚拟设备）
    fn path(&self) -> Option<&str>;

    /// 清理存储设备
    ///
    /// 执行以下操作：
    /// 1. 卸载文件系统（如果已挂载）
    /// 2. 删除空目录
    /// 3. 清理相关资源
    ///
    /// # 错误
    /// - 设备仍被占用
    /// - 卸载失败
    /// - 目录非空
    fn cleanup(&self) -> Result<()>;
}

/// 通用存储设备实现
///
/// 适用于大多数基于路径的存储设备。
#[derive(Default, Debug)]
pub struct StorageDeviceGeneric {
    /// 设备路径
    path: Option<String>,
}

impl StorageDeviceGeneric {
    /// 创建新的存储设备
    ///
    /// # 参数
    /// - `path`: 设备路径
    pub fn new(path: String) -> Self {
        Self { path: Some(path) }
    }
}

impl StorageDevice for StorageDeviceGeneric {
    fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    fn cleanup(&self) -> Result<()> {
        let path = match self.path() {
            None => return Ok(()),
            Some(v) => {
                if v.is_empty() {
                    // 空路径，不需要清理
                    return Ok(());
                } else {
                    v
                }
            }
        };

        if !Path::new(path).exists() {
            return Ok(());
        }

        // TODO: 检查并卸载挂载点
        // 需要实现挂载检查和卸载功能

        let p = Path::new(path);
        if p.is_dir() {
            // 检查目录是否为空
            let is_empty = p.read_dir()?.next().is_none();
            if !is_empty {
                return Err(anyhow!("directory is not empty when clean up storage"));
            }
            // 删除空目录
            fs::remove_dir(p)?;
        } else if p.is_file() {
            // 对于文件，通常是绑定挂载的情况，不删除
            // 可以根据具体需求决定是否删除
        } else {
            return Err(anyhow!(
                "storage path {} is neither directory nor file",
                path
            ));
        }

        Ok(())
    }
}

/// 创建新的存储设备
///
/// 辅助函数，用于创建 `StorageDeviceGeneric` 的 Arc 包装。
///
/// # 参数
/// - `path`: 设备路径
pub fn new_device(path: String) -> Result<std::sync::Arc<dyn StorageDevice>> {
    let device = StorageDeviceGeneric::new(path);
    Ok(std::sync::Arc::new(device))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_storage_device_new() {
        let device = StorageDeviceGeneric::new("/test/path".to_string());
        assert_eq!(device.path(), Some("/test/path"));
    }

    #[test]
    fn test_storage_device_cleanup_empty_dir() {
        let dir = tempdir().unwrap();
        let path = dir.path().to_str().unwrap().to_string();

        let device = StorageDeviceGeneric::new(path.clone());
        assert!(device.cleanup().is_ok());

        // 目录应该被删除
        assert!(!Path::new(&path).exists());
    }

    #[test]
    fn test_storage_device_cleanup_non_empty_dir() {
        let dir = tempdir().unwrap();
        let path = dir.path().to_str().unwrap().to_string();

        // 创建一个文件使目录非空
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "test").unwrap();

        let device = StorageDeviceGeneric::new(path);
        assert!(device.cleanup().is_err());
    }

    #[test]
    fn test_storage_device_cleanup_nonexistent() {
        let device = StorageDeviceGeneric::new("/nonexistent/path".to_string());
        assert!(device.cleanup().is_ok());
    }
}
