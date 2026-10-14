# 共享运行时、VM 与工具链

状态：垂直切片。ABI 主版本 1。测量数字在文末，随这次构建填写。

## 代码里的事实

`w3cos build app.tsx -o app`（`crates/w3cos-cli`）把 TSX 转成一个临时 Cargo 工程，这个工程用 path 依赖 `w3cos-runtime`，再 `cargo build` 出**静态链接**的 `w3cos-app`。每个应用二进制都带一份布局、渲染和窗口代码。普通 AOT 应用**不**链接 `w3cos-compiler` / `w3cos-ir` / `w3cos-vm`（`scripts/check-aot-dependency-boundary.sh`）。

虚拟机是存在的。动态文档走 SWC → W3IR → `crates/w3cos-vm`，由 runtime 的 `dynamic-js` feature 带进来。`dynamic_script.rs` 只在这个 feature 下编译。网页解析在 `html_tree_builder`，脚本执行在 `ScriptLoader`，浏览器页面的能力边界在 `browser_page_domain`：Shell 只拿不可变快照，不能把文件或设备能力交给页面。

`ARCHITECTURE.md` 里的 “no runtime, no VM, no GC” 和 “Vello 为主” 已经过时。README 和 `Cargo.toml` 与代码一致：AOT 是 rustc/LLVM；动态文档是 W3VM；二维渲染主路径是 Skia，Vello 和 tiny-skia 是兼容后端。`cpu-render` 用 tiny-skia + softbuffer，这是无 GPU 的 Xvfb 路径。

## 决策

**系统安装一份 `libw3cos_runtime.so`（cdylib，静态链入布局、渲染、W3VM 和编译器）。AOT 应用是这份运行时加载的组件包，不把运行时再链进应用。网页在同一份 `.so` 里的 W3VM 上执行。**

没有选“全部应用都是字节码、由共享 VM 解释”的原因：

- 项目目标是接近原生的性能。W3VM 解释 W3IR，热路径比 LLVM 机器码慢一个数量级以上，这和 ROADMAP 的北极星相反。
- ROADMAP Phase 3.7 已经写了应用包 = AOT 业务代码 + 元数据 + 资源 + 共享运行时引用。
- 现有 AOT 边界明确禁止普通应用链接编译器和 W3VM。

也没有选“应用是 Rust dylib，prefer-dynamic 链接 `w3cos-runtime`”。实测过：`-C prefer-dynamic` 要求依赖以 rust dylib 存在，而 taffy、winit、`w3cos-core` 这些 crate 只有 rlib。最终链接报 `required to be available in rlib format, but was not found in this form`。要把整条依赖图都变成 dylib，crates.io 的包做不到，除非镜像自己重编并冻结 rustc ABI。那不是第一刀该承诺的。

所以这一刀的应用格式是 **bundle**：编译期把静态组件树序列化成 JSON（`abi` + `component`），运行时 `w3cos_runtime_run_bundle` 反序列化一次，然后走和静态二进制相同的 `run_app_static` 绘制路径。布局和绘制仍是本机代码。代价是这一刀只覆盖**静态 UI**（没有信号和自定义事件闭包）。有行为的应用仍要以后的代码插件，见文末待决问题。

产品版本号在 `w3cos-abi`：高 16 位主版本，低 16 位次版本。Host 只在主版本相同、且运行时次版本 ≥ bundle 声明的次版本时加载。当前是 `1.0`（`0x00010000`）。

运行时导出：

- `w3cos_runtime_run_bundle()`：加载静态 UI bundle 并开窗口

- `w3cos_runtime_abi_version()`
- `w3cos_runtime_page_text()`：无窗口，把 HTML 载入 W3VM，返回 body 文本
- `w3cos_runtime_run_page()`：开窗口渲染同一份文档
- `w3cos_runtime_last_error()`

`w3cos-runtime-host` 自己不链接运行时。它 `dlopen` 系统库 `libw3cos_runtime.so`，然后调用 `w3cos_runtime_run_bundle` 或 `w3cos_runtime_run_page`。网页和静态 UI 因此用的是同一份 `.so` 里的运行时；网页脚本跑在这份库里的 W3VM 上。

## 和需求不一致的地方

