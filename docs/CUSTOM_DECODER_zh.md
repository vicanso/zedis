中文 | [English](./CUSTOM_DECODER.md) · [← 返回 README](../README_zh.md)

# 自定义解码器（Custom Decoders）

Zedis 已内置支持 MessagePack、Protobuf、Java 原生序列化（`0xAC ED 00 05`）、Python Pickle、BSON、PHP `serialize()`、JWT、Base64、URL 编码、XML、YAML、Unix 时间戳、图片以及 GZIP / ZSTD / Snappy / LZ4 自动解压。当你的业务使用了私有二进制协议、自定义包头，或尚未内置的序列化格式（例如 Java FST、Kryo、Hessian、CBOR、Ruby Marshal、Brotli 等）时，可以在桌面端配置**自定义解码器**，通过外部命令或脚本完成解码，并支持可选的编辑回写。

> **仅桌面端支持。** 自定义解码器会在本机执行外部 Shell 命令，因此浏览器 / Web 版不提供此功能。

- [适用范围与解码优先级](#适用范围与解码优先级)
- [创建自定义解码器](#创建自定义解码器)
- [占位符与安全模型](#占位符与安全模型)
- [执行约定、限制与回退机制](#执行约定限制与回退机制)
- [双向编辑回写（编码命令）](#双向编辑回写编码命令)
- [实用示例](#实用示例)
- [通过 JSON 导入 / 导出共享规则](#通过-json-导入--导出共享规则)
- [常见问题排查](#常见问题排查)

---

## 适用范围与解码优先级

- **作用于 String 键**：打开 String 类型的键时，Zedis 按固定优先级依次执行解码管线：
  1. **Protobuf 查看器** —— 匹配该键的已注册 `.proto` schema
  2. **自定义解码器** —— 匹配该键的已配置 Shell 命令
  3. **原生格式检测** —— MessagePack、GZIP、ZSTD、Snappy、Unix 时间戳、图片、Java 原生序列化、Python Pickle、BSON
  4. **LZ4（带长度前缀）** —— 针对非 UTF-8 数据
  5. **文本编码** —— JWT、PHP `serialize()`、URL / 表单编码、Base64、XML、YAML
  6. **纯文本 / 美化 JSON** 兜底
- **关于集合元素**：Hash、List、Set、Sorted Set 的单元格与详情目前仅走 Zedis 内置解码管线（第 3 步起），**暂不触发**外部自定义脚本解码器。
- **默认只读**：未配置**编码命令（可选）**时，解码结果为只读预览（格式徽章显示 `script`；如需修改原始字节可切到 **Hex** 视图）。配置**编码命令**后，值编辑器会解锁编辑并支持保存回写。

---

## 创建自定义解码器

1. 打开窗口右上角菜单，进入 **设置 → 自定义解码器**（英文界面为 **Settings → Custom decoders**）。
2. 点击 **添加**，填写规则表单：
   - **服务器名称**：选择该规则生效的连接（按已保存的服务器维度隔离）。
   - **名称**：规则显示名称，会出现在规则列表以及解码失败提示中。
   - **匹配模式** 与 **模式**：决定哪些键名会触发该解码器：
     - `Prefix`：前缀匹配（例如 `session:` 或 `cache:user:`）
     - `Suffix`：后缀匹配（例如 `:pb` 或 `.fst`）
     - `Regex`：正则匹配（使用 Rust `regex` 语法，例如 `^tenant:\d+:profile$`）
     - `Exact`：精确匹配完整键名
   - **Shell 命令**：解码命令模板。macOS / Linux 下通过 `sh -c` 执行，Windows 下通过 `cmd /v:on /c` 执行。输入框下方提供 9 个**模板**快捷按钮（`Base64`、`Python Pickle`、`Ruby Marshal`、`PHP serialize`、`Protobuf (decode_raw)`、`ZIP`、`Brotli`、`Hex dump`、`Java serialized`），点击即可一键填入（需本机 `PATH` 中已安装对应命令行工具）。
   - **编码命令（可选）**：保存编辑内容时用于把文本重新编码成原始字节的命令模板；留空则为**仅解码**（只读预览）。
3. 点击 **保存**。

---

## 占位符与安全模型

在 **Shell 命令** 和 **编码命令** 中可使用以下 5 个占位符：

| 占位符 | 传入内容 | 传递方式 | 适用场景 |
| --- | --- | --- | --- |
| `{RAW_FILE}` | 原始字节（完整二进制） | 临时文件路径（执行结束后自动清理） | **强烈推荐**：所有二进制格式、含 `\0` 字节或大体积数据 |
| `{HEX_FILE}` | 小写十六进制字符串 | 临时文件路径 | 需要从文件读取 hex 的工具 |
| `{HEX}` | 小写十六进制字符串 | 长度 `<= 8,000` 个字符（`4,000` 字节）时内联展开；超出后自动改传临时文件路径 | 小体积二进制调试 |
| `{KEY}` | Redis 键名 | 环境变量引用（Unix 为 `"$ZEDIS_KEY"`，Windows 为 `!ZEDIS_KEY!`） | 根据键名区分结构的脚本 |
| `{VALUE}` | 原始值按 UTF-8 有损转换后的文本 | 环境变量引用（Unix 为 `"$ZEDIS_VALUE"`，Windows 为 `!ZEDIS_VALUE!`） | 短文本内容 |

### 键名与键值绝不参与命令行语法解析

由于匹配到键名后打开 key 就会自动触发解码，而 Redis 中的键名与键值可能包含引号、分号、`$()`、反引号等字符，因此 Zedis 绝不把 `{KEY}` 和 `{VALUE}` 直接拼接进命令字符串：

- **macOS / Linux**（`sh -c`）：仅当模板中出现 `{KEY}` / `{VALUE}` 时才导出对应环境变量，并将占位符替换为 `"$ZEDIS_KEY"` / `"$ZEDIS_VALUE"`，Shell 先完成语法解析再读取变量内容。
- **Windows**（`cmd /v:on /c`）：启用延迟变量扩展，将占位符替换为 `!ZEDIS_KEY!` / `!ZEDIS_VALUE!`。
- 如果你在模板里自己给占位符加了外层引号（例如 `'{KEY}'` 或 `"{VALUE}"`），Zedis 会自动剥去外层多余引号以免阻止变量展开。
- **NUL 字节与环境变量大小限制**：环境变量不能包含 `\0`（导出 `ZEDIS_KEY` / `ZEDIS_VALUE` 时会自动过滤 `\0`），且受操作系统单变量长度限制（Linux 上通常约 128 KiB）。因此**二进制数据或大值请一律使用 `{RAW_FILE}`**。
- **Windows 字面量 `!` 注意**：由于启用了 `/v:on`，命令模板里如果需要字面量 `!`，请转义写成 `^!`。

---

## 执行约定、限制与回退机制

- **标准输入关闭（`stdin = null`）**：不要编写直接等待 `stdin` 的命令而不做重定向；请通过参数传入 `{RAW_FILE}`，或使用 `< {RAW_FILE}` 重定向输入。
- **执行成功（退出码 `0`）**：Zedis 读取命令输出的 `stdout`（按 UTF-8 显示）并展示在值编辑器中，格式徽章为 `script`。如果脚本输出的是合法 JSON，编辑器会自动高亮并支持 JSON 树形查看。
- **失败或超时自动回退**：若命令启动失败、返回非零退出码或超时，Zedis 会弹出包含规则名称和 `stderr` 的提示，并**自动回退到原生格式检测**，确保不会因为脚本报错导致编辑器空白。
- **运行保护上限**：
  - **超时上限**：每次执行最多 `5 秒`（`SCRIPT_TIMEOUT`）。
  - **最大 stdout**：`4 MiB`（`MAX_SCRIPT_OUTPUT`），超出部分会被截断。
  - **最大 stderr**：`64 KiB`（`MAX_SCRIPT_STDERR`）。
  - **管道收尾宽限**：进程退出后最多等待 `2 秒`（`DRAIN_GRACE`）读取残留管道输出，防止子进程持有 `stdout` 拖死界面。

---

## 双向编辑回写（编码命令）

当规则配置了 **编码命令（可选）** 后：

1. 打开匹配的 String 键时，Zedis 用 **Shell 命令**解码，并在值编辑器中解锁文本编辑。
2. 点击 **保存** 后，Zedis 将编辑器中**修改后的 UTF-8 文本**通过 `{RAW_FILE}` / `{VALUE}` 传给 **编码命令**（同时支持 `{KEY}`）。
3. Zedis **按原始字节原样读取编码命令的 `stdout`**（二进制安全，不会二次转码），并通过 `SET` 写回 Redis。
4. 写回成功后，Zedis 会对新写入的字节再执行一次 **Shell 命令**，让编辑器与实际存储保持一致。
5. 若 **编码命令** 返回非零退出码或超时，Zedis 会弹出错误提示并**中止 `SET`**，Redis 中的原数据不会被修改。

---

## 实用示例

### 1. 双向 Base64（macOS / Linux）

- **Shell 命令（解码）**：
  ```sh
  base64 --decode {RAW_FILE}
  ```
- **编码命令（回写）**：
  ```sh
  base64 < {RAW_FILE}
  ```
*（注：Linux 的 GNU `base64` 默认每 76 列换行，若业务要求单行无换行可加 `-w 0`。）*

### 2. 双向 Brotli 解压 / 压缩

- **Shell 命令（解码）**：
  ```sh
  brotli -dc {RAW_FILE}
  ```
- **编码命令（回写）**：
  ```sh
  brotli -c {RAW_FILE}
  ```

### 3. 使用 Python 脚本解码自定义二进制 / CBOR / 带自定义包头的数据

编写脚本（例如 `~/.config/zedis/decode_cbor.py`），通过绝对路径调用：

```python
#!/usr/bin/env python3
import json
import sys
import cbor2  # pip install cbor2

with open(sys.argv[1], "rb") as f:
    raw = f.read()

# 示例：跳过 4 字节自定义包头后再解 CBOR
payload = cbor2.loads(raw[4:])
print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
```

- **Shell 命令**：
  ```sh
  /usr/bin/python3 /path/to/decode_cbor.py {RAW_FILE}
  ```

### 4. 调用 Java / 自定义 CLI 工具（例如 Java FST / Kryo / Hessian）

如果你已经有带业务 POJO 依赖的可执行 JAR 或二进制工具，能够把字节反序列化成 JSON 打印到标准输出：

- **Shell 命令**：
  ```sh
  /path/to/java -jar /path/to/fst-cli.jar {RAW_FILE}
  ```

> **注意 5 秒超时限制。** 较重的 JVM 冷启动在首次调用时可能会接近甚至超过 5 秒。建议尽量减小依赖体积、编译为 GraalVM `native-image` 原生可执行文件，或者在本地起一个常驻解码服务并通过 `curl` 调用（如 `curl -sf --data-binary @{RAW_FILE} http://127.0.0.1:18080/decode`）。

---

## 通过 JSON 导入 / 导出共享规则

在 **设置 → 自定义解码器** 页面右下角，可通过 **导出** 和 **导入** 在团队内共享解码规则：

- **文件格式**：JSON 数组（或单个 JSON 对象），单个导入文件上限为 **4 MiB**：
  ```json
  [
    {
      "server_id": "local-redis",
      "name": "Brotli JSON",
      "shell_command": "brotli -dc {RAW_FILE}",
      "encode_command": "brotli -c {RAW_FILE}",
      "match_pattern": "br:",
      "mode": "Prefix"
    }
  ]
  ```
- **`mode` 可选值**：`"Prefix"`、`"Suffix"`、`"Regex"`、`"Exact"`。
- **`encode_command`**：可选字段（留空或省略表示仅解码）。
- **导入时的 `server_id` 自动映射**：如果导入 JSON 中的 `server_id` 为空，或在当前机器上找不到对应 ID 的连接，Zedis 会自动将其归属到当前打开的连接。
- 此外，**设置 → 本地数据** 中的整机备份导出/导入也会一并包含自定义解码器规则。

---

## 常见问题排查

- **从 Finder / 桌面图标启动后提示 `command not found`**：macOS / Linux 图形界面应用的环境变量 `PATH` 通常很精简，不包含 Homebrew（`/opt/homebrew/bin`）、`pyenv`、`nvm` 或 `~/.local/bin`。请在命令中使用工具的**完整绝对路径**（如 `/opt/homebrew/bin/brotli`）。
- **提示 `script timed out after 5000ms`**：通常是脚本/JVM 启动超过 5 秒、脚本误在等待 `stdin` 输入（未读取 `{RAW_FILE}`），或者命令在后台派生了未关闭 `stdout` 的子进程。
- **使用 `{VALUE}` 解码二进制数据时字节损坏**：`{VALUE}` 会做 UTF-8 有损转换并过滤 `\0` 字节。二进制数据请务必改用 **`{RAW_FILE}`**。
- **同一批 key 被别的规则拦截**：若多个规则的模式都能匹配同一个 key，请收窄 `match_pattern`（使用更具体的前缀、后缀或正则），避免规则重叠。
