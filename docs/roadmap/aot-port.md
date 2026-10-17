# w3cos AOT 移植方案：以 VS Code（Code-OSS）为验收目标

状态：已批准路线图（长期分支 `aot-port`，不进入 `main`）。取代 `w3cos-rn-electron-port-plan.md` 里 "VM 优先" 的路线。M0 的尺子在 `tools/vscode-baseline/` 与 `tools/perf-paired/`。
依据：`wangnaihe/w3cos` main @ `27203b4`；`microsoft/vscode` main 的 `package.json` / `.npmrc`（2026-10-17 抓取）。

## 0. 决策与验收

- **默认路径 = AOT**：TS/JS → 类型化 W3IR → 本地机器码，链接系统里那一份 `libw3cos_runtime.so`（ABI 1.1）。思路借鉴 Static Hermes：有可靠类型的代码走 typed 快路径（无装箱、直接字段偏移），没有类型的代码走 untyped 通用路径，**两者都是编译好的机器码，不解释执行**。
- **Node 兼容运行时库 `w3cos-node`**：在共享 runtime 上实现 `fs/path/net/child_process/worker_threads/crypto/stream/...`、N-API 子集、权限闸门，作为 `libw3cos_runtime.so` 的一部分（或同 ABI 的伴生库），所有应用共用。
- **解释器只做最小兜底**：只给 `eval` / `new Function` / `vm.runInContext` / 运行期才知道路径的 `require` 用。下文第 4 节论证：**对 VS Code 来说它消不掉**（扩展是运行时下载的任意 JS），但可以把它压到"冷路径 + 可度量 + 默认关"。
- **最终验收**：Code-OSS 在 w3cos 工具链下**完整编译并运行**（workbench、编辑器、终端、搜索、Git、扩展宿主、TS 语言服务），并在同一台机器上对比官方 Electron 版 VS Code，**性能达到第 6 节门槛**。

## 1. w3cos 现在有什么（与本方案相关）

| 已有 | 文件 | 用途 |
|---|---|---|
| TSX → Rust → rustc/LLVM 的 AOT | `crates/w3cos-compiler`（`ts_transpiler.rs`、`ts_types.rs`、`scope_analysis.rs`、`esm_codegen.rs`、`esm_resolver.rs`、`esm_lowering.rs`）、`crates/w3cos-cli`（`build/run/dev`） | AOT 主干；已有 TS 类型信息入口和 ESM 解析 |
| W3IR 与 W3IR→本地状态机后端 | `crates/w3cos-ir`、`crates/w3cos-compiler/src/w3ir_lowering.rs`、`w3ir_aot.rs`（注释：产物只依赖 `w3cos-core`，不依赖 VM） | **直接作为新 AOT 管线的中间层**；async/generator 的挂起记录 VM 与 AOT 共用 |
| JS 语义核心 | `crates/w3cos-core`（`value.rs`、`object.rs`、`property_map.rs`、`proxy.rs`、`promise.rs`、`regexp.rs`、`bigint.rs`、`json.rs`、`weak.rs`、`heap.rs`、`module_registry.rs`、`host_modules.rs`） | AOT 与兜底解释器共享同一份语义，避免两套 |
| W3VM 解释器 | `crates/w3cos-vm`（约 3.4k 行） | 降级为兜底层，不再是主执行路径 |
| 共享运行时 + C 插件 ABI | `docs/SHARED_RUNTIME.md`、`crates/w3cos-abi`、`crates/w3cos-runtime-host`、`crates/w3cos-shell-stub`、`examples/shared-counter` | AOT 产物链接对象；"rustc 在 SDK，不在 ISO" 的约束影响第 4 节 |
| AOT 依赖边界检查 | `scripts/check-aot-dependency-boundary.sh` | 扩展为 "VS Code 产物里解释器只出现在 fallback 模块" 的门禁 |
| npm 包映射 | `crates/w3cos-compiler/src/npm_bridge.rs`（codemirror 等的手写映射） | 需要被通用 npm/CommonJS 编译取代，手写映射只留给极少数 |
| Web 平台 | `crates/w3cos-runtime/src/*_web.rs`、`dom.rs`、`canvas2d.rs`、`worker*.rs`、`fetch.rs`、`fs.rs`、`fs_watch.rs`、`process.rs`、`ipc.rs`、`menu.rs`、`dialog.rs`；清单 `WEB_API_CAPABILITIES.md`；`w3cos web-api-audit` | workbench 渲染进程依赖的 DOM/CSS/Canvas/Worker |
| VS Code 差距分析 | `docs/vscode-compat.md`、`docs/monaco-gap-report.md`、`docs/indexeddb-conformance.md` | 起点；但其中 "JSON/RegExp/URL/Intl/dynamic import: None" 等条目需按当前代码复核 |
| 系统能力与权限 | `docs/W3COS_EXTENSIONS.md`（`navigator.w3cos.process/ipc/tray/shortcuts/keyring/...`）、`runtime/src/capability_gate.rs`、`crates/w3cos-ai-bridge/src/{permissions,sandbox,receipt_ledger,tool_host}.rs` | Node/Electron API 的权限闸门与 L0/L1/L2 |
| 性能基建 | `docs/perf/native-vs-browser.md`、`tools/perf-native-browser`、`tools/perf-paired`、`tools/size-audit`、`docs/performance-validation.md` | 第 6 节基准直接在这套工具上扩展 |

