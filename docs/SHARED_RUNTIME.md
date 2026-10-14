# 共享运行时、VM 与工具链

状态：owner 已拍板。ABI `1.1`（`0x00010001`）。测量数字在文末，来自这次构建。

## 代码里的事实

`w3cos build app.tsx -o app`（`crates/w3cos-cli`）把 TSX 转成一个临时 Cargo 工程，这个工程用 path 依赖 `w3cos-runtime`，再 `cargo build` 出**静态链接**的 `w3cos-app`。每个应用二进制都带一份布局、渲染和窗口代码。普通 AOT 应用**不**链接 `w3cos-compiler` / `w3cos-ir` / `w3cos-vm`（`scripts/check-aot-dependency-boundary.sh`）。

虚拟机是存在的。动态文档走 SWC → W3IR → `crates/w3cos-vm`，由 runtime 的 `dynamic-js` feature 带进来。网页解析在 `html_tree_builder`，脚本执行在 `ScriptLoader`。

`ARCHITECTURE.md` 里的 “no runtime, no VM, no GC” 和 “Vello 为主” 已经过时。README 和 `Cargo.toml` 与代码一致：AOT 是 rustc/LLVM；动态文档是 W3VM；二维渲染主路径是 Skia，Vello 和 tiny-skia 是兼容后端。`cpu-render` 用 tiny-skia + softbuffer，这是无 GPU 的 Xvfb 路径。这一份系统库只用 `cpu-render`，不另装一套 Skia。

## 已拍板

1. **有行为的应用走稳定的 C 插件 ABI。** 插件是自己的 cdylib，不把 `w3cos-runtime` 当 rlib 链进去。它 `#[link(name = "w3cos_runtime")]`，从系统那一份 `libw3cos_runtime.so` 解析符号。点击是 `extern "C" fn(*mut c_void)`，不是跨 cdylib 的 Rust `Fn`。信号是运行时的 `create_signal` / `get` / `set`。必须在 `w3cos_runtime_run_plugin` 之前创建信号；在 build 里创建会每次重绘多一个槽。示例是 `examples/shared-counter`，Shell 桌面的 Counter 按钮和 `w3cos-shell --shared-plugin` 都会把它交给 `w3cos-runtime-host plugin`。
2. **Shell 和插件、网页用同一份 `libw3cos_runtime.so`。** 产品进程是 `w3cos-shell-stub`（安装名 `w3cos-shell`，构建产物 `w3cos-shell-linked`）。它 `dlopen` 这份库再调用 `w3cos_shell_main`。桌面源码仍在 `crates/w3cos-shell/src/main.rs`，由 runtime 的 `desktop` feature 编进 cdylib。静态的 `cargo build -p w3cos-shell` 还在，只作对照和回退。
3. **rustc 在 SDK，不在 ISO。** `system/scripts/package-sdk.sh` 把 `rustc --print sysroot` 收到 SDK 目录。ISO 的编译器是 `/usr/bin/w3cos`。`post-build.sh` 不再复制 rustc，`W3COS_INSTALL_RUSTC` 已去掉。
4. **网页在 `BrowserPageDomain` worker 里跑。** `PageCommand::LoadDocument` 调 `load_html_on_shared_vm`（脚本开、网络关）。worker 启动时和每次加载前都 `capability_gate::reset()`。闸门是线程局部的，父进程里的授权（包括 `file-system`）在 worker 里看不见。worker 发出 `PageEvent::Gate { permission: "file-system", granted: false }`。快照交回 UI 之前会拆掉 `NativeHost` id，避免 UI 线程拿 worker 的节点 id 去查自己的空文档。Host 不读 AI bridge token。
5. **不并列第二套 Skia。** 系统库的 feature 是 `cpu-render,dynamic-js,desktop`。`desktop` 只把桌面和 AI bridge L1 编进同一份 `.so`。

静态 UI 仍然是 JSON bundle（`abi` + `component`），`w3cos_runtime_run_bundle` 反序列化后走 `run_app_static`。ABI `1.0`（`0x00010000`）的旧 bundle 仍能被 `1.1` 的 host 加载。次版本 1 增加的是插件入口，不改变组件 JSON。

没有把 AOT 应用放进 W3VM。W3VM 解释 W3IR，热路径比 LLVM 机器码慢一个数量级以上。有行为的应用是本机插件，网页脚本才进 VM。

也没有用 Rust `prefer-dynamic`。实测过：依赖 crate 只有 rlib，链接报 `required to be available in rlib format, but was not found in this form`。C ABI 不需要冻结 rustc 的 dylib ABI。

产品版本号在 `w3cos-abi`：高 16 位主版本，低 16 位次版本。Host 只在主版本相同、且运行时次版本 ≥ 插件声明的次版本时加载。当前是 `1.1`。

运行时导出：

