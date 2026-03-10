use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use reqwest::blocking::get;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::Path;
use tar::Archive;
#[derive(Debug, Serialize, Deserialize)]
pub struct RedisCommand {
    pub summary: String,
    pub complexity: Option<String>,
    pub group: String,
    pub since: String,
    pub arity: i32,
    pub function: Option<String>,
    pub container: Option<String>,
}
pub type RedisCommands = BTreeMap<String, RedisCommand>;

fn main() -> Result<()> {
    println!("🚀 get redis source code from GitHub...");

    let url = "https://github.com/redis/redis/archive/refs/tags/8.6.1.tar.gz";
    let response = get(url).context("网络请求失败，请检查网络连接")?;

    if !response.status().is_success() {
        anyhow::bail!("下载失败，HTTP 状态码: {}", response.status());
    }

    println!("📦 download completed, extracting JSON...");

    let tar = GzDecoder::new(response);
    let mut archive = Archive::new(tar);

    let mut final_commands: BTreeMap<String, RedisCommand> = BTreeMap::new();

    for item in archive.entries().context("read archive failed")? {
        let mut file = item?;

        let path_str = file.path()?.to_string_lossy().into_owned();

        if path_str.contains("src/commands/") && path_str.ends_with(".json") {
            let mut file_content = String::new();
            file.read_to_string(&mut file_content)?;

            let parsed: Result<BTreeMap<String, RedisCommand>, _> = serde_json::from_str(&file_content);

            match parsed {
                Ok(commands_map) => {
                    for (raw_key, command_data) in commands_map.into_iter() {
                        let full_command_name = match &command_data.container {
                            Some(container_name) => format!("{} {}", container_name, raw_key),
                            None => raw_key.clone(),
                        };
                        final_commands.insert(full_command_name, command_data);
                    }
                }
                Err(e) => {
                    println!("⚠️ warning: skip {} - {}", path_str, e);
                }
            }
        }
    }

    let output_file = Path::new("assets/commands.json");
    let final_json = serde_json::to_string_pretty(&final_commands)?;
    fs::write(output_file, final_json).context("write output file failed")?;

    println!("✅ build completed, merged {} commands.", final_commands.len());
    println!("📄 output file: {:?}", output_file);

    Ok(())
}