**必须正视的现状**：`w3cos-core` 的堆值是 `Rc<RefCell<…>>`（`value.rs` 第 90–92 行；`heap.rs` 写明 "Core heap values use `Rc`"），没有循环回收。**已拍板：不引入追踪式 GC，保持引用计数，循环一律用 WeakRef 断开**。VS Code 到处是潜在循环（Disposable、Emitter、DI 容器、DOM 监听器闭包），所以 M1 的重点是：确定性引用计数语义、WeakRef/FinalizationRegistry、仅调试构建的环检测器、泄漏 CI，以及维护 VS Code 的 WeakRef 补丁队列。

## 2. VS Code 的真实需求（已核对）

### 2.1 构建与进程结构
- `package.json`：`main = ./out/main.js`；`.npmrc`：`runtime=electron`、`target=43.7.7`、`build_from_source=true`（原生模块针对 Electron 43 头文件编译）。
- 进程：**main**（Electron + Node）、**renderer / workbench**（Chromium，DOM）、**shared process**、**extension host**（`utilityProcess`，Node，跑任意扩展）、**pty host**、**file watcher**（`@parcel/watcher` 在 utility process）、大量 **Web Worker**（Monaco 语言 worker、TextMate tokenization、diff、搜索结果处理等）。
- 进程间：Electron IPC + `MessagePort`；VS Code 自己的 IPC 层 `src/vs/base/parts/ipc`（channel/protocol 抽象，底层可换）。

### 2.2 Electron API（主要用到的）
`app`、`BrowserWindow`、`webContents`（含 `ipc`、`session`、devtools）、`ipcMain`/`ipcRenderer`、`contextBridge`（`src/vs/base/parts/sandbox/electron-browser/preload.ts`）、`utilityProcess`、`MessageChannelMain`、`Menu`/`MenuItem`、`dialog`、`shell`、`clipboard`、`nativeTheme`、`screen`、`powerMonitor`、`protocol`（`vscode-file://` 自定义协议）、`session`/`net`、`crashReporter`、`systemPreferences`、`Tray`（少量）、`autoUpdater`（Code-OSS 基本不用）、`safeStorage`（Secret Storage）。

### 2.3 Node 内建模块
`fs`/`fs/promises`（重度）、`path`、`os`、`child_process`（spawn/fork/exec）、`net`（命名管道/UDS：CLI ↔ 主进程、扩展宿主协议）、`http`/`https`/`tls`（代理、市场、`undici`）、`crypto`（哈希、随机、`createHash`）、`stream`、`events`、`zlib`、`url`、`util`、`worker_threads`、`readline`、`string_decoder`、`buffer`、`perf_hooks`、`v8`（heap 统计，可伪造）、`vm`（扩展宿主少量）、`module`（扩展宿主拦截 `require('vscode')` —— **关键**）、`process`（env、argv、`process.on('uncaughtException')`）。

### 2.4 原生模块（来自 `package.json` dependencies）
| 模块 | 作用 | w3cos 方案 |
|---|---|---|
| `node-pty` | 终端 pty | **Rust 重写**（`portable-pty`/`nix::pty`），暴露同 API；不走 N-API |
| `@vscode/sqlite3` | 全局/工作区状态存储 | Rust `rusqlite` 实现同 API；或经 N-API 子集直接编 |
| `@vscode/spdlog` | 日志 | Rust 实现（`tracing`/文件滚动），同 API |
| `@vscode/native-watchdog` | 扩展宿主卡死看门狗 | Rust 线程看门狗 |
| `@parcel/watcher` | 文件监听 | 落到 `runtime/src/fs_watch.rs`（inotify/fanotify），同 API |
| `@vscode/policy-watcher` | 企业策略 | Linux 下读 JSON 策略文件，简化实现 |
| `native-keymap` | 键盘布局 | 基于 xkbcommon / winit 键盘布局 |
| `native-is-elevated`、`@vscode/deviceid`、`@vscode/fs-copyfile`、`@vscode/sudo-prompt`、`kerberos` | 杂项 | 小 Rust 实现或 stub（kerberos 可延后） |
| `@vscode/windows-*` | 仅 Windows | 不需要 |
| `@vscode/ripgrep-universal` | 搜索（独立二进制） | **原样带 `rg` 二进制**，通过 `child_process` 拉起（沙箱内）；不是 JS 问题 |
| `@vscode/tree-sitter-wasm`、`vscode-oniguruma`（WASM） | 语法/正则 | 需要 **WebAssembly 运行时**（wasmtime/wasmer AOT，或 Cranelift）——这是硬需求 |
| `keytar`（旧）→ 现为 Electron `safeStorage` + 系统 keyring | Secret | `navigator.w3cos.keyring` |

结论：VS Code 核心依赖的原生模块数量有限且都可 **Rust 重写成同名包**（`@w3cos/node-pty` 等，通过 `w3cos.app.json` 的 alias 替换），**不必为 VS Code 本体实现完整 N-API**。N-API 子集是给扩展和第三方用的。

### 2.5 扩展宿主 = 动态代码的现实
- 扩展是从 Open VSX / 市场**运行时下载**的任意 JS（经 webpack/esbuild 打包的 CommonJS 为主），在扩展宿主里 `require` 加载；`require('vscode')` 由宿主拦截注入 API 对象。
- 扩展里常见：`new Function`（模板库、ajv 等 schema 编译器）、`eval`（少数）、动态 `require(variable)`、`vm`、`module._load` hack、带原生 `.node` 的扩展（少数，如部分 C++/Python 工具）。
- 内置扩展（`extensions/` 目录：git、typescript-language-features、json、markdown、emmet、npm 等）**构建时已知，可 AOT**。
- **TypeScript 语言服务**：`typescript-language-features` 扩展在单独的 tsserver 进程里跑 `typescript/lib/tsserver.js`（约 9MB+，单态性差、大量多态对象，这是 AOT 性能的硬骨头），新版 VS Code 也在接入 TS 7 原生（Go 实现的 tsgo），后者是独立二进制，反而简单。

