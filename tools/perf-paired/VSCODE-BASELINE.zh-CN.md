# VS Code A 侧基线驱动

这是 `tools/perf-paired` 上的 Code-OSS 驱动，只测**官方 Electron 版**（路线图第 7 节的 A）。它不声称 w3cos 更快，也不把应用自己报的时间拿去充当按键到像素的延迟。

清单 `vscode-baseline-manifest.json` 固定五个场景。改指标或把门槛放宽都要换 schema，运行时不能只改一边。

| 场景 | 证据 | 没有证据时 |
| --- | --- | --- |
| 冷启动到 workbench 就绪 | VS Code `--prof-duration-markers`，标记 `code/didStartMain-code/didStartWorkbench`（与上游 `ellapsed` 的起点一致） | 该样本为 `error`，不填 0 |
| 冷启动到窗口可见 | 外部探针，`source` 为 `compositor-timestamp` 或 `pixel-probe` | `blocked`，原因 `window-probe-not-configured` |
| 打开文件到文本首帧 | 同上 | `blocked` |
| 打字延迟 | 外部探针，`source` 必须是 `evdev-pixel-probe` | `blocked` |
| 内存 | `/proc/<pid>/smaps_rollup` 的 PSS/RSS，父进程加子进程求和，单位字节 | 读不到则为 `error` |

`complete` 要五个场景各有至少 20 个有效样本。少于 20 次、或还有场景被挡住，状态是 `partial` 或 `blocked`。默认 `--samples 1`，用来确认驱动能跑，不是一份达标报告。

```sh
python3 tools/perf-paired/vscode_driver.py \
  --code /绝对路径/code-oss \
  --samples 20 \
  --output code-oss-baseline.json
```

窗口、打开文件、打字需要探针。探针在 Code 进程还活着的时候启动，环境变量有 `W3COS_VSCODE_PID`、`W3COS_VSCODE_CODE`、`W3COS_VSCODE_USER_DATA_DIR`。标准输出一行 JSON：

```json
{"schema":"w3cos-vscode-probe-v1","source":"evdev-pixel-probe","elapsed_ms":11.2}
```

应用自报（`vscode-prof-duration-markers`、`vscode-self-report`）不能填进打字、窗口可见或打开文件。校验会拒绝这种信封。

冷启动前清页缓存、锁 CPU 频率、重复 20 次取中位数和 p95，按路线图第 7.1 节在测量机上做。这个驱动不替你 `drop_caches`（那要 root），也不会在没有探针时假装量过像素。

本地契约测试（不启动 Electron）：

```sh
python3 -m unittest discover -s tools/perf-paired -p 'test_vscode_driver.py' -v
```
