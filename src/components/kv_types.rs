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
#[derive(Copy, Clone, Default, PartialEq, Eq, Debug)]
pub enum KvTableColumnType {
    /// Standard value column displaying data
    #[default]
    Value,
    /// Row index/number column
    Index,
    /// Multi-select checkbox column. Only present when the data type can
    /// delete a whole selection in one command — see
    /// `ZedisKvFetcher::supports_batch_remove`.
    Select,
}

/// Configuration for a table column including name, width, and alignment.
#[derive(Clone, Default, Debug)]
pub struct KvTableColumn {
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
    /// Whether this column is optional in the add/edit form — i.e. NOT marked
    /// required even when the fetcher requires fields (e.g. per-field TTL).
    pub optional: bool,
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
    /// Mark this column optional so its form field isn't forced required even
    /// when the fetcher requires fields (e.g. the per-field TTL column).
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }
}

/// Row number column header.
pub const INDEX_COLUMN_HEADER: &str = "#";
/// Header of the multi-select column. Blank on purpose — the header cell
/// renders a "select every loaded row" checkbox, and a caption next to it
/// would only compete with it inside a 44px column.
pub const SELECT_COLUMN_HEADER: &str = "";

/// The columns the table delegate sees: the caller's value columns with the
/// row number in front, and the multi-select box in front of *that*.
///
/// The order is a contract, not a preference. Fetchers address their own
/// columns as if only the index column existed (`col_ix == 2` is the second
/// value, hash's TTL is 3), so every column added ahead of it has to be
/// subtracted back out before a fetcher is asked anything — see
/// `ZedisKvDelegate::fetcher_col`. Checkboxes go outside the row number so
/// the number stays where the eye already looks for it.
pub fn with_leading_columns(mut columns: Vec<KvTableColumn>, selectable: bool) -> Vec<KvTableColumn> {
    columns.insert(
        0,
        KvTableColumn {
            column_type: KvTableColumnType::Index,
            name: INDEX_COLUMN_HEADER.to_string().into(),
            width: Some(80.),
            align: Some(TextAlign::Right),
            ..Default::default()
        },
    );
    if selectable {
        columns.insert(
            0,
            KvTableColumn {
                column_type: KvTableColumnType::Select,
                name: SELECT_COLUMN_HEADER.to_string().into(),
                width: Some(44.),
                align: Some(TextAlign::Center),
                ..Default::default()
            },
        );
    }
    columns
}

/// How many columns the multi-select box adds in front of the ones a fetcher
/// knows about: 1 when the table has one, 0 otherwise.
pub fn select_offset(columns: &[KvTableColumn]) -> usize {
    usize::from(
        columns
            .iter()
            .any(|column| column.column_type == KvTableColumnType::Select),
    )
}

#[cfg(test)]
mod tests {
    use super::{KvTableColumn, KvTableColumnType, select_offset, with_leading_columns};

    fn value_columns() -> Vec<KvTableColumn> {
        vec![KvTableColumn::new("Field", None), KvTableColumn::new("Value", None)]
    }

    #[test]
    fn without_selection_the_index_column_leads() {
        let columns = with_leading_columns(value_columns(), false);
        let types: Vec<KvTableColumnType> = columns.iter().map(|c| c.column_type).collect();
        assert_eq!(
            types,
            vec![
                KvTableColumnType::Index,
                KvTableColumnType::Value,
                KvTableColumnType::Value
            ]
        );
        assert_eq!(select_offset(&columns), 0);
    }

    #[test]
    fn the_checkbox_sits_outside_the_row_number() {
        let columns = with_leading_columns(value_columns(), true);
        let types: Vec<KvTableColumnType> = columns.iter().map(|c| c.column_type).collect();
        assert_eq!(
            types,
            vec![
                KvTableColumnType::Select,
                KvTableColumnType::Index,
                KvTableColumnType::Value,
                KvTableColumnType::Value
            ]
        );
        assert_eq!(select_offset(&columns), 1);
    }

    /// The contract fetchers rely on: subtracting `select_offset` from a
    /// delegate index yields the index they were written against, which is
    /// the layout with the row number and nothing else in front.
    #[test]
    fn subtracting_the_offset_restores_the_fetchers_own_numbering() {
        let plain = with_leading_columns(value_columns(), false);
        let selectable = with_leading_columns(value_columns(), true);
        for value_ix in 0..2 {
            let in_plain = 1 + value_ix;
            let in_selectable = 2 + value_ix;
            assert_eq!(plain[in_plain].column_type, KvTableColumnType::Value);
            assert_eq!(selectable[in_selectable].column_type, KvTableColumnType::Value);
            assert_eq!(in_selectable - select_offset(&selectable), in_plain);
        }
    }
}