### 2.6 Web Worker 与 Monaco
- Monaco 的 editor worker、语言 worker（JSON/CSS/HTML/TS）、TextMate tokenization（`vscode-textmate` + `vscode-oniguruma` WASM）、`@xterm/xterm`（含 `addon-webgl`：需要 WebGL2 或回退 DOM/canvas 渲染器）。
- Worker 脚本也是构建期已知 → 全部 AOT，作为 `runtime/src/worker.rs` 的线程入口。

## 3. AOT 编译工具链设计

### 3.1 管线
```
TS/JS（ESM + CommonJS） ─SWC parse─▶ 类型收集（tsc Program / .d.ts / JSDoc / 运行期 profile）
   ─▶ W3IR（带类型注解：typed / untyped 两态） ─▶ 优化（内联、逃逸分析、shape 推断、单态化）
   ─▶ 后端 A：Rust 源 → rustc/LLVM（发布构建，最快代码，SDK 内）
   ─▶ 后端 B：Cranelift 直出机器码（设备上安装期编译，见 §4，ISO 内可用）
   ─▶ 链接 libw3cos_runtime.so（core 语义 + 引用计数堆 + DOM + w3cos-node）
```
- 扩展现有 `w3ir_lowering.rs` / `w3ir_aot.rs`，新增 typed IR 节点；`scope_analysis.rs` 做闭包捕获分析。
- **Static Hermes 式 typed 编译**：只有能**证明**的类型才走快路径（TS 类型不可信，`any`/类型断言会撒谎）。做法：类型作为"推测"，在函数入口/字段读处生成廉价 shape 检查，失败则跳到同一函数的 untyped 版本（**编译期产生的双版本**，不是 JIT 反优化）。
- **隐藏类 / shape**：对象布局用 shape + 内联缓存表（编译期静态 IC 槽，运行期填充），`property_map.rs` 增加 shape 过渡树。这是 untyped 代码性能的关键。
- **PGO**：用 VS Code 自动化测试（`test/smoke`、`test/integration`）跑出 profile，喂给 shape 推断与 LLVM PGO（`docs/perf` 已有 "max" 档用 PGO）。
- **CommonJS**：编译期把 `require('literal')` 静态解析成模块链接；`module.exports` 改写为模块记录。webpack/esbuild 产物里的 `__webpack_require__` 运行期表同样可静态化（模块 id 是字面量）。

### 3.2 内存模型：引用计数 + WeakRef（不做追踪式 GC）
- **确定性引用计数**：`Value` 保持 `Rc`；对象在最后一个强引用消失时**立即**释放，没有 GC 停顿，打字延迟不受回收影响。AOT 代码生成负责精确的 inc/dec（逃逸分析消除冗余计数，局部值借用而不计数）。
- **环一律用 WeakRef 断开**：实现标准 `WeakRef`、`FinalizationRegistry`、`WeakMap`/`WeakSet`（基于 `std::rc::Weak`，`w3cos-core/src/weak.rs` 已有基础）。约定：子→父、监听器→发射器、缓存→值、DOM 节点→JS 包装对象 这类"反向边"一律弱引用。
- **DOM ↔ JS 边界**：节点持有监听器为强引用，监听器闭包对节点/宿主对象的捕获由编译器 lint 提示改为弱引用；节点从文档移除且无 JS 强引用时立即释放。
- **调试构建的环检测器**（`--features leak-detector`，release 不编入、零开销）：周期性做试探性删除（trial deletion）扫描，发现不可达环时报告**完整的保留引用链**（源码位置：哪个闭包/字段/监听器持有），输出 JSON 供 CI 和 IDE 跳转。
- **lint/codemod `w3cos leak-lint`**：基于 SWC 静态分析标记常见隐式环：闭包捕获 `this` 后注册到长生命周期对象、`addEventListener`/`onDidX` 无对应移除或未进入 `DisposableStore`、父↔子双向强引用、`Disposable` 未 dispose/未注册、`Map` 缓存以对象为值无淘汰。可给出 codemod（改为 `WeakRef`/`WeakMap`、补 `dispose`）。
- **上游泄漏处理**：VS Code、扩展、npm 依赖里发现的环，fork 后用 WeakRef/显式 dispose 修复，进补丁队列，能上游的提 PR 上游（见 M1）。
- 代价要说清：引用计数在多态热路径上有计数开销；`FinalizationRegistry` 回调时机在引用计数下是确定的（比规范更早），需在兼容报告中注明。已实现：目标的最后一个强引用释放后，在下一个 job checkpoint（微任务清空后）交付，顺序见 `weak.rs` 模块文档。
- 注意：page arena 里的普通 AOT 值（对象、数组、闭包）现在也是严格引用计数，最后一个强引用消失即释放（M1 item 4a）；编译器证明不逃逸的字面量直接是引用计数的局部值。只有引用环要等页面 reset，或者用 WeakRef 断开。

### 3.3 WebAssembly
VS Code 依赖 tree-sitter-wasm、oniguruma WASM，扩展里也大量用 WASM。接入 `wasmtime`（Cranelift，AOT 预编译 `.cwasm` 缓存）作为 `WebAssembly.*` 的实现，与 §3.1 后端 B 共享 Cranelift。

