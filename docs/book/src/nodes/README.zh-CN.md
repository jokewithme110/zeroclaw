# ZeroClaw DT Nodes 简明手册

`zeroclaw-dt-nodes` 是一个轻量级节点运行时：

- `start` 通过 WebSocket 接入 ZeroClaw gateway
- `chat` 通过 webchat HTTP 接口向 ZeroClaw 发消息
- `event-emit` 通过 webchat HTTP 把事件推送给已订阅接收方
- `event list` 查看节点本地保存的订阅记录

这份手册只保留四件事：

- 服务端如何编译、配置、启动
- 客户端如何编译、配置、启动
- 事件订阅和推送如何配置、如何走通
- 常见问题如何排查

## 1. 服务端

### 1.1 编译

静态地址模式至少需要 gateway 带上 `node-control` feature：

```bash
cargo build --release --features node-control
```

如果要启用 FIFO 自动发现，直接编译 `auto_discovery` 即可。这个 feature 会同时带上 gateway 侧和 node 侧的自动发现逻辑：

```bash
cargo build --release --features auto_discovery
```

生成的服务端二进制：

```text
./target/release/zeroclaw
```

### 1.2 静态地址模式最小配置

服务端至少要准备两个入口：

- gateway 根路径 WebSocket：给节点 `start` 用
- webchat HTTP：给节点 `chat` / `event-emit` 用

最小配置示例：

```toml
[gateway]
host = "0.0.0.0"
port = 42617
require_pairing = true
paired_tokens = ["replace-me"]

[gateway.node_control]
enabled = true

[channels.webchat]
port = 42618
listen_path = "/response"
```

说明：

- `zeroclaw-dt-nodes start` 当前连的是 `ws://<gateway.host>:<gateway.port>/`
- 节点侧 `gateway.token` 对应的是服务端 `[gateway].paired_tokens` 中的某一个 token
- `gateway.node_control.auth_token` 是 node-control API 的独立口令，不是 `zeroclaw-dt-nodes start` 使用的 token
- 如果服务端 `[gateway].require_pairing = false`，节点侧可以不配 `gateway.token`
- 节点发送聊天和事件时，请求地址是 `http://<gateway.host>:<event_destination.port><event_destination.path>`
- 所以 `listen_path` 最省事的写法通常就是 `/response`

### 1.3 FIFO 自动发现模式配置

如果要让节点通过 FIFO 自动发现 gateway，需要同时满足：

- 服务端编译时带 `auto_discovery` feature
- `[gateway.node_control].enabled = true`
- `[gateway.node_control.auto_discovery].enabled = true`
- 服务端全局 `[secrets].encrypt = true`

最小配置示例：

```toml
[gateway]
host = "0.0.0.0"
port = 42617
require_pairing = true
paired_tokens = []

[secrets]
encrypt = true

[gateway.node_control]
enabled = true

[gateway.node_control.auto_discovery]
enabled = true
fifo_dir = "/home/your-user/clawfifo/server"
fifo_wait_timeout_secs = 300
gateway_announce_retries = 5
ip_intf = "eth0"

[channels.webchat]
port = 42618
listen_path = "/response"
```

说明：

- gateway 自动发现发布使用的是 `[gateway].paired_tokens` 的主 token
- 如果 `paired_tokens` 为空，gateway 会自动生成一个可恢复的 token，并回写到自己的 `config.toml`
- gateway 会向 `<fifo_dir>/claw_fifo_out` 写入发现消息
- 当前实现要求 `secrets.encrypt = true`，否则 gateway 不会启用自动发现发布
- `ip_intf` 用来指定发布时采用哪个网卡的 IPv4 地址

如果 node 和 gateway 之间还有中间件层，通常由中间件负责把服务端 `claw_fifo_out` 的数据转发到客户端 `claw_fifo_in`。

### 1.4 启动

```bash
./target/release/zeroclaw gateway start
```

## 2. 客户端

### 2.1 编译

静态地址模式：

```bash
cargo build -p zeroclaw-dt-nodes --release
```

如果要启用 FIFO 自动发现：

```bash
cargo build -p zeroclaw-dt-nodes --release --features auto_discovery
```

生成的客户端二进制：

```text
./target/release/zeroclaw-dt-nodes
```

### 2.2 初始化

```bash
./target/release/zeroclaw-dt-nodes init
```

默认配置目录：

```text
$HOME/.zeroclaw_node/
```

默认配置文件：