- `w3cos_runtime_abi_version()`
- `w3cos_runtime_run_bundle()`：静态 UI bundle
- `w3cos_runtime_run_plugin()`：插件的 build 函数，点击后重绘
- `w3cos_plugin_signal_i64` / `w3cos_plugin_signal_get` / `w3cos_plugin_signal_set`
- `w3cos_plugin_text` / `w3cos_plugin_button` / `w3cos_plugin_column`
- `w3cos_plugin_abi_required()`：插件侧声明自己要的 ABI（由插件实现，不是运行时导出）
- `w3cos_runtime_page_text()` / `w3cos_runtime_run_page()`：页面 worker 的快照
- `w3cos_shell_main()`：桌面入口，只在带 `desktop` feature 的库里可用
- `w3cos_runtime_last_error()`

`W3cosPluginStyle` 是 `repr(C)`，40 字节。颜色是 `0xRRGGBBAA`，alpha 为 0 时保留运行时默认色。点击回调表最多 256 项（`u8` code）。

## 和“同一个进程”之间的边界

系统上只有一份运行时库。Shell 进程映射它来画桌面。插件和网页仍由 Shell `spawn` `w3cos-runtime-host`，在子进程里 `dlopen` 同一份文件。页面 worker 在那个子进程里，不在 Shell 进程里。这样页面拿不到 Shell 的 AI token，也继承不到 Shell 线程上的 `capability_gate` 授权。

`w3cos-shell-linked` 的 `ldd` **不**列出 `libw3cos_runtime.so`。它是 `dlopen`，不是 `DT_NEEDED`。stub 可以在库还没编出来时单独构建。进程起来之后，`/proc/PID/maps` 里能看到这一份 `.so`。Host 在重新 exec 之前会把库目录 prepend 到 `LD_LIBRARY_PATH`，因为 glibc 在进程启动时读这个变量。插件自己的 `ldd` 在没设这个路径时会显示 `libw3cos_runtime.so => not found`，由 host 补上。

这份 `.so` 里也包含桌面代码。插件进程映射它时会带上桌面和 VM，换来的是磁盘和链接意义上的一份运行时，而不是每个应用再嵌一份。

## 启动

```text
w3cos-shell --shared-app examples/shared-hello/shared-hello.w3app
w3cos-shell --shared-plugin examples/shared-counter/target/release/libshared_counter.so
w3cos-shell --shared-page examples/shared-hello/page.html
w3cos-shell --shared-page --shared-headless examples/shared-hello/page.html
w3cos-runtime-host plugin examples/shared-counter/target/release/libshared_counter.so
w3cos-runtime-host page --headless examples/shared-hello/page.html
```

桌面在 `W3COS_SHARED_LAUNCHER=1` 时多三个按钮：Shared Hello、Web VM、Counter。镜像的 `S99w3cos` 在 `/usr/lib/w3cos` 存在时会设这个变量。

构建：

```bash
scripts/build-shared-runtime.sh
cargo build --release -p w3cos-runtime-host -p w3cos-shell-stub -p w3cos-cli
# 插件不是 workspace 成员，自己有 [workspace]，避免被根 workspace 吸进去
RUSTFLAGS="-L target/shared -C link-arg=-Wl,-rpath,/usr/lib/w3cos" \
  cargo build --release --manifest-path examples/shared-counter/Cargo.toml
cp examples/shared-counter/target/release/libshared_counter.so target/shared/
```

静态对照（同一渲染 feature，LTO 关闭，用来和动态壳比首帧）：

```bash
CARGO_PROFILE_RELEASE_LTO=off \
CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
CARGO_PROFILE_RELEASE_PANIC=unwind \
  cargo build --release -p w3cos-shell --no-default-features --features cpu-render
```

SDK（不要打进 ISO）：

```bash
system/scripts/package-sdk.sh            # 默认写到 dist/w3cos-sdk
system/scripts/package-sdk.sh /other/dir
```

## 性能

静态 UI bundle 加载后走 `run_app_static`，布局和绘制是运行时里的本机代码。插件的 build 和点击回调也是本机代码，信号更新后由运行时再调 build。W3VM 只跑网页脚本。

这次测量关掉了 fat LTO，`codegen-units=16`，`panic=unwind`。不要拿这些数字和 README 的 2.4 MB（Skia、fat LTO、strip）直接比。cdylib 把 std 静态链进去。

动态壳的首帧和静态壳在同一段时间里。没有变慢。静态壳不含 W3VM；动态壳映射的那份库含 VM、桌面和 AI bridge，所以库比静态壳大，进程文件本身小。见文末数字。

## 安全

- 组件 bundle、C 插件和页面都在 host 子进程里。Shell 的 `--ai-token` 不会继承给页面，除非调用方自己把环境传进去。host 不读 token。
- 页面 worker 的 `capability_gate` 在启动和 `LoadDocument` 时清空。父线程的 `file-system` 授权传不进去。没有给页面补授权的命令。
- 本地 HTML 的脚本策略关闭网络。
- ABI 主版本不对就拒绝加载插件。次版本只允许运行时不低于插件。
- 这不是沙箱。插件和 dylib 拥有进程的权限。不可信应用仍要走以后的 namespace / seccomp / Landlock。共享运行时不扩大 AI bridge 的权限。
- 点击回调最多 256 个。超过则这次 `w3cos_plugin_button` 失败。

## 镜像

`system/scripts/post-build.sh` 安装：