## 4. 解释器能不能消掉？（论证）

| 动态来源 | 能否 AOT | 处理 |
|---|---|---|
| VS Code 本体 + 内置扩展 + worker | ✅ 构建期全量已知 | 发布构建 rustc/LLVM AOT |
| 用户安装的扩展（运行时下载） | ✅ **安装期 AOT** | 安装扩展时在设备上用后端 B（Cranelift）编译，产物按内容哈希缓存、签名记录回执；首次激活前完成。ISO 内没有 rustc，所以设备侧必须是 Cranelift |
| `require(变量)` | 🟡 多数可解 | 编译期把扩展包内所有 `.js` 都编进模块表，运行期按路径查表；表外路径才落兜底 |
| `new Function(src)` / `eval` / `vm.*` | ❌ 源码运行期才生成 | **两档**：①"运行期 Cranelift 编译"（相当于 baseline 编译，几毫秒级，结果缓存）；②极小解释器（复用 `w3cos-vm`）只用于一次性小片段/编译预算超限 |
| `Function.prototype.toString` 依赖源码 | ✅ | 保留源文本 |

**结论**：在"安装期 AOT + 运行期 Cranelift 即时编译生成代码"的前提下，**解释器在功能上可以消掉**——所有代码最终都是机器码；`w3cos-vm` 只作为调试/兜底保留（`W3COS_INTERP=1` 或 Cranelift 失败时），并要求在基准中**解释执行字节数占比 < 0.1%**，否则视为未达标。严格意义上"运行期把源码编成机器码"已经是一种 JIT（无推测、无反优化），这点要对外讲清楚，不宣称"零 JIT"。安全上：运行期代码页 W^X 切换，只对扩展宿主进程开放，并受 sandbox（unshare→Landlock，`ai-bridge/src/sandbox.rs`）约束。

## 5. Node 兼容运行时 `w3cos-node`

- 位置：`crates/w3cos-node`（新），编进 `libw3cos_runtime.so`，导出给 AOT 产物；JS 侧胶水本身也 AOT 编译。
- 事件循环：基于 tokio/mio 的单线程 loop，语义对齐 libuv 阶段（timers → pending → poll → check → close）与 `process.nextTick` / 微任务顺序。**这是 Node 兼容最容易出错的地方**，用 Node 官方测试 `test/parallel` 子集门禁。
- 模块实现顺序（按 VS Code 实际用到排）：
  1. `path`、`events`、`util`、`buffer`、`string_decoder`、`url`、`querystring`、`assert`、`os`、`process`、`timers`
  2. `fs`/`fs/promises`（落 `runtime/src/fs.rs`，同步 API 必须真同步）、`stream`（含 Web Streams 互转）、`zlib`（`flate2`）、`crypto`（`ring`/RustCrypto + WebCrypto）
  3. `child_process`（spawn/fork/exec，stdio pipe，`fork` 的 IPC 通道 = 新的 AOT 进程）、`net`（TCP + UDS）、`tls`（rustls）、`http`/`https`（hyper；`undici` 用其纯 JS 实现跑在 `net` 上）、`dns`
  4. `worker_threads`（复用 `runtime/src/worker.rs` 线程模型 + `MessagePort`）、`readline`、`perf_hooks`、`v8`（统计伪造）、`vm`（走 §4 动态编译）、`module`（`Module._load`/`_resolveFilename` 钩子，扩展宿主需要）
- 参考实现：**unenv**（UnJS 的 Node 内建 polyfill/预设，纯 JS 部分可直接借用）、**Deno `ext/node`**（MIT，Rust + JS，2.8 版宣称通过 Node 测试套件 75%+）、**Bun**（Node-API：js-native-api 100%、node-api 47.7%，合计 76.3%）——作为覆盖率对照的标杆。
- **N-API 子集**：先做 engine 无关的 `js-native-api`（值、对象、函数、异常、引用、ArrayBuffer、`napi_wrap`），对照 Bun 的测试清单；`node-api` 部分只做 `napi_create_async_work`、threadsafe function、`napi_get_uv_event_loop`（返回兼容句柄）。原生 `.node` 默认**进程外**加载（崩溃隔离、沙箱），可配置进程内。
- **权限闸门**：每个 Node 入口调用 `capability_gate`：
  - 应用自身目录读写、工作区目录（用户打开文件夹即授权）→ L0
  - 工作区外路径、`child_process`、监听端口、网络出站到新域名 → permission sheet / L1（propose→confirm→execute→receipt）
  - 扩展宿主**按扩展粒度**授权（扩展 manifest 声明 + 首次使用确认），回执记在 `receipt_ledger.rs`
  - 注意：VS Code 本体需要宽权限（任意打开文件、跑终端）；对**本体**按"用户打开 = 授权"处理，避免每步弹窗，把精细权限集中在扩展上

## 6. Electron 兼容层（AOT 版）

- `@w3cos/electron`（构建期 alias 替换 `electron` 模块，全部 AOT 编译；不在全局暴露 `electron.*`，遵守 `docs/W3COS_EXTENSIONS.md`）。
- 进程映射：main → AOT 进程（w3cos-node）；renderer → AOT 编译的 workbench 与 DOM 同进程/同线程运行（不再有 Chromium，多窗口 = 多 runtime 实例）；`utilityProcess` / `child_process.fork` → 新 AOT 进程；preload/`contextBridge` → 编译期确定的 API 投射（结构化克隆边界保留，安全语义不降）。
- IPC：`ipcMain/ipcRenderer`、`MessageChannelMain`、`MessagePort` → `runtime/src/ipc.rs`（UDS JSON 总线）升级为二进制帧 + 可转移 `ArrayBuffer`；VS Code 的 `vs/base/parts/ipc` 只需要底层 protocol 适配。
- 其余映射沿用 `docs/vscode-compat.md` 第 2.3 节表格：`w3cos.window/dialog/ipc/menu` 已做；`tray`、`shell.openExternal`、`showItemInFolder` 待做；`protocol`（`vscode-file://`）→ `w3cos://` 资源协议；`safeStorage` → keyring；`autoUpdater` → w3cos 签名包管理（did:key）；`crashReporter` → 本地日志。