```text
$HOME/.zeroclaw_node/config.toml
```

如果要放到自定义目录：

```bash
ZEROCLAW_NODE_CONFIG_DIR=/path/to/node \
  ./target/release/zeroclaw-dt-nodes init
```

这时实际配置文件路径会变成：

```text
/path/to/node/.zeroclaw_node/config.toml
```

### 2.3 静态地址模式配置

初始化后，编辑：

```text
$HOME/.zeroclaw_node/config.toml
```

最小配置示例：

```toml
[gateway]
host = "127.0.0.1"
port = 42617
token = "replace-me"
events = ["router.alert"]

[event_destination]
port = 42618
path = "/response"

[secrets]
encrypt = true

[gateway.node_control.auto_discovery]
enabled = false
fifo_dir = "/var"
fifo_wait_timeout_secs = 300
```

说明：

- `gateway.host` / `gateway.port` 要指向服务端 gateway
- `gateway.token` 要和服务端 `[gateway].paired_tokens` 中的某个 token 一致
- `gateway.node_control.auth_token` 与这里无关
- `events` 是“允许订阅的事件白名单”
- 如果这里配置了白名单，而你下发的 topic 不在列表里，订阅会失败
- 如果这里不配置 `events`，当前实现等价于“不限制 topic”
- `event_destination.path` 要和服务端 `[channels.webchat].listen_path` 一致
- `event_destination` 没有单独的 host 配置，`chat` 和 `event-emit` 总是复用当前 `[gateway].host`
- 静态模式下，手工配置的 `gateway.token` 始终按明文处理，不受 `[secrets].encrypt` 影响

如果你准备做事件订阅，建议在启动节点前就明确写上：

```toml
[gateway]
host = "127.0.0.1"
port = 42617
token = "replace-me"
events = ["router.alert"]
```

### 2.4 FIFO 自动发现模式配置

如果启用自动发现，`start` 会优先等待 FIFO 里的发现结果，再用发现到的地址和 token 建立 WebSocket 连接。

最小配置示例：

```toml
[gateway]
host = "127.0.0.1"
port = 42617
events = ["router.alert"]

[event_destination]
port = 42618
path = "/response"

[secrets]
encrypt = true

[gateway.node_control.auto_discovery]
enabled = true
fifo_dir = "/home/your-user/clawfifo/client"
fifo_wait_timeout_secs = 300
```

说明：

- `auto_discovery.enabled = false` 或缺省时，`start` 走静态地址模式
- `auto_discovery.enabled = true` 时，`start` 会监听 `<fifo_dir>/claw_fifo_in`
- 发现成功后，节点会把发现到的 `host`、`port`、`token` 回写到本地 `config.toml` 的 `[gateway]`
- 后续 `chat` 和 `event-emit` 直接读取这个最新的 `[gateway]`，不再依赖额外的缓存文件
- 如果 `[secrets].encrypt = true`，自动发现回写的 `gateway.token` 会按密文落盘
- 如果 `[secrets].encrypt = false`，自动发现回写的 `gateway.token` 会按明文落盘
- 静态模式下这个开关不影响手工填写的 `gateway.token`

发现消息当前使用的 JSON 形态如下：

```json
{
  "ip": "127.0.0.1",
  "port": 42617,
  "token": "zc_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
  "timestamp": 1780908217298
}
```

如果你有中间件层，需要注意：

- gateway 侧默认发布文件名是 `<fifo_dir>/claw_fifo_out`
- node 侧默认监听文件名是 `<fifo_dir>/claw_fifo_in`
- 当前 node 监听器把“一次写入后关闭写端”当作一条完整消息的结束标志
- 所以中间件写完一条 JSON 后应关闭写端，否则 `start` 可能一直等待 EOF

### 2.5 启动

静态地址模式：

```bash
./target/release/zeroclaw-dt-nodes start
```

如果只想交互式修改客户端里的 gateway 地址和 token：

```bash
./target/release/zeroclaw-dt-nodes start --interactive
```

静态地址模式支持以下覆盖参数：

```bash
./target/release/zeroclaw-dt-nodes start \
  --host 192.168.1.10 \
  --port 42617 \
  --token replace-me \
  --name my-node
```

说明：

- `--host` / `--port` / `--token` / `--interactive` 只影响静态地址模式
- 自动发现模式下，连接地址和 token 只来自 FIFO 发现结果
- `--name` 在两种模式下都可用，用来覆盖节点显示名

