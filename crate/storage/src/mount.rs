//! # 文件系统挂载操作
//!
//! 提供各种类型的文件系统挂载和卸载功能。

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use nix::mount::{MsFlags, mount as nix_mount, umount};

/// 挂载标志位映射
///
/// 将字符串挂载选项转换为 MsFlags。
pub fn parse_mount_flags(options: &[String]) -> MsFlags {
    let mut flags = MsFlags::empty();

    for opt in options {
        match opt.as_str() {
            "ro" | "readonly" => flags |= MsFlags::MS_RDONLY,
            "nosuid" => flags |= MsFlags::MS_NOSUID,
            "nodev" => flags |= MsFlags::MS_NODEV,
            "noexec" => flags |= MsFlags::MS_NOEXEC,
            "sync" => flags |= MsFlags::MS_SYNCHRONOUS,
            "remount" => flags |= MsFlags::MS_REMOUNT,
            "bind" => flags |= MsFlags::MS_BIND,
            "rbind" => flags |= MsFlags::MS_BIND | MsFlags::MS_REC,
            "private" => flags |= MsFlags::MS_PRIVATE,
            "rprivate" => flags |= MsFlags::MS_PRIVATE | MsFlags::MS_REC,
            "shared" => flags |= MsFlags::MS_SHARED,
            "rshared" => flags |= MsFlags::MS_SHARED | MsFlags::MS_REC,
            "slave" => flags |= MsFlags::MS_SLAVE,
            "rslave" => flags |= MsFlags::MS_SLAVE | MsFlags::MS_REC,
            _ => {
                // 忽略不识别的选项
            }
        }
    }

    flags
}

/// 绑定挂载
///
/// 将源目录绑定挂载到目标位置。
///
/// # 参数
/// - `source`: 源路径
/// - `target`: 目标挂载点
/// - `options`: 挂载选项
///
/// # 示例
/// ```no_run
/// use storage::mount::bind_mount;
///
/// bind_mount("/host/path", "/container/path", &["ro".to_string()]).unwrap();
/// ```
pub fn bind_mount(source: &str, target: &str, options: &[String]) -> Result<()> {
    let source_path = Path::new(source);
    let target_path = Path::new(target);

    if !source_path.exists() {
        return Err(anyhow!("Source path does not exist: {}", source));
    }

    // 创建目标目录
    if !target_path.exists() {
        std::fs::create_dir_all(target_path)
            .with_context(|| format!("Failed to create target directory: {}", target))?;
    }

    // 默认添加 MS_BIND 标志
    let mut flags = MsFlags::MS_BIND;
    flags |= parse_mount_flags(options);

    nix_mount(
        Some(source_path),
        target_path,
        None::<&str>,
        flags,
        None::<&str>,
    )
    .with_context(|| format!("Failed to bind mount {} to {}", source, target))?;

    // 如果需要只读挂载,需要再次 remount
    if options.iter().any(|o| o == "ro" || o == "readonly") {
        nix_mount(
            Some(source_path),
            target_path,
            None::<&str>,
            flags | MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY,
            None::<&str>,
        )
        .with_context(|| format!("Failed to remount {} as readonly", target))?;
    }

    Ok(())
}

/// 块设备挂载
///
/// 将块设备挂载到目标位置。
///
/// # 参数
/// - `device`: 设备路径
/// - `target`: 目标挂载点
/// - `fstype`: 文件系统类型 (ext4, xfs, etc.)
/// - `options`: 挂载选项
///
/// # 示例
/// ```no_run
/// use storage::mount::mount_device;
///
/// mount_device("/dev/sda1", "/mnt", "ext4", &["ro".to_string()]).unwrap();
/// ```
pub fn mount_device(device: &str, target: &str, fstype: &str, options: &[String]) -> Result<()> {
    let device_path = Path::new(device);
    let target_path = Path::new(target);

    if !device_path.exists() {
        return Err(anyhow!("Device does not exist: {}", device));
    }

    // 创建目标目录
    if !target_path.exists() {
        std::fs::create_dir_all(target_path)
            .with_context(|| format!("Failed to create target directory: {}", target))?;
    }

    let flags = parse_mount_flags(options);

    // 构建挂载选项字符串
    let data = if !options.is_empty() {
        Some(options.join(","))
    } else {
        None
    };

    nix_mount(
        Some(device_path),
        target_path,
        Some(fstype),
        flags,
        data.as_deref(),
    )
    .with_context(|| format!("Failed to mount {} to {}", device, target))?;

    Ok(())
}