## 7. 性能方法论与门槛

### 7.1 环境
- 同一台机器、两套系统盘或同一 Linux：A = 官方 Code-OSS（同 commit、Electron 43.7.7 官方构建）；B = 同 commit 经 w3cos 工具链编译的 Code-OSS。
- 两档硬件：x86_64 桌面（如 8C/16G，有 GPU）+ ARM64 低配（如 RK3588 8G）——后者体现"自有硬件"定位。
- CPU 频率锁定、关闭 turbo、`--disable-extensions` 与"预装 10 个常用扩展"两组配置；每项 ≥ 20 次，报告中位数与 p95，冷启动前 `drop_caches`。
- 采集：`tools/perf-paired` 扩展为 VS Code 驱动器；用 VS Code 自带 `--prof-startup` / `Developer: Startup Performance` 打点、`/proc/<pid>/smaps_rollup`（PSS/RSS，**所有子进程求和**）、输入延迟用 evdev 注入 + 屏幕像素探测（或 compositor 呈现时间戳），不信任应用自报。

### 7.2 指标与通过门槛（B 相对 A）
| 指标 | 定义 | 门槛 |
|---|---|---|
| 冷启动到窗口可见 | exec → 首帧呈现 | ≤ 0.6× |
| 冷启动到 workbench 就绪 | exec → `didStartWorkbench`/可输入 | ≤ 0.7× |
| 打开文件 | 快速打开选择 → 文本首帧（1 MB TS 文件） | ≤ 0.8× |
| 大文件打字延迟 | 50 MB 文件、开启语法高亮，按键 → 像素，p50 / p95 | p50 ≤ 1.0×，p95 ≤ 0.9× 且绝对值 < 16 ms |
| 滚动 | 大文件连续滚动丢帧率 | ≤ A |
| 全局搜索 | Linux 内核源码树字符串搜索到结果全部渲染（rg 相同，比的是管线） | ≤ 1.0× |
| 内存 | 打开中型仓库 + 5 个编辑器，稳定后所有进程 PSS 之和 | ≤ 0.5× |
| 长时间稳定性 | 8 小时脚本化编辑（含开关文件、终端、扩展激活）后 PSS 增长 | ≤ 5% 且 ≤ A 的增长量 |
| 场景堆差分 | 每个场景（开/关编辑器、开/关面板、开/关终端、重载窗口）执行 N 次前后堆快照对比 | 残留对象数 = 0（允许已登记的缓存白名单） |
| 回收停顿 | 引用计数释放大对象图的最长单次耗时 | p99 < 2 ms（大图释放拆为延迟批量释放） |
| 环检测器 | 调试构建跑全套 smoke 测试 | 新增未登记环 = 0 |
| 扩展宿主激活 | 宿主启动 → 10 个扩展 `activate` 完成（首次=含安装期 AOT 已完成） | ≤ 1.2×（允许略差，动态代码现实） |
| TS 语言服务 | 打开 vscode 仓库自身，首次诊断完成时间；补全 p95 | ≤ 1.3× / ≤ 1.2×（tsserver 是最难项；若改用 tsgo 原生则 ≤ 1.0×） |
| 磁盘体积 | 安装包（不含共享 runtime） | ≤ 0.3× |
| 解释执行占比 | fallback 执行的字节码/总执行 | < 0.1% |

"真正原生"的判定：上表全部达标，且 `perf record` 显示热点在 AOT 机器码与 runtime，而非解释器分发循环。

## 8. 里程碑与 PR 切片（VS Code 全程 dogfood）

每个切片是一个能独立合并、CI 能验证的 PR。中间目标：hello Electron → Code-OSS workbench 启动 → 编辑器可用 → 终端 → 扩展。

### M0 基线与度量（先把尺子做好）
1. CI 里固定构建官方 Code-OSS（锁 commit），产出基线二进制与依赖清单。
2. `w3cos port analyze vscode/`：列出 Electron/Node/原生模块/动态代码点（`eval`、`new Function`、动态 `require`），输出 JSON 报告，作为后续切片的"待清零清单"。
3. `tools/perf-paired` 增加 VS Code 驱动：冷启动、打开文件、打字延迟、内存的 A 侧基线。

