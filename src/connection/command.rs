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

use crate::assets::Assets;
use gpui::SharedString;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::fmt;
use std::sync::OnceLock;

// Top-level Map: Key is the command name (e.g., "SET"), Value is the command details.
type CommandsMap = HashMap<SharedString, Command>;

static COMMANDS_MAP: OnceLock<CommandsMap> = OnceLock::new();

pub fn list_commands(version: &str) -> Vec<SharedString> {
    let version: Version = version.into();
    get_commands()
        .iter()
        .filter(|(_, command)| {
            let Some(since) = command.since else {
                return true;
            };
            since.le(&version)
        })
        .map(|(name, _)| name.clone())
        .collect()
}

pub fn get_command_description(name: &str) -> Option<(SharedString, SharedString)> {
    let commands = get_commands();
    let command = commands.get(name)?;
    Some((
        command.summary.clone().unwrap_or_default(),
        command.generate_syntax(name).into(),
    ))
}

fn get_commands() -> &'static CommandsMap {
    COMMANDS_MAP.get_or_init(|| {
        let Some(data) = Assets::get("commands.json") else {
            return HashMap::new();
        };
        let Ok(commands) = serde_json::from_slice(&data.data) else {
            return HashMap::new();
        };
        commands
    })
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct Command {
    summary: Option<SharedString>,
    since: Option<Version>,
    group: Option<String>,
    complexity: Option<String>,
    #[serde(default)] // Default to an empty Vec if the arguments field is missing.
    arguments: Vec<Argument>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct Argument {
    // Argument name, e.g., "key", "seconds".
    // Note: Some types (like pure-token) might not have a name, or act as placeholders like "oneof".
    name: Option<String>,

    // Argument type, maps to "type" in JSON.
    #[serde(rename = "type")]
    arg_type: ArgType,

    // Whether the argument is optional.
    #[serde(default)]
    optional: bool,

    // Whether the argument can be repeated (e.g., MSET key value [key value ...]).
    #[serde(default)]
    multiple: bool,

    // Specific token keyword, e.g., "EX", "NX".
    token: Option<String>,

    // Recursive: Sub-arguments list (used for blocks or oneof).
    #[serde(default)]
    arguments: Vec<Argument>,
}

// Use Enum to strictly limit types, which is safer than String.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(rename_all = "kebab-case")] // Automatically map "pure-token" in JSON to PureToken.
enum ArgType {
    String,
    Integer,
    Double,
    Key,
    Pattern,
    UnixTime,
    PureToken, // Pure keyword, e.g., "GET".
    Oneof,     // Mutually exclusive options, e.g., NX | XX.
    Block,     // Composition block, e.g., [EX seconds].
    #[serde(other)] // Handle unknown future types to prevent deserialization errors.
    Unknown,
}

impl Command {
    // Generate the complete syntax string for the command.
    fn generate_syntax(&self, command_name: &str) -> String {
        let mut parts = vec![command_name.to_uppercase()];
        for arg in &self.arguments {
            parts.push(arg.to_syntax_string());
        }
        parts.join(" ")
    }
}

impl Argument {
    // Recursively generate the string representation of the argument.
    fn to_syntax_string(&self) -> String {
        let inner_text = match self.arg_type {
            // 1. Mutually exclusive options (OneOf): A | B
            ArgType::Oneof => self
                .arguments
                .iter()
                .map(|arg| arg.to_syntax_string())
                .collect::<Vec<_>>()
                .join(" | "),

            // 2. Pure keyword (PureToken): e.g., "GET"
            ArgType::PureToken => self.token.clone().unwrap_or_else(|| "TOKEN".to_string()).to_uppercase(),

            // 3. Composition Block or others: Recursively display sub-content.
            // Often, blocks also have tokens, e.g., "EX seconds".
            _ => {
                let mut chunk = Vec::with_capacity(self.arguments.len() + 2);

                // If there is a Token prefix (e.g., "EX" in "EX seconds").
                if let Some(ref t) = self.token {
                    chunk.push(t.to_uppercase());
                }

                // If a name exists and it's not a pure-token, display the name.
                if let Some(ref n) = self.name {
                    chunk.push(n.clone());
                }

                // If there are sub-arguments (inside a Block), process recursively.
                for sub_arg in &self.arguments {
                    chunk.push(sub_arg.to_syntax_string());
                }

                chunk.join(" ")
            }
        };

        // Handle modifiers
        let mut result = inner_text;

        // If optional, wrap in brackets [].
        if self.optional {
            result = format!("[{}]", result);
        }

        // If multiple (repeatable), append ellipsis ...
        if self.multiple {
            result = format!("{} ...", result);
        }

        result
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    major: u32,
    minor: u32,
    patch: u32,
}

impl Version {
    fn le(&self, other: &Version) -> bool {
        self.major < other.major
            || (self.major == other.major && self.minor < other.minor)
            || (self.major == other.major && self.minor == other.minor && self.patch <= other.patch)
    }
}

impl From<&str> for Version {
    fn from(value: &str) -> Self {
        let mut parts = value.split('.').filter_map(|p| p.parse::<u32>().ok());
        Self {
            major: parts.next().unwrap_or_default(),
            minor: parts.next().unwrap_or_default(),
            patch: parts.next().unwrap_or_default(),
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl Serialize for Version {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s: String = Deserialize::deserialize(deserializer)?;
        Ok(s.as_str().into())
    }
}
