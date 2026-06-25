# service-manager 使用文档

`service-manager` 是一个本机服务管理器。它提供一个 Rust 单二进制，同时包含：

- Web 管理页面
- REST API
- 本地服务注册表
- Bearer token 鉴权
- 多平台 provider：`process`、`proot-distro`、`systemd`、`launchd`、`docker`、`termux-services`

它只管理当前机器，不包含远程节点概念。

## 快速开始

从源码构建：

```bash
cargo build --release
```

查看诊断：

```bash
./target/release/service-manager doctor
```

启动 Web/API 服务：

```bash
./target/release/service-manager serve
```

默认监听：

```text
127.0.0.1:20087
```

打开 Web UI：

```text
http://127.0.0.1:20087/
```

查看 token：

```bash
./target/release/service-manager token show
```

Web UI 右上角输入这个 token 后保存，即可调用 API。

## 安装

安装到当前用户路径：

```bash
./scripts/install.sh ./target/release/service-manager
```

默认安装位置：

- Termux：`$PREFIX/bin/service-manager`
- Linux/macOS：`$HOME/.local/bin/service-manager`

指定安装目录：

```bash
INSTALL_DIR=/some/bin ./scripts/install.sh ./target/release/service-manager
```

安装并尝试注册为本机用户服务：

```bash
INSTALL_SERVICE=1 ./scripts/install.sh ./target/release/service-manager
```

也可以手动安装服务：

```bash
service-manager install-service
```

卸载但保留配置和数据：

```bash
./scripts/uninstall.sh
```

卸载并清理配置/数据：

```bash
./scripts/uninstall.sh --purge
```

`--purge` 带安全保护，只会删除目录名明确为 `service-manager` 的目录，避免误删上级目录。

## 配置和数据

默认配置文件：

- Linux：`${XDG_CONFIG_HOME:-$HOME/.config}/service-manager/config.json`
- macOS：`$HOME/Library/Application Support/service-manager/config.json`

默认数据目录：

```text
${UserConfigDir}/service-manager/data/
```

默认存储：

```text
${data_dir}/store.json
```

配置示例：

```json
{
  "listen_addr": "127.0.0.1:20087",
  "data_dir": "/home/me/.config/service-manager/data",
  "auth_token": "",
  "log_level": "info",
  "store": {
    "type": "json",
    "path": ""
  }
}
```

说明：

- `auth_token` 为空时，首次运行会自动生成并写入配置文件。
- `SERVICE_MANAGER_TOKEN` 只在配置中的 `auth_token` 为空时作为覆盖值使用。
- `store.path` 为空时，默认使用 `${data_dir}/store.json`。
- 默认只绑定 `127.0.0.1`，不要直接暴露公网。

## 常用命令

启动服务端：

```bash
service-manager serve
```

指定监听地址：

```bash
service-manager serve --bind 127.0.0.1:20087
```

指定配置文件：

```bash
service-manager serve --config /path/to/config.json
```

诊断环境和 provider：

```bash
service-manager doctor
```

查看 token：

```bash
service-manager token show
```

轮换 token：

```bash
service-manager token rotate
```

安装为用户服务：

```bash
service-manager install-service
```

卸载用户服务：

```bash
service-manager uninstall-service
```

## Web UI

Web UI 由同一个二进制直接提供，不需要 Node 构建。

功能包括：

- 查看 provider 诊断状态
- 创建和编辑服务
- 启动、停止、重启、删除服务
- 查看服务状态
- 查看日志
- 保存 bearer token 到浏览器 `localStorage`

页面地址：

```text
http://127.0.0.1:20087/
```

如果显示未授权，先运行：

```bash
service-manager token show
```

然后把输出的 token 填到页面右上角。

## REST API

健康检查不需要鉴权：

```bash
curl -fsS http://127.0.0.1:20087/api/v1/health
```

其他 API 都需要：

```http
Authorization: Bearer <token>
```

设置 token 变量：

```bash
TOKEN="$(service-manager token show | head -n1)"
```

查看 provider：

```bash
curl -fsS \
  -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:20087/api/v1/providers
```

查看服务列表：

```bash
curl -fsS \
  -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:20087/api/v1/services
```

按标签或分组过滤服务：

```bash
curl -fsS \
  -H "Authorization: Bearer $TOKEN" \
  "http://127.0.0.1:20087/api/v1/services?tag=smallphoneai"

curl -fsS \
  -H "Authorization: Bearer $TOKEN" \
  "http://127.0.0.1:20087/api/v1/services?group=phone-control"
```

批量查看状态：