如果当前二进制是用 `--features auto_discovery` 编译出来的，还支持这些参数：

```bash
./target/release/zeroclaw-dt-nodes start \
  --auto-discovery \
  --fifo-dir /home/your-user/clawfifo/client \
  --fifo-wait-timeout-secs 300
```

也可以显式强制回退到静态模式：

```bash
./target/release/zeroclaw-dt-nodes start --no-auto-discovery
```

## 3. 如何使用

### 3.1 启动顺序

静态地址模式建议按这个顺序：

1. 先启动服务端 `zeroclaw gateway start`
2. 再启动客户端 `zeroclaw-dt-nodes start`
3. 最后用 `chat` 做联通测试

自动发现模式建议按这个顺序：

1. 启动服务端 `zeroclaw gateway start`
2. 启动软总线或中间件转发层
3. 启动客户端 `zeroclaw-dt-nodes start`
4. 等待本地 `config.toml` 的 `[gateway]` 被发现结果刷新
5. 再用 `chat` 或 `event-emit` 做联通测试

### 3.2 发送测试消息

```bash
./target/release/zeroclaw-dt-nodes chat --message "Hello from the node"
```

如果服务端和客户端配置对齐，节点会通过 webchat 把这条消息发到 ZeroClaw。

如果要同时发送图片：

```bash
./target/release/zeroclaw-dt-nodes chat \
  --message "请描述这张图片" \
  --images ./demo.jpg
```

多张图片用逗号分隔：

```bash
./target/release/zeroclaw-dt-nodes chat \
  --message "请描述这些图片" \
  --images ./a.png,./b.jpg,./c.webp
```

说明：

- `--images` 传的是节点本机上的图片文件路径
- 当前实现会把图片转成 base64 后再发给 webchat
- 常用格式可按文件后缀直接使用：`jpg`、`jpeg`、`png`、`gif`、`webp`
- `chat` 总是读取当前 `config.toml` 里的 `[gateway]` 和 `[event_destination]`
- 自动发现模式下，只要你已经成功跑过一次 `start`，后续 `chat` 不需要再额外传发现参数

### 3.3 事件订阅与推送

先分清两件事：

- 订阅是通过 gateway 侧的 `nodes` 工具下发到节点
- 推送是节点本地执行 `event-emit`，把事件通过 webchat 发出去

特别说明：

- 事件订阅通常不需要在节点机器本地敲命令
- 只要 QQ 等已接入同一个 ZeroClaw gateway 的 app 能和 agent 对话，并且 agent 可调用 `nodes` 工具，就可以直接在这些 app 里下达订阅指令
- 如果你的 gateway 已经接入了 `qq`、Telegram、Discord 等 app/channel，日常使用时可以直接在这些 app 里给 ZeroClaw 下指令完成订阅
- 例如在 `qq` 里告诉 ZeroClaw：“给节点 `zeroclaw-node-abc123` 订阅 `router.alert`，推送到 `qq` 群 `group:987654`”
- 这里说的 `nodes` 是 gateway 侧的工具动作，不是本地 CLI 子命令
- 下面的 JSON 示例更适合联调、排错和手工验证

要走通这条链路，配置要同时满足：

- 服务端开启 `[gateway.node_control].enabled = true`
- 如果服务端 `[gateway].require_pairing = true`，客户端 `gateway.token` 要与服务端 `[gateway].paired_tokens` 中的某个 token 一致
- 自动发现模式下，`start` 成功后会自动把发现到的 token 回写进客户端本地 `[gateway]`
- 服务端 `[channels.webchat].listen_path` 和客户端 `[event_destination].path` 一致
- 客户端如配置了 `gateway.events`，你要订阅的 topic 必须在白名单里

订阅记录会保存在：

```text
$HOME/.zeroclaw_node/events/subscriptions.json
```

最小流程如下。

1. 先在客户端配置可订阅事件：

静态地址模式示例：

```toml
[gateway]
host = "127.0.0.1"
port = 42617
token = "replace-me"
events = ["router.alert"]

[event_destination]
port = 42618
path = "/response"
```

自动发现模式示例：

```toml
[gateway]
events = ["router.alert"]

[event_destination]
port = 42618
path = "/response"

[secrets]
encrypt = true

[gateway.node_control.auto_discovery]
enabled = true
fifo_dir = "/home/your-user/clawfifo/client"
fifo_wait_timeout_secs = 300
```

2. 启动服务端：

```bash
./target/release/zeroclaw gateway start
```

