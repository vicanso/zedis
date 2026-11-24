[中文](./CUSTOM_DECODER_zh.md) | English · [← Back to README](../README.md)

# Custom Decoders

Zedis includes built-in decoders for MessagePack, Protobuf, Java Serialization (`0xAC ED 00 05`), Python Pickle, BSON, PHP `serialize()`, JWT, Base64, URL encoding, XML, YAML, timestamps, images, and GZIP/ZSTD/Snappy/LZ4 compression. When your application uses an internal binary format, custom wrapper header, or another serialization protocol (such as Java FST, Kryo, Hessian, CBOR, Ruby Marshal, or Brotli), you can configure a **Custom decoder** in the desktop app to pipe values through an external CLI tool or script.

> **Desktop app only.** Custom decoders execute local shell commands on your machine and are therefore unavailable in the browser/Web version.

- [Scope and Pipeline Priority](#scope-and-pipeline-priority)
- [Creating a Custom Decoder](#creating-a-custom-decoder)
- [Placeholders and Security Model](#placeholders-and-security-model)
- [Execution Contract, Limits, and Fallback](#execution-contract-limits-and-fallback)
- [Bidirectional Write-Back (`Encode command`)](#bidirectional-write-back-encode-command)
- [Practical Examples](#practical-examples)
- [Sharing Rules via JSON Import / Export](#sharing-rules-via-json-import--export)
- [Troubleshooting](#troubleshooting)

---

## Scope and Pipeline Priority

- **Applies to String keys**: When opening a String key, Zedis runs its decode pipeline in a deterministic order:
  1. **Protobuf viewer** — a registered `.proto` schema matching the key
  2. **Custom decoder** — a configured shell command matching the key
  3. **Native format detection** — MessagePack, GZIP, ZSTD, Snappy, Unix timestamps, images, Java Serialization, Python Pickle, BSON
  4. **LZ4** (size-prepended) — for non-UTF-8 payloads
  5. **Text encodings** — JWT, PHP `serialize()`, URL/form encoding, Base64, XML, YAML
  6. **Text / pretty-printed JSON** fallback
- **Collection elements**: Hash, List, Set, and Sorted Set elements currently use Zedis's built-in decoders (step 3 onward) and do **not** run external custom decoder scripts.
- **Read-only by default**: Without an **Encode command**, the decoded result is a read-only preview (the badge displays `script`; edit raw bytes in **Hex** mode if needed). Adding an **Encode command** unlocks editing and write-back directly from the value editor.

---

## Creating a Custom Decoder

1. Open the top-right application menu and go to **Settings → Custom decoders**.
2. Click **Add** to open the rule form and fill in the fields:
   - **Server name**: Selects which saved Redis/Valkey connection this rule applies to.
   - **Name**: A human-readable rule name shown in the table and in error notifications.
   - **Match pattern** and **Mode**: Controls which key names trigger the decoder:
     - `Prefix`: matches keys starting with the pattern (e.g. `session:` or `cache:user:`)
     - `Suffix`: matches keys ending with the pattern (e.g. `:pb` or `.fst`)
     - `Regex`: matches keys using Rust `regex` syntax (e.g. `^tenant:\d+:profile$`)
     - `Exact`: matches one exact key name
   - **Shell command**: The decode command template executed via `sh -c` on macOS/Linux or `cmd /v:on /c` on Windows. Below the input, **starter template chips** can prefill commands for common tools (`Base64`, `Python Pickle`, `Ruby Marshal`, `PHP serialize`, `Protobuf (decode_raw)`, `ZIP`, `Brotli`, `Hex dump`, `Java serialized` — each assumes the corresponding tool is available on `PATH`).
   - **Encode command (optional)**: An inverse command template used when saving edits back to Redis. Leave empty for read-only preview (`Decode only`).
3. Click **Save**.

---

## Placeholders and Security Model

Zedis supports five placeholders inside `Shell command` and `Encode command`:

| Placeholder | Content | How it is passed | Best used for |
| --- | --- | --- | --- |
| `{RAW_FILE}` | Exact raw bytes of the value | Path to a temporary file (auto-cleaned after exit) | **Recommended** for binary data, arbitrary bytes, NUL bytes, and large values |
| `{HEX_FILE}` | Lowercase hex string of the raw bytes | Path to a temporary file | Tools that expect hex input from a file |
| `{HEX}` | Lowercase hex string of the raw bytes | Inline text if `<= 8,000` hex chars (`4,000` bytes); automatically switches to a temp file path if larger | Small binary inspections |
| `{KEY}` | Redis key name | Out-of-band env var (`"$ZEDIS_KEY"` on Unix, `!ZEDIS_KEY!` on Windows) | Passing the key name to a script that routes schemas by key |
| `{VALUE}` | Lossy UTF-8 text of the value | Out-of-band env var (`"$ZEDIS_VALUE"` on Unix, `!ZEDIS_VALUE!` on Windows) | Short UTF-8 text values |

### Key and value never touch command-line parsing

Because a custom decoder triggers automatically when you open a matching key, both `{KEY}` and `{VALUE}` come from Redis server data that could contain shell metacharacters (such as quotes, `$()`, backticks, or semicolons). Zedis never interpolates `{KEY}` or `{VALUE}` as raw command-line text:

- On **macOS / Linux** (`sh -c`), Zedis sets `ZEDIS_KEY` / `ZEDIS_VALUE` only when those placeholders are used and replaces `{KEY}` / `{VALUE}` with `"$ZEDIS_KEY"` / `"$ZEDIS_VALUE"`.
- On **Windows** (`cmd /v:on /c`), Zedis enables delayed expansion and replaces them with `!ZEDIS_KEY!` / `!ZEDIS_VALUE!`.
- If you wrapped a placeholder in quotes yourself (`'{KEY}'` or `"{VALUE}"`), Zedis strips that outer pair cleanly so expansion still works as intended.
- **NUL bytes and size limits**: Environment variables cannot contain `\0` bytes (Zedis strips NUL bytes from `ZEDIS_KEY` / `ZEDIS_VALUE`) and are subject to OS limits (~128 KiB per variable on Linux). Always use **`{RAW_FILE}`** for binary payloads or large strings.
- **Windows literal `!` note**: Because Windows runs with `/v:on`, a literal exclamation mark `!` in a command template must be escaped as `^!`.

---

## Execution Contract, Limits, and Fallback

- **`stdin` is closed (`null`)**: Do **not** write scripts that block reading `stdin` without redirecting `{RAW_FILE}`. Use `{RAW_FILE}` as a file argument or redirect `< {RAW_FILE}`.
- **Success (`exit 0`)**: `stdout` (interpreted as UTF-8, lossy if needed) is displayed in the value editor with the `script` format badge. Pretty-printed JSON output works directly with Zedis's JSON syntax highlighting and JSON Tree view.
- **Failure or timeout**: If the command fails to spawn, exits non-zero, or times out, Zedis displays a notice naming the rule and `stderr`, then **falls back to native format detection** so a broken rule never leaves you with a blank editor.
- **Runtime limits**:
  - **Timeout**: `5 seconds` per execution (`SCRIPT_TIMEOUT`).
  - **Max stdout**: `4 MiB` (`MAX_SCRIPT_OUTPUT`); output beyond 4 MiB is truncated and logged.
  - **Max stderr**: `64 KiB` (`MAX_SCRIPT_STDERR`).
  - **Pipe drain grace**: `2 seconds` after process exit (`DRAIN_GRACE`) in case a background child process kept `stdout` open.

---

## Bidirectional Write-Back (`Encode command`)

When **Encode command** is filled in:

1. Opening a matching String key decodes it via **Shell command** and unlocks the value editor for editing.
2. When you click **Save**, Zedis runs **Encode command**, passing the **edited UTF-8 text** as `{RAW_FILE}` / `{VALUE}` (and the key name as `{KEY}`).
3. Zedis reads the encoder's **raw `stdout` bytes** (binary-safe — it does not re-encode stdout as text) and writes them to Redis via `SET`.
4. Zedis then re-runs **Shell command** on the newly written bytes so the editor reflects the canonical decoded view.
5. If **Encode command** exits non-zero or times out, Zedis shows an error notification and **aborts the `SET`**, leaving Redis untouched.

---

## Practical Examples

### 1. Bidirectional Base64 (macOS / Linux)

- **Shell command**:
  ```sh
  base64 --decode {RAW_FILE}
  ```
- **Encode command**:
  ```sh
  base64 < {RAW_FILE}
  ```
*(Note: `base64` on Linux wraps at 76 columns by default; add `-w 0` on GNU `base64` if your application expects a single line without line breaks.)*

### 2. Bidirectional Brotli Compression

- **Shell command**:
  ```sh
  brotli -dc {RAW_FILE}
  ```
- **Encode command**:
  ```sh
  brotli -c {RAW_FILE}
  ```

### 3. Python Script for Custom Binary / CBOR / Custom Header

Write a small script (e.g. `~/.config/zedis/decode_cbor.py`) and call it by absolute path:

```python
#!/usr/bin/env python3
import json
import sys
import cbor2  # pip install cbor2

with open(sys.argv[1], "rb") as f:
    raw = f.read()

# Example: skip a 4-byte custom header before decoding CBOR
payload = cbor2.loads(raw[4:])
print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
```

- **Shell command**:
  ```sh
  /usr/bin/python3 /path/to/decode_cbor.py {RAW_FILE}
  ```

### 4. Calling a Java CLI Tool (e.g. FST / Kryo / Hessian)

If you have a small helper JAR or compiled binary that deserializes domain classes (like Java FST with project-specific POJO JARs on the classpath) and prints JSON to stdout:

- **Shell command**:
  ```sh
  /path/to/java -jar /path/to/fst-cli.jar {RAW_FILE}
  ```

> **Watch the 5-second timeout.** A heavy JVM cold start can approach or exceed 5 seconds on the first invocation. To keep decoding snappy, prefer a lightweight CLI with minimal classpath, a GraalVM `native-image` binary, or a tiny local HTTP/Unix-socket helper script invoked with `curl` (`curl -sf --data-binary @{RAW_FILE} http://127.0.0.1:18080/decode`).

---

## Sharing Rules via JSON Import / Export

In **Settings → Custom decoders**, use **Export** and **Import** to share decoder rules across a team:

- **Format**: A JSON array (or a single JSON object) up to **4 MiB**:
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
- **`mode` values**: `"Prefix"`, `"Suffix"`, `"Regex"`, `"Exact"`.
- **`encode_command`**: Optional (`""` or omitted means decode-only).
- **`server_id` mapping on import**: If an imported entry's `server_id` is empty or does not match any saved server on the recipient's machine, Zedis automatically maps it to the currently active server connection.
- Custom decoders are also included when exporting/importing machine-local backups under **Settings → Local data**.

---

## Troubleshooting

- **`command not found` when launching Zedis from Finder / Desktop Launcher**: GUI apps on macOS and Linux often inherit a minimal `PATH` that omits Homebrew (`/opt/homebrew/bin`), `pyenv`, `nvm`, or user bins (`~/.local/bin`). Use the **full absolute path** to the binary in `Shell command` (e.g. `/opt/homebrew/bin/brotli` instead of `brotli`).
- **`script timed out after 5000ms`**: Either the tool takes longer than 5 seconds to start/run, or it tried to read from `stdin` instead of `{RAW_FILE}`, or it spawned a background daemon that inherited `stdout`. Ensure the command reads `{RAW_FILE}` and exits cleanly.
- **Corrupted binary bytes when using `{VALUE}`**: `{VALUE}` converts bytes via lossy UTF-8 and strips NUL (`\0`) bytes. Always use **`{RAW_FILE}`** for binary formats.
- **Wrong rule matched**: When multiple rules match the same key name, keep `match_pattern` specific (prefix/suffix/regex) so rules do not overlap.
