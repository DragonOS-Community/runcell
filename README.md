# Runcell 使用文档

Runcell 是一个参照 rustjail 实现的轻量级容器运行时，支持容器的创建、运行和管理。

## 功能特性

- **容器生命周期管理**: 创建、启动、停止、删除容器
- **容器列表查看**: 列出所有运行中或已停止的容器
- **容器内执行命令**: 类似 `docker exec` 进入运行中的容器执行命令
- **交互式终端支持**: 支持 `-it` 参数进行交互式操作
- **状态持久化**: 容器状态保存到磁盘，支持跨进程查询
- **Namespace 隔离**: 支持 PID、Mount、UTS、IPC、Network 等命名空间隔离
- **Cgroup 资源限制**: 支持 CPU、内存等资源限制

## 构建

```bash
cargo build
```

## 基础用法

### 容器管理命令

#### 运行容器

创建并启动一个容器：

```bash
sudo ./target/debug/runcell container run \
    --id <容器ID> \
    --image <镜像路径> \
    --command <执行命令> \
    --args <参数>
```

**示例：**

```bash
# 运行一个 sleep 进程的容器
sudo ./target/debug/runcell container run \
    --id test \
    --image /path/to/rootfs \
    --command /bin/sleep \
    --args 180

# 交互式运行容器（类似 docker run -it）
sudo ./target/debug/runcell container run \
    --id mycontainer \
    --image /path/to/rootfs \
    -t -i

# 后台运行容器（类似 docker run -d）
sudo ./target/debug/runcell container run \
    --id mycontainer \
    --image /path/to/rootfs \
    --command /bin/sleep \
    --args 3600 \
    -d
```

**参数说明：**
| 参数 | 简写 | 说明 |
|------|------|------|
| `--id` | `-i` | 容器 ID（必需） |
| `--image` | `-m` | 镜像源，目前只支持本地路径 |
| `--command` | `-c` | 要执行的命令，默认 `/bin/sh` |
| `--args` | `-a` | 命令参数 |
| `--tty` | `-t` | 分配伪终端（TTY） |
| `--interactive` | `-i` | 保持 STDIN 打开（交互模式） |
| `--detach` | `-d` | 后台运行（分离模式） |

#### 列出容器

列出所有容器：

```bash
# 列出运行中的容器
sudo ./target/debug/runcell container list

# 列出所有容器（包括已停止的）
sudo ./target/debug/runcell container list --all

# JSON 格式输出
sudo ./target/debug/runcell container list --format json
```

**输出示例：**

```
CONTAINER ID         PID      STATUS     CREATED              ROOTFS
test                 12345    Running    2025-12-20 10:30:00  /tmp/runcell/containers/test/rootfs
mycontainer          0        Stopped    2025-12-20 09:00:00  /tmp/runcell/containers/mycontainer/rootfs
```

**参数说明：**
| 参数 | 简写 | 说明 |
|------|------|------|
| `--format` | `-f` | 输出格式：`table`（默认）或 `json` |
| `--all` | `-a` | 显示所有容器（包括已停止的） |

#### 在容器内执行命令

在运行中的容器内执行命令（类似 `docker exec`）：

```bash
# 执行单个命令
sudo ./target/debug/runcell container exec \
    --id <容器ID> \
    --command <命令>

# 交互式进入容器
sudo ./target/debug/runcell container exec \
    --id <容器ID> \
    -t -i
```

**示例：**

```bash
# 在容器内执行 ls 命令
sudo ./target/debug/runcell container exec \
    --id test \
    --command /bin/ls \
    --args /

# 交互式进入容器的 shell
sudo ./target/debug/runcell container exec \
    --id test \
    -t -i

# 在容器内执行带参数的命令
sudo ./target/debug/runcell container exec \
    --id test \
    --command /bin/cat \
    --args /etc/os-release
```

**参数说明：**
| 参数 | 简写 | 说明 |
|------|------|------|
| `--id` | `-i` | 容器 ID（必需） |
| `--command` | `-c` | 要执行的命令，默认 `/bin/sh` |
| `--args` | `-a` | 命令参数 |
| `--tty` | `-t` | 分配伪终端（TTY） |
| `--interactive` | `-i` | 保持 STDIN 打开（交互模式） |

#### 删除容器

```bash
sudo ./target/debug/runcell container delete --id <容器ID>
```

删除操作会：
1. 读取容器状态获取 PID
2. 如果进程仍在运行，发送 SIGKILL 信号终止
3. 清理 bundle 目录
4. 清理状态目录
5. 清理镜像文件

**示例：**

```bash
sudo ./target/debug/runcell container delete --id test
```

#### 创建容器（仅创建，不启动）

```bash
sudo ./target/debug/runcell container create \
    --id <容器ID> \
    --rootfs <rootfs路径> \
    [--bundle <bundle目录>]
```

#### 启动容器