1. **不要把 AOT 应用放进 VM。** 需求说“链接系统运行时”和“网页在 VM 上跑”，这两句可以同时成立，但“所有应用都在 VM 里执行”不行。切片按链接系统 dylib 实现。若 owner 坚持字节码应用，启动和帧时间都会离开现在的 AOT 数字，需要单独的性能预算。
2. **Shell 桌面仍然静态链接自己的一份运行时。** 把现在的 `w3cos-shell` 改成动态链接同一份 cdylib，会让默认 CI 构建依赖那份库，并且和 #56 刚落地的 chrome 抢同一条链接配置。切片里 Shell 通过子进程 `w3cos-runtime-host` 启动共享应用和网页。桌面按钮只在 `W3COS_SHARED_LAUNCHER=1` 时出现（镜像的 `S99w3cos` 在 `/usr/lib/w3cos` 存在时会设这个变量），默认桌面布局不变。
3. **网页和 Shell 不在同一个进程里。** 同一个 VM **实现**（那一份 dylib），不是 Shell 进程里的同一个 VM 实例。页面进程拿不到 AI bridge 的 token，也拿不到 Shell 的文件系统能力。`load_html_on_shared_vm` 把 `allow_network` 设为 false。这比“Shell 进程里直接 eval 页面”更接近现有的 `browser_page_domain` 隔离。生产环境仍应使用那个 worker 边界，而不是在 Shell 进程里跑不受信任的脚本。
4. **镜像默认不塞完整 rustc。** rustc sysroot 是几百 MB，和现在 50–100 MB ISO 的目标冲突。`post-build.sh` 在 `W3COS_INSTALL_RUSTC=1` 时才复制 sysroot。镜像始终带运行时 dylib、host、`w3cos` CLI 和 `/usr/bin/w3cos-toolchain`。设备上编译需要 owner 接受 ISO 变大，或者把工具链放在单独的 SDK 介质里。

## 启动

```text
w3cos-shell --shared-app examples/shared-hello/shared-hello.w3app
w3cos-shell --shared-page examples/shared-hello/page.html
w3cos-shell --shared-page --shared-headless examples/shared-hello/page.html
w3cos-runtime-host app examples/shared-hello/shared-hello.w3app
w3cos-runtime-host page --headless examples/shared-hello/page.html
```

桌面在 `W3COS_SHARED_LAUNCHER=1` 时多两个按钮：Shared Hello、Web VM。它们 `spawn` host，不把页面放进 Shell 的意图会话里执行。

构建：

```bash
scripts/build-shared-runtime.sh
cargo build --release -p w3cos-runtime-host -p w3cos
w3cos build examples/shared-hello/app.tsx -o shared-hello.w3app --shared --release
```

静态对照（同一渲染 feature，避免和 GPU/Skia 比体积）：

```bash
W3COS_RUNTIME_FEATURES=cpu-render w3cos build examples/shared-hello/app.tsx -o hello-static --release
```

## 性能

这一刀的应用包是数据，不是插件机器码。加载之后走 `run_app_static`：布局和绘制是运行时 cdylib 里的本机代码，没有解释器。W3VM 只跑网页脚本。

冷加载一份几十 MB 的 `libw3cos_runtime.so` 是动态链接器的开销，之后帧时间和静态 AOT 走同一条 `paint_cpu`。把同一份 UI 放进 W3VM 解释执行会把布局外面的脚本逻辑变成解释开销，不满足“接近原生”。

体积上，N 个静态应用大约是 N 份运行时。共享之后是 1 份运行时 + N 个组件 bundle。静态 UI 的 bundle 已经远小于 Phase 3.7 的“简单系统链接应用 ≤ 1 MB”。有信号和自定义闭包的应用还不在这个格式里。

这次测量关掉了 fat LTO，`codegen-units=16`，feature 是 `cpu-render`（运行时 cdylib 另加 `dynamic-js`）。不要拿这些数字和 README 的 2.4 MB（Skia、fat LTO、strip）直接比。

## 安全

- 组件 bundle 和页面都在 host 子进程里。Shell 的 `--ai-token` 不会继承给页面，除非调用方自己把环境传进去。host 不读 token。
- 本地 HTML 的脚本策略关闭网络。页面不能借这次加载去 fetch。
- ABI 主版本不对就拒绝加载，避免一个旧插件进新运行时后把 DOM 能力用错。
- 这不是沙箱。dylib 和 Shell 一样有进程的全部权限。不可信应用仍要走以后的 namespace / seccomp / Landlock，以及 AI bridge 已有的权限闸门。共享运行时**不**扩大 bridge 的权限。
- 生产网页应继续走 `BrowserPageDomain` 的 worker，只把快照交回 Shell。这次切片为了证明“同一 VM 能跑页面”，在 host 进程里跑解析和 W3VM，没有把页面接上 bridge。

## 镜像