### M1 编译器与内存模型地基（引用计数 + WeakRef）
4. 确定性引用计数语义：明确释放时机规范、AOT 冗余 inc/dec 消除、大对象图延迟批量释放（避免长停顿）。
   - **现状更正（M1 第一个 PR 核实）**：当时普通 AOT 对象字面量、数组和闭包（`Value::object`/`Value::array`/`Value::function`）分配在 page arena 里，句柄是 `Copy` 的 `u32`，不计数，页面 reset 前不会释放；只有宿主 `Rc` 值是真正的引用计数。**4a（M1 最大的一块，已完成）**：让 arena 值也能回收，否则 VS Code 长跑内存只增不减。分两部分：
     - **第 1 部分（已完成）**：`crates/w3cos-compiler/src/alloc_escape.rs` 在 W3IR 上做函数内逃逸分析（基于合一的指向分析，覆盖寄存器和局部绑定，并跟踪"对象里装了什么"）。能证明不逃逸的对象/数组字面量，AOT 生成 `Value::local_object` / `Value::local_array`，即引用计数的宿主值，最后一个引用消失就释放，最晚在函数返回时。逃逸条件全部从严：返回、抛出、作为任何调用/构造的参数或接收者、被闭包捕获、存进参数/外层/全局绑定、`await`/`yield` 跨挂起点、可能触发用户代码的强制转换、计算键、访问字面量形状之外的键（避免原型链上的 getter/setter 拿到 `this`）、自引用环、出现 `eval`。即使误判，宿主 `Rc` 值放在哪里都合法，只影响回收时机，不影响正确性。实测（probe：`work(10000)` 调 5 / 10 轮）：arena 存活对象从 102809 / 202809 降到 2809（与空跑一致），slab 从 31.4 MB / 61.8 MB 降到 0.98 MB，RSS 从 60.1 MB / 103.9 MB 降到 16.1 MB / 16.2 MB。逃逸的 `workEscape` 不变，对象照常存活。
     - **第 2 部分（已完成）**：arena 槽位严格引用计数（对象、数组、闭包含 `aot_function`，即 Immediate 标签 6/7/8）。
       - `Value` 改为手写 `Clone`/`Drop`：clone/存入时槽位计数 +1，drop/覆盖时 −1；`JsObjectRef`/`JsArrayRef`/`JsFunction` 的 interned 句柄同样持有一个强计数。
       - 计数归零：槽位立刻对句柄和弱引用失效（generation +1），payload 进线程局部的 LIFO 待释放队列，在不持有 arena 借用的情况下原地 drop；子值的释放只入队，不递归（20 万层链表在 2 MiB 测试栈上释放）。drop 完的槽位进 free list 复用。
       - 句柄 = 24 位槽号 + 8 位 generation 低字节；弱引用保存完整 32 位 generation，所以槽位复用 256 次以后也不会升级到新占用者。`identity()` / `object_identity_bits()` 改用"槽种类 + epoch + generation + 槽号"，不再用槽地址，按身份做键的旁路表（事件槽、IDB/结构化克隆的图编码、JSON 环检测等）不会因槽位复用而串号。
       - slab 句柄空间用满时回退成宿主 `Rc`；线程退出、arena 被借用等无法安全释放的情况一律泄漏到 reset，不冒释放仍被引用的槽位的风险。`page_arena::reset()` 仍是批量释放：先推进 epoch（旧句柄的 inc/dec 变成空操作），再在 arena 借用之外 drop 旧 slab；如果 reset 发生在某个 payload 的 drop 过程中，旧 slab 先暂存，等这一轮释放结束再 drop。
       - 接入 #112 的弱身份 + 死亡队列：arena payload drop 时 `HeapAllocation` 照常记死亡，`WeakRef.deref()` 返回 `undefined`，`FinalizationRegistry`/`WeakMap`/`WeakSet` 在下一个 checkpoint 生效。弱引用不持有计数。
       - 编译器配合：`unique_back_edges` 弱化的捕获只属于"挂到目标上的那个闭包"。嵌套在它里面的闭包如果继承了弱 getter，现在改为在创建时强持有目标（`weak::strong_capture_target`），否则像 IDB `request.onsuccess` 里再挂 `put.onsuccess` 这种写法，`request` 会在仍可达时被释放（以前 arena 值不释放，这个问题被掩盖了）。
       - 环仍然泄漏到页面 reset，除非用 WeakRef 断开（自动弱化覆盖的三种回边除外）。
       - 实测（box x86_64，release；probe 每轮跑 `workEscapeDrop`（工厂函数返回的对象，逃逸）+ `workClosures`（每次迭代新建闭包）+ `workBatch`（局部数组装 1 万个对象），各 10000 次）：

         | | arena 存活对象 | 存活函数 | slab | RSS |
         |---|---|---|---|---|
         | 空跑 前 → 后 | 2,812 → 1,911 | 1,813 → 1,788 | 0.98 MB → 0.71 MB | 16.0 MB → 15.3 MB |
         | 5 轮 前 → 后 | 302,826 → 1,915 | 101,827 → 1,792 | 98.6 MB → 3.75 MB | 187.4 MB → 20.6 MB |
         | 10 轮 前 → 后 | 602,836 → 1,915 | 201,837 → 1,792 | 196.2 MB → 3.75 MB | 358.2 MB → 20.6 MB |
         | 全局数组保留 1000 个 | 增量 +2,000 → +2,000（仍存活） | | | |

         微基准（`cargo run --release -p w3cos-core --example arena_rc_bench`，n = 100 万，3 次取中位）：

         | | 前 | 后 |
         |---|---|---|
         | 逃逸对象分配后立即丢弃 | 1050 ns/次，丢弃后仍存活 1,000,000 个，slab 304 MB | 794 ns/次，存活 0，slab 3.9 KB |
         | `get_property` | 22.1 ns | 22.8 ns |
         | `set_property` | 1530 ns | 1311 ns（噪声范围） |
         | 句柄 clone + drop | 2.2 ns | 6.9 ns（每次 clone/drop 多一次线程局部的槽位计数） |
         | 10 万个对象装进数组再丢弃 | 798 ns/个，丢弃后存活 100,000 | 944 ns/个，存活 0 |
       - 还没做的：AOT 冗余 inc/dec 消除（item 4）、句柄 clone 的计数开销优化、标量替换和跨过程逃逸分析、环检测器（item 6）。