```bash
sudo ./target/debug/runcell container start --id <容器ID>
```

### 存储管理命令

#### 拉取镜像

```bash
sudo ./target/debug/runcell storage pull \
    --image <镜像源> \
    --container-id <容器ID>
```

#### 挂载

```bash
sudo ./target/debug/runcell storage mount \
    --source <源路径> \
    --target <目标路径> \
    --options <挂载选项>
```

#### 卸载

```bash
sudo ./target/debug/runcell storage umount --target <挂载点>
```

#### 清理镜像

```bash
sudo ./target/debug/runcell storage cleanup --container-id <容器ID>
```

## 完整示例

### 示例 1：基础容器操作

```bash
# 1. 构建项目
cargo build

# 2. 运行容器
sudo ./target/debug/runcell container run \
    --id test \
    --image /path/to/rootfs \
    --command /bin/sleep \
    --args 180

# 3. 查看容器列表
sudo ./target/debug/runcell container list

# 4. 在容器内执行命令
sudo ./target/debug/runcell container exec \
    --id test \
    --command /bin/ls \
    --args /

# 5. 删除容器
sudo ./target/debug/runcell container delete --id test
```

### 示例 2：交互式容器

```bash
# 1. 启动交互式容器
sudo ./target/debug/runcell container run \
    --id interactive-test \
    --image /path/to/rootfs \
    -t -i

# 2. 在另一个终端查看容器
sudo ./target/debug/runcell container list

# 3. 在另一个终端进入同一容器
sudo ./target/debug/runcell container exec \
    --id interactive-test \
    -t -i

# 4. 退出后删除容器
sudo ./target/debug/runcell container delete --id interactive-test
```

## 状态持久化

容器状态保存在 `state.json` 文件中，位于 `/tmp/runcell/states/<容器ID>/state.json`。

**状态文件格式：**

```json
{
  "id": "test",
  "init_process_pid": 12345,
  "init_process_start_time": 1734567890,
  "status": "Running",
  "bundle": "/tmp/runcell/bundles/test",
  "rootfs": "/tmp/runcell/containers/test/rootfs",
  "created": 1734567880,
  "namespace_paths": {
    "mnt": "/proc/12345/ns/mnt",
    "pid": "/proc/12345/ns/pid",
    "net": "/proc/12345/ns/net",
    "ipc": "/proc/12345/ns/ipc",
    "uts": "/proc/12345/ns/uts"
  }
}
```

## 调试技巧

### 查看容器进程

```bash
sudo ps aux | grep -E "sleep|runcell" | grep -v grep
```

### 手动进入容器命名空间

使用 `nsenter` 进入容器的命名空间：

```bash
# 先获取容器进程 PID
sudo ./target/debug/runcell container list

# 使用 nsenter 进入容器执行命令
sudo nsenter -t <PID> -m -p -u -i -n <命令>
```

**nsenter 参数说明：**
| 参数 | 说明 |
|------|------|
| `-t <PID>` | 目标进程 PID |
| `-m` | 进入 mount 命名空间 |
| `-p` | 进入 PID 命名空间 |
| `-u` | 进入 UTS 命名空间 |
| `-i` | 进入 IPC 命名空间 |
| `-n` | 进入 network 命名空间 |

## 日志

使用 `-v` 或 `--verbose` 启用详细日志：

```bash
sudo ./target/debug/runcell -v container run --id test --image /path/to/rootfs
```

## 数据目录

| 目录 | 说明 |
|------|------|
| `/tmp/runcell/bundles/<容器ID>` | OCI bundle 目录，包含 config.json |
| `/tmp/runcell/states/<容器ID>` | 容器状态目录，包含 state.json |
| `/tmp/runcell/containers/<容器ID>` | 容器镜像目录 |

## 依赖项

如果你要使用 seccomp 的话，确保本机已安装 libseccomp：

```bash
sudo apt update && sudo apt install libseccomp-dev
```

## 命令速查表

| 命令 | 说明 |
|------|------|
| `container run` | 创建并运行容器 |
| `container list` | 列出容器 |
| `container exec` | 在容器内执行命令 |
| `container delete` | 删除容器 |
| `container create` | 创建容器（不启动） |
| `container start` | 启动已创建的容器 |
| `storage pull` | 拉取镜像 |
| `storage mount` | 挂载存储 |
| `storage umount` | 卸载存储 |
| `storage cleanup` | 清理镜像 |

## 与 Docker 命令对比

| Docker | Runcell |
|--------|---------|
| `docker run -it image` | `runcell container run -m image -t -i` |
| `docker run -d image` | `runcell container run -m image -d` |
| `docker ps` | `runcell container list` |
| `docker ps -a` | `runcell container list --all` |
| `docker exec -it container cmd` | `runcell container exec --id container -t -i` |
| `docker rm container` | `runcell container delete --id container` |
