# w3cos 下一代智能输入协议（W3C-IME Protocol）规划草案

> 状态：草案 v0.1 · 2026-10-15 · 面向 w3cos AI Shell（意图优先 / 本地优先 / 提议→确认→执行→回执）
>
> 本文位置：`docs/ime/smart-ime-protocol.md`。JSON Schema 仍计划放在 `spec/ime/`，尚未落地。

## 0. 一句话

**输入法不再是一个“打字程序”，而是一个协议**：任何输入源（键盘、触摸、手写、语音、手势、摄像头、剪贴板、另一台设备）把用户的“想表达的东西”变成统一的 **输入意图（InputIntent）**，由引擎结合 **授权范围内的上下文** 给出候选、补全或 **动作提议**，用户一次确认即提交，可撤销、有回执、全程本地。

## 1. 现有方案调研（Prior Art）

| 方案 | 要点 | 可借鉴 | 不足 |
|---|---|---|---|
| W3C EditContext API https://w3c.github.io/edit-context/ | 将文本编辑与 DOM 解耦，应用自持文本模型，接收 textupdate/textformatupdate/characterboundsupdate | 作为 Web 应用侧的上下文与提交通道 | 只管文本，无语义/动作 |
| UI Events 组合事件 https://w3c.github.io/uievents/#events-compositionevents | compositionstart/update/end | 兼容现有网页的 CJK 组合 | 仅低层事件 |
| Input Events Level 2 https://w3c.github.io/input-events/ | beforeinput 的 inputType（insertText、insertReplacementText…） | 提交/撤销语义映射 | 无候选模型 |
| Virtual Keyboard API https://w3c.github.io/virtual-keyboard/ | 软键盘几何、overlaysContent | 移动端布局 | — |
| Wayland text-input-v3 https://wayland.app/protocols/text-input-unstable-v3 | 应用↔合成器：surrounding_text、content_type(hint/purpose)、preedit、commit | content_purpose(password 等) 用于安全字段 | 双缓冲、无多模态 |
| Wayland input-method-v2 https://wayland.app/protocols/input-method-unstable-v2 | 合成器↔输入法进程，popup surface | 引擎作为独立进程 | 每次仅一个 IM |
| IBus https://github.com/ibus/ibus / Fcitx5 https://fcitx-im.org/wiki/Fcitx_5 | D-Bus 框架，引擎插件（拼音、Rime） | 插件化引擎、Rime 方案 | 桌面 Linux 专属 |
| Android InputConnection https://developer.android.com/reference/android/view/inputmethod/InputConnection | getTextBeforeCursor、commitText、setComposingText、performEditorAction、EditorInfo.inputType | 上下文读取 + 编辑动作 | 上下文无细粒度授权 |
| Apple Scribble / UIIndirectScribbleInteraction https://developer.apple.com/documentation/uikit/uiindirectscribbleinteraction-1nfjm | 手写直接写入任意字段，手势删除/选择 | 手写与手势作为一等输入 | 封闭 |
| Web Speech API https://webaudio.github.io/web-speech-api/ | SpeechRecognition interim/final 结果 | 流式结果 → 预编辑 | 浏览器多走云端 |
| Model Context Protocol https://modelcontextprotocol.io/specification/2026-07-28 | JSON-RPC 资源/工具/提示，能力协商 | 消息信封、能力协商、工具化动作 | 非输入场景 |
| WAI-ARIA / a11y 树 https://www.w3.org/TR/wai-aria-1.2/ | 角色、名称、状态 | 原生应用上下文来源（[w3cos #50](https://github.com/wangnaihe/w3cos/issues/50) a11y index） | — |

结论：**文本层**复用 EditContext / text-input-v3 / InputConnection 的成熟语义；**新增**的是多模态输入意图、上下文授权范围、动作提议、回执与跨设备同步。

## 2. 愿景与原则

1. **极简**：用户只表达意图，“打字”只是一种方式；常见场景 0~1 次确认完成。
2. **本地优先**：所有识别、预测默认本机完成；无网络可用；云/远端引擎必须显式授权且可见。
3. **用户掌控**：引擎只能“提议”，不能“替你做”；提交文本可撤销，执行动作必须确认并留回执。
4. **不做键盘记录**：协议层禁止持久化原始按键流；安全字段完全旁路。
5. **最小上下文**：应用按 scope 暴露上下文，引擎按需申请，用户可见可撤销。
6. **开放协议**：规范公开，引擎可替换（拼音、Rime、手写、语音、第三方 AI），去中心化分发。
7. **多语言天生**：CJK 组合、RTL、混输是一等公民，与 shell 的 Fluent 语言包共享 locale。