- `/usr/lib/w3cos/libw3cos_runtime.so`
- `/usr/bin/w3cos-runtime-host`
- `/usr/bin/w3cos-shell`（优先 `w3cos-shell-linked`）
- `/usr/bin/w3cos` 和 `/usr/bin/w3cos-toolchain`
- 若 `target/shared/libshared_counter.so` 存在，装到 `/usr/share/w3cos/apps/shared-counter/`

不安装 rustc。`system/rootfs_overlay/usr/lib/w3cos/toolchain-manifest.json` 的 `compiler` 是 `/usr/bin/w3cos`，`rustc` 字段写明 SDK only。ISO workflow 先跑 `scripts/build-shared-runtime.sh`，再编 stub 和 `w3cos-cli`。

## 字体

宿主 UI 字体按名字选中 Noto Sans 或 DejaVu 时，拉丁字形是有的，汉字不在这张 cmap 里。CPU 路径以前对缺字直接栅格化 `.notdef`，所以中文是方框。Skia 路径本来就会 `match_family_style_character`。

现在缺字按这个顺序找脸，并且只有栅格器真能画出墨水才采用：

1. `libfontconfig.so.1` 的 charset 匹配（`dlopen`，镜像里没有这个库就跳过）。
2. 具名回退：汉字/假名/谚文优先 Noto Sans CJK SC、思源黑体、文泉驿、Droid Sans Fallback；emoji 优先轮廓字体，Noto Color Emoji 放在最后；符号优先 Noto Sans Symbols 和 DejaVu。
3. 已安装字体里再扫最多 64 个族。

ISO 带的是 `system/rootfs_overlay/usr/share/fonts/truetype/w3cos/DroidSansFallback.ttf`（3.9 MB，Apache-2.0）。`fonts-noto-cjk` 安装后大约 100 MB，不进 ISO。DevContainer / Codespaces 的 `post-create.sh` 和 README 的 Xvfb 依赖装 `fonts-droid-fallback`，同一张脸。HF 演示镜像继续用 `fonts-noto-cjk`。

Noto Color Emoji 是 CBDT 彩色位图。CPU 栅格器画不出它的轮廓时不会选它，避免再画出空方框。系统里如果没有可栅格的轮廓 emoji，U+1F600 仍然没有字形。符号（例如 ✓）走 DejaVu。

## 测量

机器：x86_64，rustc 1.99。`CARGO_PROFILE_RELEASE_LTO=off`、`CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16`、`CARGO_PROFILE_RELEASE_PANIC=unwind`。workspace 的 `opt-level` 是 3。

Xvfb `:99`，`W3COS_RENDERER=cpu`，`W3COS_FRAME_DUMP` + `W3COS_FRAME_DUMP_EXIT=1`，`W3COS_SHARED_LAUNCHER=1`。时间是进程墙钟（`date +%s%N` 包住整个进程，进程在首帧后退出）。

| 产物 | 字节 | 说明 |
|---|---:|---|
| `w3cos-shell`（静态，改之前的对照） | 29,702,040 | 静态链入 cpu-render 桌面。`ldd` 无 `libw3cos_runtime.so`。不含 W3VM |
| `w3cos-shell-linked`（改之后的进程文件） | 471,648 | `dlopen` 运行时。`ldd` 只有 `libgcc_s` 和 `libc` |
| `libw3cos_runtime.so` | 35,360,024 | 一份库：cpu-render、W3VM、编译器、桌面、AI bridge L1 |
| `libshared_counter.so` | 417,320 | 插件。`NEEDED` `libw3cos_runtime.so`，不含第二份运行时 |
| `shared-hello.w3app` | 12,319 | JSON，`abi` = 65536（1.0），1.1 host 仍接受 |

静态壳首帧：0.327s、0.272s、0.272s、0.269s。

动态壳首帧：0.275s、0.264s、0.261s、0.266s。

同一会话里重测的动态壳没有比静态壳慢。静态壳的第一下 0.327s 是这组里最冷的一次；后面两边都在 0.26–0.27s。动态进程的 `maps` 只有一份 `/workspace/target/release/libw3cos_runtime.so`（四个段，同一个 inode）。

体积上要分开看：进程文件从 29.7 MB 降到 472 KB，但安装足迹是那份 35.4 MB 的库。库比静态壳大约 5.7 MB，因为里面还有 W3VM、编译器和桌面。N 个插件不再各带一份运行时；它们加上这一份库。

Xvfb 帧：

- Counter 插件首帧是 `count 0` 和 Add。按钮色 `#2BB0ED`，标题字色 `#F0F4F8`，窗口底 `#0B1220`。这张图是初始帧，点击加一由单测覆盖，不由这张截图证明。
- `w3cos-shell-linked --shared-plugin` 的首帧与 host 直接打开同一插件的首帧字节相同。
- 页面 worker 开窗后的首帧画出 “Hello from W3VM”（脚本执行之后的文本）。无头 `page --headless` 同样打出这句并退出码 0。
- `W3COS_SHARED_LAUNCHER=1` 的动态桌面首帧包含 Shared Hello、Web VM、Counter。