5. ~~`WeakRef` / `FinalizationRegistry` / `WeakMap` / `WeakSet` 完整实现~~ **已完成（M1 PR 1）**：
   - 原有基础：`w3cos-core/src/weak.rs` 早已基于 `Rc`/`Weak` 实现四个类，编译器 `unique_back_edges.rs` 会把三类常见闭包回边（自挂载、Promise 回调捕获自身、集合迭代器）自动改写成 `WeakRef`。
   - **确定性 finalization**：每个堆分配的 `HeapAllocation` 带一个 48 位、永不复用的弱身份 id（塞进已有的对齐空洞，对象大小不变）。被弱引用过的对象在 `Drop` 时只把 id 记进死亡队列（不在 `Drop` 里跑 JS、不重入）；下一个 job checkpoint（`drain_microtasks` 清空微任务后）统一处理并按"死亡顺序 → 同一目标按注册顺序"调用 `FinalizationRegistry` 回调，每个回调独立 panic 边界，回调里排的微任务在下一批回调前清空。`cleanupSome()` 保留，可同步交付本 registry 的待处理项；`unregister()` 同时取消已死但未交付的项；registry 自身死亡后不再回调。`FinalizationRegistry.cleanupMode` 现在是 `"automatic"`。
   - **O(1) `WeakMap`/`WeakSet`**：以弱身份 id 为键的哈希表，无线性扫描；键死亡时通过反向索引立即删除条目并释放值（O(持有者数)）。集合实例自己死亡时（包括 page reset）释放整张表——修掉了旧实现里 `WeakRef`/`WeakMap` 状态表永不释放的泄漏（AOT 每个弱化捕获都会建一个 `WeakRef`）。
   - 基准（`cargo run --release -p w3cos-core --example weakmap_bench`，100k 个宿主 `Rc` 键，x86_64 Linux，release）：
     | | set ×100k | get ×100k |
     |---|---|---|
     | 旧（`Vec` 线性查找 + 每次 retain） | 420.3 s（4.2 ms/次） | 447.9 s（4.5 ms/次） |
     | 新（id 哈希表） | 40.6–52.2 ms（≈0.4–0.5 µs/次） | 22.4–25.3 ms（≈0.23 µs/次） |
     旧实现是 O(n²)：10k 键时 set 2.6 s、30k 键时 25.5 s。
   - test262 相关用例并入 item 14 的门禁。
6. 调试构建环检测器（trial deletion），报告保留引用链与源码位置；release 构建不编入（CI 检查二进制符号）。
7. 泄漏 CI：每个场景堆差分 + 每晚 8 小时 soak，结果入 `docs/perf/`。
8. `w3cos leak-lint` + codemod：闭包捕获 `this`、未移除监听器、父↔子强引用、Disposable 模式。
9. VS Code fork / 补丁队列流程：`vscode-w3cos` 仓库按上游 tag 重放补丁（quilt 式 `patches/` 目录 + CI 自动 rebase），每个补丁标注"可上游/仅 w3cos"，可上游的向 microsoft/vscode 提 PR；扩展与 npm 依赖同一流程（`patches/npm/<pkg>@<ver>`，安装时应用）。
10. Shape/隐藏类 + 静态 IC 槽。
11. W3IR typed 节点 + "推测类型 + 双版本"降级。
12. CommonJS 静态链接 + Node 解析算法（`exports` 条件含 `w3cos`/`node`）。
13. Cranelift 后端 B（W3IR → 机器码），与 rustc 后端结果对拍（test262 子集）。
14. test262 门禁（目标先 80%，VS Code 用到的特性优先），与 Porffor 等对照公开结果。

### M2 hello Electron
15. `w3cos-node` 第一批：path/events/util/buffer/url/os/process/timers + 事件循环。
16. `fs`/`stream`/`crypto`/`zlib`。
17. `@w3cos/electron`：app、BrowserWindow、ipcMain/ipcRenderer、contextBridge/preload、Menu、dialog。
18. electron-quick-start + electron-api-demos 子集 AOT 编译运行，进 CI（截图为不透明 PNG）。

### M3 Code-OSS workbench 启动
19. `child_process`、`net`（UDS）、`worker_threads`、`MessageChannelMain`/`utilityProcess`。
20. `@w3cos/sqlite3`、`@w3cos/spdlog`、`@w3cos/watcher`（parcel API → `fs_watch.rs`）、`native-keymap`、杂项 stub。
21. `vscode-file://` 协议、`nativeTheme`、`screen`、`safeStorage`。
22. CSS/DOM 缺口清零（按 `docs/vscode-compat.md` 2.1 与 `docs/monaco-gap-report.md`：adoptedStyleSheets、Drag and Drop、scrollTo 等）。
23. **里程碑验收**：Code-OSS（`--disable-extensions`）AOT 编译并显示 workbench，记录启动与内存首组数据。

### M4 编辑器可用
24. Monaco 编辑器与 worker（editor worker、TextMate tokenization）AOT；`WebAssembly`（wasmtime）接入 oniguruma / tree-sitter-wasm。
25. IME/composition、选择、剪贴板与 w3cos IME 打通。
26. 大文件性能专项：piece tree 热路径 typed 化、渲染批处理；打字延迟达门槛。
27. 搜索：`rg` 子进程（沙箱内）+ 结果视图；文件树、快速打开。