`system/scripts/post-build.sh` 在 `target/shared` 存在时安装：

- `/usr/lib/w3cos/libw3cos_runtime.so`（cdylib，静态链入 std、渲染、W3VM 和编译器；`dynamic-js` 打开）
- `/usr/bin/w3cos-runtime-host`
- `/usr/bin/w3cos` 和 `/usr/bin/w3cos-toolchain`
- 示例 `examples/shared-hello/`（`post-build.sh` 复制整个 `examples/`）
- 可选的 rustc/cargo（`W3COS_INSTALL_RUSTC=1`）

`S99w3cos` 把 `/usr/lib/w3cos` 放进 `LD_LIBRARY_PATH`，并打开桌面的共享启动按钮。Buildroot defconfig 只增加了说明，不改变内核和显示栈。

## 请 owner 决定

1. 静态 UI 用 JSON bundle + C ABI 加载。有行为的应用（信号、自定义闭包）还没有格式。要不要再做一个冻结的 C 插件 ABI，让业务代码也是机器码、但不再把 Taffy/渲染器链进每个应用？切片没有做，因为 `-C prefer-dynamic` 在 crates.io 的 rlib 依赖上链不过。
2. Shell 进程自己要不要改成动态链接同一份 `.so`？能少一份内存，但会把桌面和不可信页面放进同一个链接世界，除非页面仍是子进程。切片保持子进程。
3. 完整 rustc 是进 ISO，还是单独的 SDK？建议 SDK。ISO 只带运行时、host 和 CLI。
4. 共享运行时的默认渲染 feature：这一刀是 `cpu-render` + `dynamic-js`，才能在没有 GPU 的镜像里开窗口并跑网页。桌面发行版可能要 Skia。两套库并列，还是只装一个？
5. 网页是否必须进 `BrowserPageDomain` worker 才算完成，还是 host 进程内的 W3VM 足够作为第一刀？切片是后者，并且故意不在 Shell 进程里跑页面。

## 测量

机器：x86_64，rustc 1.99.0。两边都设了 `CARGO_PROFILE_RELEASE_LTO=off`、`CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16`、`CARGO_PROFILE_RELEASE_PANIC=unwind`。

- 静态应用的生成工程自己写了 `opt-level = "z"` 和 `strip = "symbols"`，feature 只有 `cpu-render`（`W3COS_RUNTIME_FEATURES=cpu-render`），不含 W3VM。
- cdylib 用 workspace 的 `opt-level = 3` 和 `strip`，feature 是 `cpu-render,dynamic-js`，所以里面还有编译器和 W3VM。

不要拿这些数字和 README 的 2.4 MB（Skia、fat LTO、strip）直接比。cdylib 把 std 静态链进去，`ldd` 只有 `libsqlite3`、`libgcc_s`、`libm`、`libc`。静态二进制的 `ldd` 也是这四份，没有 `libw3cos_runtime.so`。

| 产物 | 字节 | 说明 |
|---|---:|---|
| hello-static | 24,071,296 | ELF，静态链入 cpu-render 运行时，不含 W3VM |
| shared-hello.w3app | 12,319 | JSON 组件树，`abi` = 65536，不是 ELF |
| libw3cos_runtime.so | 33,271,520 | 一份 cdylib，含 cpu-render、W3VM 和编译器 |

同一份静态 UI，应用侧从 24 MB 降到 12 KB。多出来的体积在系统那一份 33 MB 里，而且那一份还带了网页要用的 VM。N 个静态应用不再各带一份运行时。

无头页面：`w3cos-runtime-host page --headless examples/shared-hello/page.html` 退出码 0，正文包含 `Hello from W3VM`。原文 “page loading” 被内联脚本替换掉了。

Xvfb `:99`、`W3COS_RENDERER=cpu`、首帧 `W3COS_FRAME_DUMP`：

- `app shared-hello.w3app` 的 1200×800 帧里能数到标题色 `#F0F4F8`、副标题色 `#9FB3C8`、按钮色 `#2BB0ED`。列本身是内容宽高，没有铺满窗口，窗口底色是运行时默认的 `#0B1220`。
- `page page.html` 的首帧画出标题 “Hello from W3VM”（脚本执行之后的文本，不是 “page loading”）。
- `w3cos-shell --shared-app examples/shared-hello/shared-hello.w3app` 画出的首帧与 host 直接打开同一份 bundle 的首帧字节相同。
- `W3COS_SHARED_LAUNCHER=1` 时桌面多出 Shared Hello 和 Web VM 两个按钮。未设置这个变量时桌面不加这两项。
