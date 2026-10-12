# 在同一台 Mac 上运行原生 / Chromium 对比

这套脚本测两个固定场景：256 个节点的 append/remove，以及 class 修改 / matches，
各重复 32 轮。计时包含脚本、DOM API 和正确性检查，不包含绘制完成、屏幕呈现或启动。
它不能证明“所有场景都超过浏览器”。渲染、滚动、输入延迟、动画、内存等仍未测。

## 1. 准备环境

使用这条独立性能分支的干净 checkout；不要同时编辑源码、更新浏览器或运行其他构建。
需要（[Rust 官方安装说明](https://doc.rust-lang.org/book/ch01-01-installation.html)）：

- macOS 14 或更新版（按当前 [Playwright 支持范围](https://playwright.dev/python/docs/intro)）
- macOS Command Line Tools / Xcode 的 C/C++ 编译器和 SDK（用 `xcode-select -p` 检查）
- 已安装 Rust / Cargo，支持 Rust 2024 edition 及锁定依赖。历史 Linux 测试用 Rust 1.99.0；
  Mac 尚未实机验证。记录 `rustc -Vv`，不要把不同版本测量混在一起。
- Python 3.11 或更新版
- 已安装 Google Chrome 或 Chromium；支持 `/Applications`、`~/Applications` 中的标准路径，
  也可显式指定可执行文件（不是 `.app` 目录）
- Python Playwright 客户端。下面仅安装到项目的 `target` 虚拟环境，不下载浏览器。

在仓库根目录按需手动运行：

```sh
python3 --version
rustc -Vv
xcode-select -p
python3 -m venv target/perf-venv
target/perf-venv/bin/python -m pip install -r tools/perf-paired/requirements.txt
```

脚本不会安装系统工具、修改安全设置、删除隔离属性或禁用浏览器沙箱。
缺少工具时先按对应供应商的正常安装方式解决；不要通过关闭安全功能绕过启动错误。
不要同时使用 Rosetta 的 x86_64 浏览器和原生 arm64 工具链，或混用远程浏览器与本地程序。

## 2. 构建 CLI

`Cargo.lock` 在上游被忽略。这次保留了实际测试的工作区和生成程序锁文件。
在一个新 checkout 中复制工作区锁，然后只构建无图形依赖的 CLI：

```sh
test ! -e Cargo.lock && cp tools/perf-paired/locks/workspace.Cargo.lock Cargo.lock
cargo build --release --locked -p w3cos-cli
```

已有 Cargo.lock 时不要盲目覆盖。使用新 checkout，或先自行保留现有锁并核对版本。
这里不需要构建整个默认 graphics workspace，避免把未通过的默认特性组合误当作前置步骤。
构建仍需要平台系统库，链接失败会保留具体错误；这不是自动安装依赖的脚本。

## 3. 检查并运行

```sh
target/perf-venv/bin/python tools/perf-paired/run_local.py --preflight-only
target/perf-venv/bin/python tools/perf-paired/run_local.py
```

自定义路径示例：

```sh
target/perf-venv/bin/python tools/perf-paired/run_local.py \
  --w3cos "$PWD/target/release/w3cos" \
  --chromium "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
```

运行器会在 checkout 的同级目录创建独立输出目录（`../仓库名-perf-results/local-*`），构建原生 fixture，准备锁定依赖，保留原始构建记录后
按指定锁离线重建，再在同一台机器上启动独立原生进程和浏览器进程，交替执行 8 对。
首次运行默认允许 Cargo 获取锁定的构建依赖；`--offline` 禁止 Cargo 获取，缺缓存则明确失败。
Cargo 的离线模式不是网络沙箱，不能阻止依赖自身构建脚本另行联网。
锁文件必须一致，不能为获得成功而让 Cargo 改成另一个版本。没有权限或不支持平台依赖
时，保留错误交回处理。

每个进程每个场景先做 3 次排除的 warmup，再记录 7 次正式样本。相同 fixture 字节、
工作量、状态校验及窗口环境 1024×768 / DPR 1，不使用原生 cpu_frame 去对比浏览器 rAF。
浏览器用独立离线上下文，不读取个人浏览器账号或现有标签页；保持沙箱启用。

接电，关闭其他负载，保持相同电源/温度条件。不应只选最快的一次，或看到有利结果就停止。
脚本输出的文件夹内保存构建日志、锁、源码/二进制摘要、逐次样本和独立验证结果。
把整个结果文件夹压缩后交回来，尤其是失败时的部分样本与日志。

## 4. 如何解释结果

- `pass`：两个已覆盖场景都满足严格门槛
- `fail`：证据显示场景更慢
- `inconclusive`：样本不够确定或达不到预设优势
- `blocked`：依赖、启动、功能校验或证据不完整

只有 pass 返回成功退出码；其他状态均非零。非零不等于脚本损坏。
每对进程内先取中位数，再做预先固定的配对 bootstrap；两场景进行多重比较调整。
原生/浏览器比值区间上界须低于 0.95，而且任何一对都不能出现原生中位数不快于浏览器。
不能用总平均隐藏较慢场景，也不能把未运行的场景标成通过。

## 已验证与未验证

本分支包含 main v2 的惰性祖先过滤、console / live childNodes / sibling、共享字体克隆
改动，以及 Location 对象缓存。可选的本地字体注册优化未包含。
Linux 原生组合已有记录的测试与原生前后对比；当前环境无法启动受沙箱保护的 Chromium。
macOS 全链路尚未运行。现有全量正确性测试有基线失败，详细数字与限制见
[performance-validation.md](../../docs/performance-validation.md)。此次脚本测试通过不代表全量 CI 通过。