3. 启动客户端：

```bash
./target/release/zeroclaw-dt-nodes start
```

4. 在 gateway 的 `nodes` 工具里先查看在线节点：

```json
{
  "action": "status"
}
```

5. 再下发订阅：

```json
{
  "node": "zeroclaw-node-abc123",
  "action": "event_subscribe",
  "topics": ["router.alert"],
  "channel": "qq",
  "recipient": "group:987654"
}
```

实际使用时，很多场景不需要你手工拼这段 JSON。更常见的做法是：

- 在 QQ 等已接入 gateway 的 app 里，直接让 agent 给某个节点订阅事件
- agent 会在后台调用 `nodes` 工具，完成 `event_subscribe` 下发

6. 回到节点机器上确认订阅已经写入：

```bash
./target/release/zeroclaw-dt-nodes event list --event router.alert --json
```

7. 本地发送一个测试事件：

```bash
./target/release/zeroclaw-dt-nodes event-emit \
  --event router.alert \
  --message "Router CPU > 80%"
```

如果配置正确，这条事件会按订阅记录，通过 webchat 推送到 `qq / group:987654`。

如果要在事件通知里同时带图片：

```bash
./target/release/zeroclaw-dt-nodes event-emit \
  --event router.alert \
  --message "CPU 使用率过高" \
  --images ./router-status.jpg
```

多张图片同样用逗号分隔：

```bash
./target/release/zeroclaw-dt-nodes event-emit \
  --event router.alert \
  --message "请查看路由器截图" \
  --images ./cpu.png,./memory.png
```

说明：

- 这些图片会随 `router.alert` 事件一起推送给已订阅接收方
- 图片文件必须存在于节点本机；路径写错时，命令会直接失败
- `event-emit` 只会对当前 topic 的本地订阅记录逐条发送 HTTP 请求
- 如果某个订阅发送失败，命令会打印失败项和汇总结果

效果示意：

![QQ 中下发事件订阅并接收告警](./docs-assets/event-subscribe-qq-demo.svg)

![本地 event-emit 推送成功](./docs-assets/event-emit-cli-demo.svg)

要点：

- `event_subscribe` 不是节点本地 CLI 子命令，而是 gateway `nodes` 工具动作
- `event list` 只负责查看本地订阅
- `event-emit` 只负责把某个 topic 推给当前已订阅的接收方

### 3.4 事件相关命令一览

先区分两类。

节点本地命令：

- `event-emit`：把某个事件推送给当前已经订阅的接收方
- `event list`：查看本地保存的订阅记录

示例：

```bash
./target/release/zeroclaw-dt-nodes event list
./target/release/zeroclaw-dt-nodes event list --event router.alert --json

./target/release/zeroclaw-dt-nodes event-emit \
  --event router.alert \
  --message "CPU 使用率过高"
```

gateway 侧 `nodes` 工具动作：

- `event_subscribe`：给节点新增订阅
- `event_unsubscribe`：取消节点上的某条订阅
- `event_subscribe_query`：查询节点当前已有的订阅

示例：

```json
{
  "node": "zeroclaw-node-abc123",
  "action": "event_subscribe",
  "topics": ["router.alert"],
  "channel": "qq",
  "recipient": "group:987654"
}
```

```json
{
  "node": "zeroclaw-node-abc123",
  "action": "event_subscribe_query",
  "channel": "qq"
}
```

```json
{
  "node": "zeroclaw-node-abc123",
  "action": "event_unsubscribe",
  "topics": ["router.alert"],
  "channel": "qq",
  "recipient": "group:987654"
}
```

注意：

- 节点本地没有 `event subscribe` 这种 CLI 子命令
- 订阅、退订、查询订阅，都是通过 gateway 的 `nodes` 工具完成
- 节点本地只负责查看订阅记录和发送事件

### 3.5 命令参数分隔

如果你直接执行二进制，不要加 `--`：

```bash
./target/release/zeroclaw-dt-nodes chat --message "hello"
```

如果你用 `cargo run`，才需要 `--`：

```bash
cargo run -p zeroclaw-dt-nodes -- chat --message "hello"
```

## 4. 常见问题

### 4.1 `start` 报 404 或 `Node WebSocket is disabled`

优先检查：

- 服务端是否真的启动了 `zeroclaw gateway start`
- 服务端是否带 `node-control` feature 编译
- `[gateway.node_control].enabled = true` 是否已打开
- 静态模式下客户端 `gateway.host` / `gateway.port` 是否写对