## 3. 架构分层

```
┌──────────── 输入源 Sources ────────────┐
 键盘 触摸/滑行 手写 语音 手势 摄像头/OCR 剪贴板 其他设备
└──────────────┬─────────────────────────┘
               ▼  L1 采集层：原始信号 → InputIntent（模态无关）
┌──────────── IME Hub（shell 内，系统服务）─────────┐
 路由 · 权限 · 安全字段旁路 · 回执 · 撤销栈 · 流式会话
   ├─ 引擎插件 Engines（进程隔离）：拼音/Rime、手写、ASR、OCR、L2 LLM
   ├─ 上下文提供者 Context Provider（EditContext / a11y / 原生 trait）
   └─ 个人模型 Personal Store（加密、did:key 同步）
└──────────────┬───────────────────────┘
               ▼  L3 提议层：Candidate / Completion / ActionProposal
               ▼  L4 提交层：commit / replace / undo / execute(确认后)
          应用（Web: EditContext+navigator.w3cos.ime；原生：Rust trait）
          Shell 意图栏（输入 → 意图）
```

- Hub 运行在 shell（`libw3cos_runtime.so`）内；引擎作为子进程或 WASM 插件，经 stdio/本地 socket 以 JSON-RPC 2.0 通讯（与 `w3cos-ai-mcp`、llama-server 子进程模式一致）。
- 合成器侧对齐 Wayland text-input-v3 / input-method-v2 语义，便于将来与 Linux 生态互通。

## 4. 统一消息模型

### 4.1 会话（Session）
每次焦点进入可编辑目标即建立 `session`，携带：目标描述（purpose/hints/locale/安全级）、授权上下文 scope、活动引擎集合。焦点离开即销毁，内存清零。

### 4.2 InputIntent（输入意图）
模态无关的“用户想输入/想做什么”的中间表示：

| 字段 | 说明 |
|---|---|
| `modality` | key / swipe / handwriting / voice / gesture / camera / clipboard / handoff |
| `payload` | 按模态：键序列、笔迹、音频片段引用、OCR 文本等（仅在会话内存中） |
| `interim` | 是否流式中间结果 |
| `locale` | BCP 47 列表，支持混输 `["zh-Hans","en"]` |
| `hint` | 可选：插入 / 替换选区 / 命令 / 搜索 |

### 4.3 输出种类
- **Preedit**：组合中文本（拼音串、语音中间结果），可带分段与光标。
- **Candidate**：候选词/句，带来源引擎、置信度、是否个人词。
- **Completion**：续写（行尾灰字），Tab/右滑接受。
- **Edit**：结构化编辑（替换选区、改写语气、纠错、翻译），以 diff 形式提出。
- **ActionProposal**：非文本动作（“发给张三”“设 3 点提醒”“打开文件”），**必须走 shell 确认**。

### 4.4 提交与撤销
`commit` 返回 `commitId`；`undo(commitId)` 原子撤销（含一次改写的多处替换）。动作执行后的撤销由 shell 回执流程负责（能撤则撤，否则提示补偿操作）。

## 5. 上下文提供（Context Provider）与权限

