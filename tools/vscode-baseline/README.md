# Code-OSS 基线（M0，A 侧）

这把尺子只量**官方 Electron 版 Code-OSS**。w3cos 编译产物还不在这组数字里。

锁定提交在 `CODE_OSS.lock.json`：`microsoft/vscode` @ `e97039dd9293ef2b9df8876d0bc8c0c8ff87f871`（2026-10-17），`.npmrc` 的 `runtime=electron`、`target=43.7.7`、`build_from_source=true`，`package.json` 的 `main` 是 `./out/main.js`。Node 版本跟仓库 `.nvmrc`（24.18.0）。

## CI 里已经做的

针对 `aot-port` 的工作流会：

1. 按锁拉取这一棵 Code-OSS，核对 `package.json`、`.npmrc`、`product.json`、`.nvmrc` 的字段和 sha256。
2. 写出依赖清单（原生模块按 `native-modules.json` 分类）。
3. 跑 `w3cos port analyze`，得到 Electron API、Node 内建、原生模块和动态代码点的 JSON，并检查锚点还在（`app`、`BrowserWindow`、`protocol`、`fs`、`node-pty` 等）。

```sh
python3 tools/vscode-baseline/inventory.py --root /path/to/vscode --check --output dependency-inventory.json
cargo run -p w3cos-cli -- port analyze /path/to/vscode --output port-analyze.json
python3 tools/vscode-baseline/check_analyze_report.py port-analyze.json
```

`port analyze` 不解析 `node_modules`、`out` 和 `.d.ts`。测试目录里的动态代码进 `test_clearance_list`，不进产品待清零清单。

## 官方二进制

`build_code_oss.sh` 在锁死的提交上执行上游的 `npm ci` 和 `npm run gulp -- vscode-linux-x64`，再记下 `code-oss` 的 sha256。它不安装系统包。Node 主版本必须和 `.nvmrc` 一致。

PR 的 CI **不会**跑这段。GitHub 托管 runner 的时间和内存不够稳地编完 Electron 43 和原生模块。要手动编：

```sh
W3COS_BUILD_CODE_OSS=1 tools/vscode-baseline/build_code_oss.sh --dest "$PWD/target/code-oss"
```

工作流里同名的手动任务同样要设这个开关，并且可能需要比 `ubuntu-latest` 更大的机器。

## 还没做完的 M0

- 在 CI 里实际编出并保存官方 `code-oss` 二进制（脚本在，默认不跑）。
- 在有显示器的机器上采满 20 次 A 侧样本。驱动见 `tools/perf-paired/VSCODE-BASELINE.zh-CN.md`。窗口可见、打开文件首帧、打字延迟没有外部探针时保持 `blocked`，不会编造时间。