```bash
curl -fsS \
  -H "Authorization: Bearer $TOKEN" \
  "http://127.0.0.1:20087/api/v1/services/statuses?group=phone-control"

curl -fsS \
  -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:20087/api/v1/groups/phone-control/status
```

分组控制：

```bash
curl -fsS -X POST \
  -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:20087/api/v1/groups/phone-control/restart
```

创建一个 `process` 服务：

```bash
curl -fsS \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "demo-sleep",
    "description": "demo process service",
    "provider": "process",
    "command": ["sleep", "300"],
    "working_dir": "",
    "env": {},
    "runtime": {},
    "restart": { "mode": "no", "max_retries": 0 },
    "health": [],
    "enabled": true,
    "tags": ["demo"]
  }' \
  http://127.0.0.1:20087/api/v1/services
```

返回里会包含服务 `id`。后续假设：

```bash
SERVICE_ID="..."
```

启动服务：

```bash
curl -fsS -X POST \
  -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:20087/api/v1/services/$SERVICE_ID/start
```

查看状态：

```bash
curl -fsS \
  -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:20087/api/v1/services/$SERVICE_ID/status
```

查看日志：

```bash
curl -fsS \
  -H "Authorization: Bearer $TOKEN" \
  "http://127.0.0.1:20087/api/v1/services/$SERVICE_ID/logs?limit=100"
```

停止服务：

```bash
curl -fsS -X POST \
  -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:20087/api/v1/services/$SERVICE_ID/stop
```

删除服务：

```bash
curl -fsS -X DELETE \
  -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:20087/api/v1/services/$SERVICE_ID
```

## 服务规格

创建服务时提交的是 `ServiceSpec`：

```json
{
  "name": "my-service",
  "description": "optional text",
  "provider": "process",
  "command": ["node", "server.js"],
  "working_dir": "/srv/my-service",
  "env": {
    "PORT": "3000",
    "NODE_ENV": "production"
  },
  "runtime": {},
  "restart": {
    "mode": "always",
    "max_retries": 0
  },
  "repair": {
    "mode": "hook",
    "command": ["/srv/my-service/scripts/repair.sh"],
    "working_dir": "/srv/my-service",
    "env": {},
    "timeout": "10m"
  },
  "health": [
    {
      "type": "http",
      "url": "http://127.0.0.1:3000/health",
      "interval": "30s",
      "timeout": "5s"
    }
  ],
  "enabled": true,
  "tags": ["api"]
}
```

字段说明：

- `name`：服务名，只允许字母、数字、`.`、`_`、`-`。
- `provider`：使用哪个 provider。
- `command`：结构化命令数组，不是 shell 字符串。
- `working_dir`：工作目录，可为空。
- `env`：环境变量。
- `runtime`：provider 专用选项。
- `restart.mode`：`no`、`on-failure`、`always`。
- `repair`：可选修复 hook。配置后，`POST /api/v1/services/:id/repair` 会执行这里的
  `command`；不配置时保持旧行为，即重新注册并重启服务。`restart` 和 `repair` 是两个
  独立操作，`restart` 不会触发修复 hook，修复 hook 失败时也不会自动 fallback 到重启。
- `health`：健康检查配置。
- `enabled`：是否启用。
- `tags`：标签。分组使用 `group:<name>` 标签，例如 `group:phone-control`。

托管服务命令规范：

- `command` 必须启动 service-manager 要托管的前台长进程。
- 如果 `command` 指向包装脚本，脚本最后必须用 `exec` 切换到真正的服务进程。不要把服务放到后台运行后让包装脚本退出。
- 被托管前台进程的 stdout/stderr 会由 provider 日志接口或底层服务平台捕获。服务日志应优先写到这里，不要只写隐藏的临时日志文件。
- `health` 只说明一个正在运行的服务是否可用，不能替代 provider 的进程生命周期跟踪。正确状态应同时满足：被跟踪进程仍在运行，并且健康检查通过。
- 修复 hook 的 stdout/stderr 会被有意丢弃。hook 失败只通过退出状态返回，避免 token、凭据、环境变量等敏感信息通过 API 泄露。

服务列表和状态接口支持 `tag` / `tags`、`group` / `groups` 查询参数。多个值可以用逗号分隔，多个条件会同时匹配。`GET /api/v1/services/statuses` 和 `GET /api/v1/groups/:name/status` 返回数组，每一项形如：

```json
{
  "service": {},
  "status": {},
  "error": ""
}
```

如果某个 provider 查询状态失败，该项会包含 `error`，不会影响其他服务的状态结果。

## Provider 说明

### process

直接启动本机进程，使用 PID、日志文件和 `meta.json` 跟踪状态。

适合：

- 简单脚本
- 开发环境服务
- 没有 systemd/runit/launchd 的场景