### M5 终端与 Git
28. `@w3cos/node-pty`（Rust pty）+ pty host 进程。
29. xterm.js：DOM/canvas 渲染器先行，WebGL2 后续（`addon-webgl`）。
30. 内置 git 扩展（AOT，子进程 git）。

### M6 扩展
31. 扩展宿主进程：`require('vscode')` 注入、`module` 钩子、扩展 manifest 权限声明 → capability gate + 回执。
32. 内置扩展全部 AOT（json、markdown、emmet、npm、typescript-language-features）。
33. 安装期 AOT：Open VSX 下载 → Cranelift 编译 → 内容哈希缓存 → 回执。
34. 运行期 `new Function`/`eval`/`vm` → Cranelift 即时编译；`w3cos-vm` 兜底 + 解释执行占比统计。
35. N-API 子集（js-native-api 全量 + 线程安全函数），带 `.node` 的扩展进程外加载。
36. tsserver 专项：PGO、shape 稳定化；评估直接调用 tsgo 原生二进制。
37. 前 50 个 Open VSX 扩展兼容矩阵（CI 自动生成）。

### M7 验收
38. 全部第 7 节基准在两档硬件跑完，公开报告（`docs/perf/vscode-aot.md`）。
39. 8 小时稳定性 + 内存增长测试。
40. 发布：签名包（did:key），共享 runtime 版本绑定。
41. 附加：VS Code 命令自动暴露为 MCP 工具（命令注册表天然是工具清单），走 propose→confirm→execute→receipt。

## 9. 最难的风险（诚实评估）

1. **只靠引用计数 + WeakRef 的泄漏风险**：任何没被发现的环都是永久泄漏。VS Code 与依赖量大，补丁队列可能长期维护几十到上百个补丁，跟上游每月发版有持续成本；运行时下载的第三方扩展无法预先修补，只能靠 lint 报告、按扩展的内存配额与"重启扩展宿主"兜底。WeakRef 改错会导致对象被提前释放（比泄漏更难查），需要调试构建检测器与 soak 测试兜住。
2. **TS 类型不可信**：VS Code 代码虽然是严格 TS，但 `any`、类型断言、`as unknown as`、声明合并都有；第三方依赖多是无类型 JS。typed 快路径的实际覆盖率可能只有 50–70%，其余靠 shape/IC 的 untyped 路径；**不加推测与 IC，性能会明显落后 V8 的 JIT**。
3. **V8 的 JIT 是很强的对手**：长时间运行的热代码（tsserver、tokenizer）在 V8 TurboFan/Maglev 下很快；纯 AOT 在多态代码上可能输。缓解：PGO、双版本、shape 推断，把门槛设为"启动/内存大幅领先，热路径持平"而不是全面碾压。
4. **扩展的动态代码**：任意 JS、`new Function`、`vm`、monkey-patch `Module`，甚至依赖 V8 特有行为（`Error.captureStackTrace`、`Error.prepareStackTrace`、stack 字符串格式）。需要模拟 V8 的这些非标准 API。
5. **Node 语义细节**：事件循环阶段、同步 fs、`Buffer` 边界、流背压、`child_process` 的 stdio 细节；Deno 和 Bun 投入巨大也只到 75% 左右的 Node 测试通过率。
6. **npm 生态与原生模块**：依赖树里带 `.node` 的包、postinstall 脚本、依赖 Electron ABI 的预编译二进制；N-API 的 libuv 部分很难完全兼容（Bun 也明确不打算完整实现）。
7. **Web 平台缺口**：workbench 需要的 CSS/DOM 细节（Grid、滚动、选择、拖拽、WebGL2 给 xterm）以及 WASM 运行时。
8. **规模**：VS Code 源码几十万行 + 依赖；AOT 编译时间和产物体积可能很大，需要增量编译和按模块缓存。
9. **跟版本**：VS Code 每月发版、Electron 每 8 周大版本；锁定一个 commit 达标后再滚动升级。

## 10. 开源项目核对（2026-10）

- **Static Hermes / Hermes V1**：RN 0.84 起 Hermes V1 成为默认引擎；"Static Hermes" 的名称已并入 Hermes V1。`shermes` AOT 编译器仍在开发，2026 年 5 月 Hermes 的 test262 runner 加了 `--shermes`（编出原生二进制再跑）。它是 typed 编译思路的最好参考，但主要针对 RN/Hermes 运行时，不能直接给 Node/Electron 用。
- **Porffor**：从零写的 JS/TS AOT 编译器。2026-03 版本 0.61.12（版本号次位 = test262 通过率约 61%）；2026-08-11 进入 alpha，重写为 JS → 自有 IR → C，加入了 GC，支持闭包与 async/await。适合作对照与思路参考，离跑 VS Code 还很远。
- **AssemblyScript**：TS 的严格子集 → WASM，不是通用 JS 编译器，不能编 VS Code，只能参考其 typed 降级做法。
- **Bun**：Node-API 测试 js-native-api 100%、node-api 47.7%（合计 76.3%）；按 Node v26 跟踪兼容性。
- **Deno**：2.8 起通过 Node 测试套件 75% 以上，`node:` 内建几乎全有，N-API 需要 `--allow-ffi`；`ext/node` 是 MIT，可参考或借用。
- **unenv**（UnJS）：Node 内建模块的 polyfill/预设集合；纯 JS 部分可直接复用（未逐项上网核对版本状态）。
- 其他：**wasmtime/Cranelift**（WASM 与后端 B）、**rusqlite / portable-pty / notify / rustls / hyper / ring**（原生模块重写）、**Open VSX**（扩展来源）。