| scope | 内容 | 默认 |
|---|---|---|
| `ctx.purpose` | 字段类型（text/email/url/number/search/password…）、locale | 总是允许 |
| `ctx.surrounding` | 光标前后 N 字（默认 ≤ 256） | 允许（本机引擎） |
| `ctx.selection` | 选区文本 | 允许（本机引擎） |
| `ctx.document` | 全文/对话历史 | 需授权，按应用记住 |
| `ctx.app` | 应用名、窗口标题、a11y 节点角色 | 需授权 |
| `ctx.screen` | 屏幕截图/OCR（复用 [#51](https://github.com/wangnaihe/w3cos/issues/51) 标注截图） | 每次确认 |
| `ctx.personal` | 联系人、日程等用于实体补全 | 需授权，仅本地 |

- Web 应用：从 EditContext 的 text/selection 与 `navigator.w3cos.ime` 声明读取。
- 原生应用：实现 `ImeContext` trait；未实现时由 a11y index（[#50](https://github.com/wangnaihe/w3cos/issues/50)）兜底，只读。
- 远端/第三方引擎：默认只能拿到 `ctx.purpose`，其余逐项授权；授权写回执。
- 安全分级：`secure`（密码、OTP、支付 PIN、助记词）→ **旁路**：仅基础键盘引擎，无预测、无上下文、无日志、不进个人模型、禁止截图与跨设备同步。

## 6. 与 Shell 意图栏及 L0/L1/L2 的整合

- **同一条管线**：意图栏就是一个 `purpose=command` 的输入目标；任意文本框里也可以“升级”为意图（长按空格 / 输入 `/` / 语音“帮我…”）。
- 分层：
  - **L0 规则**：确定性转换（拼音→汉字词典、数字/日期格式、快捷短语、`/` 命令）。毫秒级、总是开启。
  - **L1 本地同义词**：个人词库、同义映射、实体补全（联系人、文件名），把自然短语映射到已知动作。
  - **L2 本地 LLM（默认关闭）**：整句改写、续写、翻译、多步动作规划；经 llama-server 子进程，语法约束输出 ActionProposal JSON。
- **文本 vs 动作**：
  - 文本类 Edit/Candidate：直接提交，可撤销，不打扰。
  - ActionProposal：进入 shell 的 **提议→确认→执行→回执**；确认卡显示动作、目标、所需权限；跨应用动作需要 `interactive` 以上权限。
- 示例：在聊天框输入“明天下午三点和老王开会”→ 候选 1：原文；候选 2：ActionProposal「创建日程 明天 15:00 · 与王某」→ 确认后执行并生成加密回执，聊天框里插入“好的，已约明天 15:00”。

## 7. 应用侧 API

### 7.1 Web：`navigator.w3cos.ime`（EditContext 之上的扩展）
```js
const ec = new EditContext();
el.editContext = ec;                      // 标准文本通道
const s = await navigator.w3cos.ime.attach(ec, {
  purpose: "chat",                        // text|chat|email|search|command|code|secure
  locales: ["zh-Hans", "en"],
  share: ["surrounding", "selection"],    // 主动声明可共享的上下文
  actions: ["calendar.create", "contact.send"] // 应用可承接的动作
});
s.addEventListener("proposal", e => {/* 应用自定义候选 UI（可选） */});
s.addEventListener("action", async e => { await e.respondWith(handle(e.action)); });
```
- 未调用扩展的普通网页：仍得到标准 composition / beforeinput 事件，完全兼容。
- 能力门控走现有 `capability_gate`。

### 7.2 原生 Rust trait
```rust
pub trait ImeContext {
    fn purpose(&self) -> Purpose;                 // 含 Secure
    fn surrounding(&self, before: usize, after: usize) -> Option<Surrounding>;
    fn selection(&self) -> Option<TextRange>;
    fn caret_rect(&self) -> Rect;                 // 候选窗定位
    fn apply(&mut self, edit: Edit) -> CommitId;  // insert/replace/preedit
    fn undo(&mut self, id: CommitId) -> bool;
    fn accepts_action(&self, kind: &str) -> bool { false }
}

pub trait InputEngine: Send {
    fn manifest(&self) -> EngineManifest;         // 模态、语言、所需 scope
    fn on_intent(&mut self, s: &Session, i: &InputIntent) -> Vec<Proposal>;
    fn on_feedback(&mut self, f: Feedback) {}     // 接受/拒绝 → 个人模型
}
```
C ABI 1.1 插件接口增加 `w3cos_ime_engine_v1` 入口，与共享运行时同进程或子进程加载。

## 8. 多语言与 CJK

- 组合（preedit）为一等公民：拼音、注音、五笔、仓颉、日文假名、韩文字母合成都走 `Preedit` 分段模型；兼容 Rime 方案（首个引擎可直接封装 librime）。
- 混输：同一会话多 locale，引擎按段判语种（“明天 meeting 改到 3pm”）。
- RTL：候选窗、preedit 光标按 bidi 处理；路径/代码/短码保持 LTR（与 [#62](https://github.com/wangnaihe/w3cos/pull/62) 第 14.4 节一致）。
- locale 来源：shell Fluent 语言包设置；每设备独立，字段 `lang` 属性可覆盖。
- 语音/手写引擎按已安装语言包启用；L2 跨语言改写、翻译作为 Edit 提出。
- 字体：候选渲染复用 fontconfig + CJK 回退链。

## 9. 个人模型与跨设备同步

- **内容**：个人词库、短语、用词频率、纠错习惯、实体别名（“老王”→联系人）、引擎偏好；可选 L2 LoRA 适配器。
- **不存**：原始按键流、原始音频、安全字段任何内容、完整文档。
- **存储**：本地加密（ChaCha20-Poly1305，密钥由解锁口令 PBKDF2 派生，与回执同源），格式 `W3P1`。
- **同步**：沿用多设备接续：did:key 配对设备之间 P2P 加密同步；CRDT（计数用 G-Counter、词条用 LWW-Map）解决冲突；可选导出到用户自选的去中心化存储（IPFS 等）作为备份，仅密文。
- **可见可控**：设置里可查看、编辑、导出、一键清空；可按应用关闭学习（“隐身输入”）。
- **接续**：手机上开始的输入草稿（未提交 preedit 不同步，已存草稿可同步）在桌面继续；动作确认不跟随，需在新设备重新确认。

## 10. 安全

1. 安全字段旁路（第 5 节），系统级硬约束，应用不可降级。
2. 禁止键盘记录：Hub 不提供原始事件订阅接口给第三方；引擎只看当前会话；会话结束清零。
3. 引擎沙箱：子进程/WASM，默认无网络、无文件系统；网络需在 manifest 声明并经用户授权，状态栏常亮指示。
4. 权限模型：scope 授权按（引擎 × 应用）粒度记录，可在设置撤销。
5. 回执：动作执行、敏感 scope 授权、远端引擎调用写入加密回执（不含输入正文，仅摘要哈希与元数据）。
6. 防注入：上下文文本对 L2 视为不可信数据；L2 只能产出 Proposal，不能直接执行。
7. 防伪造：候选窗/确认卡由 shell 绘制在受信层，应用不可覆盖或模拟。
8. 引擎签名：插件 manifest 由开发者 did:key 签名，shell 显示来源与权限。

## 11. 可扩展性与开放生态

- 草案在 `docs/ime/smart-ime-protocol.md`。JSON Schema 计划公开于 `spec/ime/`，版本化 `w3cos-ime/1`。
- 引擎形态：原生动态库（C ABI）、子进程（JSON-RPC stdio）、WASM 组件。
- 分发去中心化：引擎包含签名 manifest，可从任意来源（git、IPFS、本地文件）安装；不依赖中心化商店。
- 互通桥：IBus/Fcitx5 引擎适配器、Wayland input-method-v2 桥、MCP 适配（让外部 AI 代理以“输入源”身份提交提议，需 interactive 权限）。

## 12. 消息示例（JSON-RPC 2.0）

能力协商：
```json
{"jsonrpc":"2.0","id":1,"method":"ime.hello","params":{
  "protocol":"w3cos-ime/1",
  "engine":{"id":"org.rime.pinyin","did":"did:key:z6Mk...","version":"0.1.0"},
  "modalities":["key"],"locales":["zh-Hans"],
  "scopes":["ctx.purpose","ctx.surrounding"],"network":false}}
```
会话开始（Hub → 引擎）：
```json
{"jsonrpc":"2.0","method":"session.begin","params":{
  "session":"s-7f2a","purpose":"chat","security":"normal",
  "locales":["zh-Hans","en"],
  "context":{"surrounding":{"before":"周五的评审","after":""},"selection":null},
  "granted":["ctx.purpose","ctx.surrounding"]}}
```
输入意图（流式语音）：
```json
{"jsonrpc":"2.0","method":"intent.input","params":{
  "session":"s-7f2a","seq":12,"modality":"voice","interim":true,
  "payload":{"text":"改到下周一上午"},"locale":["zh-Hans"]}}
```
提议（引擎 → Hub）：
```json
{"jsonrpc":"2.0","method":"proposal.update","params":{
  "session":"s-7f2a","seq":12,"items":[
   {"id":"c1","kind":"candidate","text":"改到下周一上午","score":0.92,"source":"L0"},
   {"id":"e1","kind":"edit","op":"replace","range":[0,5],"text":"周一上午的评审","source":"L2"},
   {"id":"a1","kind":"action","action":"calendar.move",
    "args":{"event":"周五评审","to":"2026-10-19T10:00+08:00"},
    "requires":["calendar.write"],"confirm":true,"source":"L1"}]}}
```
提交与撤销：
```json
{"jsonrpc":"2.0","id":7,"method":"commit","params":{"session":"s-7f2a","item":"c1"}}
{"jsonrpc":"2.0","id":7,"result":{"commitId":"k-301"}}
{"jsonrpc":"2.0","id":8,"method":"undo","params":{"commitId":"k-301"}}
```
动作转交 shell（确认后回执）：
```json
{"jsonrpc":"2.0","method":"shell.propose","params":{
  "from":"ime","session":"s-7f2a","item":"a1","receipt":true}}
```
安全字段：
```json
{"jsonrpc":"2.0","method":"session.begin","params":{
  "session":"s-8b10","purpose":"password","security":"secure",
  "context":null,"granted":[],"engines":["builtin.basic-keyboard"]}}
```

## 13. 交互要点（桌面 + 手机）

- 桌面：候选条贴近光标；意图提议以浅色芯片显示在候选末尾；`Tab` 接受补全，`Ctrl+Z` 原子撤销，`Esc` 收起。
- 手机：键盘上方“智能条”= 候选 + 动作芯片；长按麦克风流式语音；手写区直接写入字段（Scribble 式）；划除手势删除。
- 统一：动作芯片永远带图标与“需确认”标识；远端引擎工作时显示网络标记。

## 14. 开放问题

1. Hub 与合成器边界：放在 shell 进程内，还是独立 `w3cos-imed` 服务？
2. 上下文默认值：`surrounding` 256 字是否过多？是否按应用学习？
3. 第三方引擎是否允许看到 `ctx.document`，还是只允许系统内置 L2？
4. 个人模型同步粒度：全量词库 vs 仅增量统计；LoRA 适配器是否同步（体积）？
5. 语音/手写模型选型：whisper.cpp / sherpa-onnx；手写用开源 CNN 还是 L2 多模态？
6. 应用自绘候选 UI 的边界：如何防止钓鱼式仿冒确认？
7. 与 W3C 标准化路径：以 EditContext 扩展提案形式推进到 WICG？
8. 多引擎同时提议时的排序与合并策略（L0/L1/L2 及第三方）。
9. 跨设备“在手机上输入、桌面上提交”的延迟与安全确认方式。

## 15. 路线图

| 阶段 | 内容 | 验收 |
|---|---|---|
| P0 规范 | 本草案 → `spec/ime/` JSON Schema；消息与 scope 定稿 | Schema 校验测试 |
| P1 MVP | Hub 骨架；键盘 + 拼音（封装 librime 或简易词典）；preedit/candidate/commit/undo；EditContext 与原生 trait 打通；安全字段旁路 | shell 内中英文输入、密码框旁路测试 |
| P2 意图打通 | 意图栏接入同一管线；L0/L1 ActionProposal → 确认 → 回执；个人词库本地加密 | “明天 3 点开会”→日程提议端到端 |
| P3 多模态 | 流式语音（本地 ASR）、手写、剪贴板/OCR（复用 [#51](https://github.com/wangnaihe/w3cos/issues/51)）；手机智能条 | 离线语音输入、手写入框 |
| P4 L2 智能 | 改写/续写/翻译 Edit；语法约束动作规划；上下文授权 UI | L2 默认关闭，开启后效果基准 |
| P5 同步与接续 | did:key 设备间个人模型 CRDT 同步；草稿接续 | 两设备词库合并、冲突测试 |
| P6 开放生态 | 第三方引擎插件（C ABI/WASM/JSON-RPC）、签名 manifest、IBus/Fcitx5/Wayland/MCP 桥 | 外部引擎可安装、权限可撤销 |
| P7 标准化 | 整理为 WICG 提案（EditContext 语义扩展 + 输入意图） | 公开规范与参考实现 |