示例：

```json
{
  "provider": "process",
  "command": ["python3", "main.py"],
  "working_dir": "/srv/app",
  "runtime": {}
}
```

安全说明：

- Unix 下子进程会放进独立进程组。
- 停止服务前会校验 `/proc/<pid>/stat` starttime 和 cmdline。
- 如果无法确认 PID 属于该服务，会拒绝发送信号，避免 PID 复用误杀。

### proot-distro

通过 Termux 的 `proot-distro` 进入 Ubuntu 等发行版运行服务。

适合：

- 当前机器是 Termux
- 服务实际运行在 Ubuntu proot 中

示例：

```json
{
  "provider": "proot-distro",
  "command": ["node", "server.js"],
  "working_dir": "/srv/api",
  "runtime": {
    "distro": "ubuntu"
  }
}
```

等价于：

```bash
proot-distro login ubuntu -- node server.js
```

如果 `proot-distro` 不在 `PATH`，程序也会尝试探测：

```text
/data/data/com.termux/files/usr/bin/proot-distro
```

### termux-services

通过 Termux 外层的 `termux-services`/`runit` 管理服务。

适合：

- 服务要由外层 Termux 常驻托管
- 希望崩溃后由 runit 拉起

要求：

```bash
pkg install termux-services
```

程序会查找：

```text
$PREFIX/bin/sv
$PREFIX/var/service
/data/data/com.termux/files/usr/bin/sv
```

### systemd

使用 Linux 用户级 systemd 服务。

适合：

- 普通 Linux 桌面或服务器
- `systemctl --user` 可用

在 proot 环境里，`systemctl` 可能存在但处于 `offline`，程序会把它识别为不可用，而不是误判可用。

### launchd

使用 macOS 用户级 LaunchAgent。

适合：

- macOS 用户服务
- 不需要 sudo 的本机启动项

### docker

使用 Docker CLI 管理容器。

要求：

- `docker` CLI 可用
- Docker daemon 可连接

`runtime.image` 是必需的：

```json
{
  "provider": "docker",
  "command": ["node", "server.js"],
  "runtime": {
    "image": "node:22-alpine"
  }
}
```

## Termux + Ubuntu proot 推荐部署

开发可以放在 Ubuntu proot：

```text
/root/service-manager
```

但长期运行管理器时，更推荐把最终二进制部署到外层 Termux，由 Termux 管理它。

原因：

- 外层 Termux 更适合托管常驻进程。
- Ubuntu proot 不是真正的系统 init 环境。
- `systemd` 在 proot 中通常不可用或 offline。

推荐结构：

```text
Android
└─ Termux
   ├─ service-manager
   ├─ termux-services / runit
   └─ proot-distro ubuntu
      └─ 实际业务服务
```

如果二进制是在 Ubuntu proot 中用 glibc 构建的，它不一定能直接在外层 Termux 运行。要部署到外层 Termux，最好在 Termux 原生环境里构建或下载匹配 Termux 的预编译二进制。

## 安全建议

- 默认只监听 `127.0.0.1`。
- 不要把服务直接暴露到公网。
- 需要远程访问时，优先使用 SSH tunnel、Tailscale 或 Cloudflare Tunnel。
- API token 视为本机管理权限，泄露后对方可以注册并启动本机命令。
- 外部注册服务时使用结构化 `command` 数组，不要把未校验的 shell 字符串传进来。

## 排错

### Web UI 显示未授权

查看 token：

```bash
service-manager token show
```

把 token 填入页面右上角并保存。

### provider 不可用

查看诊断：

```bash
service-manager doctor
```

或者：

```bash
curl -fsS \
  -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:20087/api/v1/providers
```

### systemd 显示 offline

这是 proot/容器环境常见情况。不要在这种环境里依赖 systemd，改用：

- `process`
- `proot-distro`
- 外层 Termux 的 `termux-services`

### termux-services 不可用

安装：

```bash
pkg install termux-services
```

重新打开 Termux 会话，或者确认 `sv` 是否存在：

```bash
command -v sv
```

### 端口被占用

换端口：

```bash
service-manager serve --bind 127.0.0.1:20088
```

### 启动服务后无法停止

`process` 和 `proot-distro` provider 会校验 PID 归属。如果 `meta.json` 缺失或 `/proc` 无法读取，程序会拒绝停止，避免误杀其他进程。

这种情况下需要手动检查：

```bash
service-manager doctor
```

然后根据服务目录中的日志和 pid 文件人工处理。

## 发布打包

构建 release：

```bash
./scripts/build-release.sh
```

脚本会构建 release 二进制，并打包二进制、脚本和 README。
