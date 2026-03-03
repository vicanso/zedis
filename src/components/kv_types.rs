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

use gpui::{SharedString, TextAlign};
use zedis_ui::ZedisFormFieldType;

bitflags::bitflags! {
    /// Defines the operations supported by the table.
    ///
    /// Use bitwise operations to combine multiple modes:
    /// - `KvTableMode::ADD | KvTableMode::UPDATE` - Allow add and update
    /// - `KvTableMode::ALL` - Allow all operations
    /// - `KvTableMode::empty()` - Read-only mode (no operations)
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct KvTableMode: u8 {
        /// Support adding new values
        const ADD    = 0b0001;
        /// Support updating existing values
        const UPDATE = 0b0010;
        /// Support removing values
        const REMOVE = 0b0100;
        /// Support filtering/searching values
        const FILTER = 0b1000;
        /// All operations enabled
        const ALL    = Self::ADD.bits() | Self::UPDATE.bits() | Self::REMOVE.bits() | Self::FILTER.bits();
    }
}

/// Defines the type of table column for different purposes.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub enum KvTableColumnType {
    /// Standard value column displaying data
    #[default]
    Value,
    /// Row index/number column
    Index,
}

/// Configuration for a table column including name, width, and alignment.
#[derive(Clone, Default, Debug)]
pub struct KvTableColumn {
    /// Whether the column is readonly
    pub readonly: bool,
    /// Type of the field
    pub field_type: Option<ZedisFormFieldType>,
    /// Whether the column is flexible
    pub flex: bool,
    /// Type of the column
    pub column_type: KvTableColumnType,
    /// Display name of the column
    pub name: SharedString,
    /// Optional fixed width in pixels
    pub width: Option<f32>,
    /// Text alignment (left, center, right)
    pub align: Option<TextAlign>,
    /// Whether the column is auto-created
    pub auto_created: bool,
}

impl KvTableColumn {
    /// Creates a new value column with the given name and optional width.
    pub fn new(name: &str, width: Option<f32>) -> Self {
        Self {
            name: name.to_string().into(),
            width,
            ..Default::default()
        }
    }
    pub fn new_flex(name: &str) -> Self {
        Self {
            name: name.to_string().into(),
            flex: true,
            ..Default::default()
        }
    }
    pub fn new_auto_created(name: &str) -> Self {
        Self {
            name: name.to_string().into(),
            auto_created: true,
            ..Default::default()
        }
    }
    pub fn field_type(mut self, field_type: ZedisFormFieldType) -> Self {
        self.field_type = Some(field_type);
        self
    }
}