/// OverlayFS 挂载
///
/// 创建 OverlayFS 联合挂载。
///
/// # 参数
/// - `lower`: 下层目录 (可以有多个,用:分隔)
/// - `upper`: 上层目录 (可写层)
/// - `work`: 工作目录
/// - `target`: 目标挂载点
/// - `options`: 额外挂载选项
///
/// # 示例
/// ```no_run
/// use storage::mount::mount_overlay;
///
/// mount_overlay("/lower1:/lower2", "/upper", "/work", "/merged", &[]).unwrap();
/// ```
pub fn mount_overlay(
    lower: &str,
    upper: &str,
    work: &str,
    target: &str,
    options: &[String],
) -> Result<()> {
    let target_path = Path::new(target);

    // 创建必要的目录
    if !target_path.exists() {
        std::fs::create_dir_all(target_path)
            .with_context(|| format!("Failed to create target directory: {}", target))?;
    }

    let upper_path = Path::new(upper);
    if !upper_path.exists() {
        std::fs::create_dir_all(upper_path)
            .with_context(|| format!("Failed to create upper directory: {}", upper))?;
    }

    let work_path = Path::new(work);
    if !work_path.exists() {
        std::fs::create_dir_all(work_path)
            .with_context(|| format!("Failed to create work directory: {}", work))?;
    }

    // 构建 overlay 挂载选项
    let overlay_opts = format!("lowerdir={},upperdir={},workdir={}", lower, upper, work);

    let flags = parse_mount_flags(options);

    nix_mount(
        Some("overlay"),
        target_path,
        Some("overlay"),
        flags,
        Some(overlay_opts.as_str()),
    )
    .with_context(|| format!("Failed to mount overlay to {}", target))?;

    Ok(())
}

/// 卸载文件系统
///
/// # 参数
/// - `target`: 要卸载的挂载点
///
/// # 示例
/// ```no_run
/// use storage::mount::unmount;
///
/// unmount("/mnt").unwrap();
/// ```
pub fn unmount(target: &str) -> Result<()> {
    let target_path = Path::new(target);

    if !target_path.exists() {
        // 如果路径不存在,认为已经卸载
        return Ok(());
    }

    umount(target_path).with_context(|| format!("Failed to unmount {}", target))?;

    Ok(())
}

/// 检查路径是否是挂载点
///
/// # 参数
/// - `path`: 要检查的路径
///
/// # 返回
/// - `true`: 是挂载点
/// - `false`: 不是挂载点
pub fn is_mounted(path: &str) -> Result<bool> {
    // 读取 /proc/mounts 文件
    let mounts = std::fs::read_to_string("/proc/mounts").context("Failed to read /proc/mounts")?;

    for line in mounts.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 && parts[1] == path {
            return Ok(true);
        }
    }

    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_mount_flags() {
        let options = vec!["ro".to_string(), "nosuid".to_string(), "nodev".to_string()];
        let flags = parse_mount_flags(&options);

        assert!(flags.contains(MsFlags::MS_RDONLY));
        assert!(flags.contains(MsFlags::MS_NOSUID));
        assert!(flags.contains(MsFlags::MS_NODEV));
    }

    #[test]
    fn test_parse_bind_flags() {
        let options = vec!["bind".to_string()];
        let flags = parse_mount_flags(&options);

        assert!(flags.contains(MsFlags::MS_BIND));
    }

    #[test]
    fn test_parse_rbind_flags() {
        let options = vec!["rbind".to_string()];
        let flags = parse_mount_flags(&options);

        assert!(flags.contains(MsFlags::MS_BIND));
        assert!(flags.contains(MsFlags::MS_REC));
    }
}
