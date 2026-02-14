// Copyright 2026 Tree xie.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::{error::Error, helpers::get_or_create_config_dir};
use arc_swap::ArcSwap;
use gpui::{Action, App, AppContext};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use smol::fs;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::{fmt, fs::read_to_string, path::PathBuf, sync::LazyLock};
use tracing::{debug, error};

type Result<T, E = Error> = std::result::Result<T, E>;

fn get_or_create_session_config() -> Result<PathBuf> {
    let config_dir = get_or_create_config_dir()?;
    let path = config_dir.join("redis-sessions.toml");
    debug!(file = path.display().to_string(), "get or create server config");
    if path.exists() {
        return Ok(path);
    }
    std::fs::write(&path, "")?;
    Ok(path)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, JsonSchema, Action)]
pub enum QueryMode {
    #[default]
    All,
    Prefix,
    Exact,
}

impl fmt::Display for QueryMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            QueryMode::Prefix => "^",
            QueryMode::Exact => "=",
            _ => "*",
        };
        write!(f, "{}", s)
    }
}

impl FromStr for QueryMode {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "^" => Ok(QueryMode::Prefix),
            "=" => Ok(QueryMode::Exact),
            _ => Ok(QueryMode::All),
        }
    }
}

#[derive(Debug, Default, Deserialize, Clone, Serialize)]
pub struct SessionOption {
    pub id: String,
    pub soft_wrap: Option<bool>,
    pub query_mode: Option<String>,
    pub refresh_interval_sec: Option<u32>,
}

#[derive(Debug, Default, Deserialize, Clone, Serialize)]
struct SessionOptions {
    options: Vec<SessionOption>,
}

static SESSION_OPTION_MAP: LazyLock<ArcSwap<HashMap<String, SessionOption>>> =
    LazyLock::new(|| ArcSwap::from_pointee(HashMap::new()));

fn get_session_options() -> Result<Arc<HashMap<String, SessionOption>>> {
    if SESSION_OPTION_MAP.load().is_empty() {
        let path = get_or_create_session_config()?;
        let value = read_to_string(path)?;
        if value.is_empty() {
            return Ok(Arc::new(HashMap::new()));
        }
        let data: SessionOptions = toml::from_str(&value)?;
        let options = data.options;
        let mut configs = HashMap::new();
        for option in options.iter() {
            configs.insert(option.id.clone(), option.clone());
        }
        SESSION_OPTION_MAP.store(Arc::new(configs));
    }
    Ok(SESSION_OPTION_MAP.load().clone())
}

pub fn get_session_option(id: &str) -> Result<SessionOption> {
    let options = get_session_options()?;
    Ok(options.get(id).cloned().unwrap_or_else(|| SessionOption {
        id: id.to_string(),
        ..Default::default()
    }))
}

pub fn save_session_option(id: &str, mut option: SessionOption, cx: &App) {
    if id.is_empty() {
        return;
    }
    let id = id.to_string();
    option.id = id.clone();
    cx.spawn(async move |cx| {
        let task = cx.background_spawn(async move {
            let mut options = vec![option.clone()];
            let mut new_options = HashMap::new();
            new_options.insert(id.to_string(), option.clone());

            for (key, value) in get_session_options()?.iter() {
                if *key == id {
                    continue;
                }
                options.push(value.clone());
                new_options.insert(key.to_string(), value.clone());
            }

            SESSION_OPTION_MAP.store(Arc::new(new_options));
            let path = get_or_create_session_config()?;
            let value = toml::to_string(&SessionOptions { options })?;
            fs::write(&path, value).await?;
            Ok(())
        });
        let result: Result<()> = task.await;
        if let Err(e) = &result {
            error!(error = %e, "Failed to save session option");
        }
    })
    .detach();
}