当前实现有两种连接路径：

- 静态模式：直接连 `ws://<gateway.host>:<gateway.port>/`
- 自动发现模式：先等 `<fifo_dir>/claw_fifo_in`，再连发现到的 `ws://<discovered_host>:<discovered_port>/`

### 4.2 `start` / `chat` / `event-emit` 报未授权

优先检查：

- 服务端 `[gateway].require_pairing` 是否为 `true`
- 客户端 `[gateway].token` 是否与服务端 `[gateway].paired_tokens` 中的某个 token 一致
- 不要把客户端 token 配成 `[gateway.node_control].auth_token`
- 自动发现模式下，先跑通一次 `start`，确认本地 `[gateway].token` 已被发现结果写回

### 4.3 自动发现模式没有生效

优先检查：

- 客户端是否用 `--features auto_discovery` 编译
- 服务端是否用 `--features auto_discovery` 编译
- 客户端 `[gateway.node_control.auto_discovery].enabled = true`
- 服务端 `[gateway.node_control.auto_discovery].enabled = true`
- 客户端监听的是 `<fifo_dir>/claw_fifo_in`
- 服务端发布的是 `<fifo_dir>/claw_fifo_out`
- 中间件是否真的把服务端发现消息转发到了客户端 `claw_fifo_in`
- 当前实现要求写完一条 JSON 后关闭 FIFO 写端，否则节点可能一直等不到 EOF

自动发现成功后，可以直接检查客户端 `config.toml` 的 `[gateway]` 是否已经被刷新。

### 4.4 `chat` 或 `event-emit` 报 404

优先检查这两个配置是否一致：

- 服务端：`[channels.webchat].listen_path`
- 客户端：`[event_destination].path`

最简单的对齐方式就是两边都用：

```text
/response
```

同时注意：

- `chat` 和 `event-emit` 的 host 取自当前 `[gateway].host`
- 所以如果自动发现刚把 `[gateway].host` 改掉，后续 HTTP 请求也会自动跟着新 host 走

### 4.5 `unexpected argument 'chat' found`

这是因为你把 `cargo run` 的写法误用了到直接二进制。

错误写法：

```bash
./target/release/zeroclaw-dt-nodes -- chat --message "hello"
```

正确写法：

```bash
./target/release/zeroclaw-dt-nodes chat --message "hello"
```

### 4.6 `init` 提示配置文件已存在

说明这个目录里已经初始化过了。处理方式二选一：

- 直接修改现有 `config.toml`
- 换一个新的 `ZEROCLAW_NODE_CONFIG_DIR`

## 附录：常见环境打包命令

以下命令默认都在仓库根目录执行，示例以 `zeroclaw-dt-nodes` 为主。

### 当前机器

静态地址模式：

```bash
cargo build -p zeroclaw-dt-nodes --release
```

自动发现模式：

```bash
cargo build -p zeroclaw-dt-nodes --release --features auto_discovery
```

产物：

```text
target/release/zeroclaw-dt-nodes
```

服务端 `zeroclaw` 当前机器构建：

静态地址模式：

```bash
cargo build --release --features node-control
```

自动发现模式：

```bash
cargo build --release --features auto_discovery
```

### Linux x86_64 动态包

```bash
rustup target add x86_64-unknown-linux-gnu
cargo build -p zeroclaw-dt-nodes --release \
  --target x86_64-unknown-linux-gnu
```

产物：

```text
target/x86_64-unknown-linux-gnu/release/zeroclaw-dt-nodes
```

### Linux x86_64 静态包

```bash
rustup target add x86_64-unknown-linux-musl
cargo build -p zeroclaw-dt-nodes --release \
  --target x86_64-unknown-linux-musl
```

产物：

```text
target/x86_64-unknown-linux-musl/release/zeroclaw-dt-nodes
```

### Linux ARM64 静态包

```bash
rustup target add aarch64-unknown-linux-musl
cargo build -p zeroclaw-dt-nodes --release \
  --target aarch64-unknown-linux-musl
```

产物：

```text
target/aarch64-unknown-linux-musl/release/zeroclaw-dt-nodes
```

### Windows x86_64

```bash
rustup target add x86_64-pc-windows-msvc
cargo build -p zeroclaw-dt-nodes --release \
  --target x86_64-pc-windows-msvc
```

产物：

```text
target/x86_64-pc-windows-msvc/release/zeroclaw-dt-nodes.exe
```
