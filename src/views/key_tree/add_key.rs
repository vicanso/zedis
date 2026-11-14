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

//! Add Key dialog and argument builders for native and module key types
//! (`TS.CREATE`, `BF.RESERVE`, `CF.RESERVE`).

use super::*;
use crate::connection::{BfReserve, CfReserve, TS_DUPLICATE_POLICIES, TsCreate};
use crate::states::ModuleKeyCreateSpec;

/// The Add Key dialog's value field, in lines: how tall it opens, and how
/// tall it grows before it scrolls.
const ADD_KEY_VALUE_ROWS: (usize, usize) = (3, 10);

const ADD_KEY_STATIC_FIELDS: &[&str] = &[
    "category",
    "key",
    "ttl",
    "value",
    "score",
    "member",
    "hash_field",
    "hash_value",
    "hash_field_ttl",
    "stream_id",
    "json_value",
    "ts_retention_ms",
    "ts_duplicate_policy",
    "bf_error_rate",
    "bf_capacity",
    "bf_nonscaling",
    "cf_capacity",
    "cf_bucket_size",
];

impl ZedisKeyTree {
    pub(super) fn handle_add_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let separator = self.server_state.read(cx).key_separator().to_string();
        let prefix: Option<SharedString> = if let Some(key) = self.server_state.read(cx).key()
            && let Some((prefix, _)) = key.rsplit_once(&separator)
        {
            Some(format!("{prefix}{separator}").into())
        } else {
            None
        };
        let (supports_rejson, supports_field_ttl, supports_ts, supports_bf) = {
            let s = self.server_state.read(cx);
            (
                s.supports_rejson(),
                s.supports_hash_field_ttl(),
                s.supports_timeseries(),
                s.supports_bf(),
            )
        };
        let mut category_list = vec!["String", "List", "Set", "Zset", "Hash", "Stream"];
        if supports_rejson {
            category_list.push("Json");
        }
        if supports_ts {
            category_list.push("TimeSeries");
        }
        if supports_bf {
            category_list.push("BloomFilter");
            category_list.push("CuckooFilter");
        }
        let json_index = category_list.iter().position(|&s| s == "Json");
        let ts_index = category_list.iter().position(|&s| s == "TimeSeries");
        let bf_index = category_list.iter().position(|&s| s == "BloomFilter");
        let cf_index = category_list.iter().position(|&s| s == "CuckooFilter");

        let ttl_invalid = i18n_common(cx, "ttl_invalid");
        let field_ttl_invalid = i18n_kv_table(cx, "field_ttl_invalid");
        let key_too_long = i18n_key_tree(cx, "key_too_long");
        let bf_rate_err = i18n_key_tree(cx, "bf_error_rate");
        let bf_cap_err = i18n_key_tree(cx, "bf_capacity");
        let cf_cap_err = i18n_key_tree(cx, "cf_capacity");
        let cf_bucket_err = i18n_key_tree(cx, "cf_bucket_size");
        let ts_ret_err = i18n_key_tree(cx, "ts_retention_ms");

        let mut fields = vec![
            ZedisFormField::new("category", i18n_key_tree(cx, "category"))
                .field_type(ZedisFormFieldType::RadioGroup)
                .options(category_list.iter().map(|s| s.to_string().into()).collect()),
            ZedisFormField::new("key", i18n_common(cx, "key"))
                .placeholder(i18n_common(cx, "key_placeholder"))
                .required()
                .when_some(prefix, |this, prefix| this.default_value(prefix))
                .focus()
                .validate(move |s| {
                    if validate_long_string(s) {
                        None
                    } else {
                        Some(key_too_long.clone())
                    }
                }),
            ZedisFormField::new("ttl", i18n_common(cx, "ttl"))
                .placeholder(i18n_common(cx, "ttl_placeholder"))
                .validate(move |s| {
                    if s.trim().is_empty() || ttl_secs(s).is_some() {
                        None
                    } else {
                        Some(ttl_invalid.clone())
                    }
                }),
            ZedisFormField::new("value", i18n_common(cx, "value"))
                .placeholder(i18n_common(cx, "value_placeholder"))
                .field_type(ZedisFormFieldType::AutoGrow(ADD_KEY_VALUE_ROWS.0, ADD_KEY_VALUE_ROWS.1))
                .required()
                .visible_on("category", &[0, 1, 2]),
            ZedisFormField::new("score", i18n_common(cx, "score"))
                .placeholder(i18n_common(cx, "score_placeholder"))
                .paste_pair_with("member")
                .visible_on("category", &[3]),
            ZedisFormField::new("member", i18n_common(cx, "member"))
                .placeholder(i18n_common(cx, "member_placeholder"))
                .required()
                .visible_on("category", &[3]),
            ZedisFormField::new("hash_field", i18n_common(cx, "field"))
                .placeholder(i18n_common(cx, "field_placeholder"))
                .paste_pair_with("hash_value")
                .required()
                .visible_on("category", &[4]),
            ZedisFormField::new("hash_value", i18n_common(cx, "value"))
                .placeholder(i18n_common(cx, "value_placeholder"))
                .field_type(ZedisFormFieldType::AutoGrow(ADD_KEY_VALUE_ROWS.0, ADD_KEY_VALUE_ROWS.1))
                .required()
                .visible_on("category", &[4]),
            ZedisFormField::new("stream_id", i18n_common(cx, "stream_id"))
                .placeholder(i18n_common(cx, "stream_id_placeholder"))
                .visible_on("category", &[5]),
            ZedisFormField::new("json_value", i18n_common(cx, "value"))
                .placeholder("{}")
                .default_value("{}")
                .required()
                .field_type(ZedisFormFieldType::Editor)
                .h(px(90.))
                .visible_on("category", &json_index.map_or(vec![], |i| vec![i])),
        ];
        if supports_field_ttl {
            fields.push(
                ZedisFormField::new("hash_field_ttl", i18n_kv_table(cx, "field_ttl"))
                    .placeholder(i18n_kv_table(cx, "field_ttl_placeholder"))
                    .validate(move |s| {
                        if s.trim().is_empty() || ttl_secs(s).is_some() {
                            None
                        } else {
                            Some(field_ttl_invalid.clone())
                        }
                    })
                    .visible_on("category", &[4]),
            );
        }
        if let Some(ts_idx) = ts_index {
            let policies: Vec<SharedString> = TS_DUPLICATE_POLICIES.iter().map(|p| (*p).into()).collect();
            fields.push(
                ZedisFormField::new("ts_retention_ms", i18n_key_tree(cx, "ts_retention_ms"))
                    .default_value("0")
                    .placeholder("0")
                    .validate(move |s| {
                        let t = s.trim();
                        if t.is_empty() || t.parse::<u64>().is_ok() {
                            None
                        } else {
                            Some(ts_ret_err.clone())
                        }
                    })
                    .visible_on("category", &[ts_idx]),
            );
            fields.push(
                ZedisFormField::new("ts_duplicate_policy", i18n_key_tree(cx, "ts_duplicate_policy"))
                    .field_type(ZedisFormFieldType::RadioGroup)
                    .options(policies)
                    .default_value("0")
                    .visible_on("category", &[ts_idx]),
            );
        }
        if let Some(bf_idx) = bf_index {
            fields.push(
                ZedisFormField::new("bf_error_rate", i18n_key_tree(cx, "bf_error_rate"))
                    .default_value("0.01")
                    .required()
                    .validate(move |s| {
                        if s.trim()
                            .parse::<f64>()
                            .is_ok_and(|r| r.is_finite() && r > 0.0 && r < 1.0)
                        {
                            None
                        } else {
                            Some(bf_rate_err.clone())
                        }
                    })
                    .visible_on("category", &[bf_idx]),
            );
            fields.push(
                ZedisFormField::new("bf_capacity", i18n_key_tree(cx, "bf_capacity"))
                    .default_value("1000")
                    .required()
                    .validate(move |s| {
                        if s.trim().parse::<u64>().is_ok_and(|c| c > 0) {
                            None
                        } else {
                            Some(bf_cap_err.clone())
                        }
                    })
                    .visible_on("category", &[bf_idx]),
            );
            fields.push(
                ZedisFormField::new("bf_nonscaling", i18n_key_tree(cx, "bf_nonscaling"))
                    .field_type(ZedisFormFieldType::Checkbox)
                    .visible_on("category", &[bf_idx]),
            );
        }
        if let Some(cf_idx) = cf_index {
            fields.push(
                ZedisFormField::new("cf_capacity", i18n_key_tree(cx, "cf_capacity"))
                    .default_value("1024")
                    .required()
                    .validate(move |s| {
                        if s.trim().parse::<u64>().is_ok_and(|c| c > 0) {
                            None
                        } else {
                            Some(cf_cap_err.clone())
                        }
                    })
                    .visible_on("category", &[cf_idx]),
            );
            fields.push(
                ZedisFormField::new("cf_bucket_size", i18n_key_tree(cx, "cf_bucket_size"))
                    .placeholder("2")
                    .validate(move |s| {
                        let t = s.trim();
                        if t.is_empty() || t.parse::<u32>().is_ok_and(|b| b > 0) {
                            None
                        } else {
                            Some(cf_bucket_err.clone())
                        }
                    })
                    .visible_on("category", &[cf_idx]),
            );
        }

        let server_state = self.server_state.clone();
        let field_placeholder = i18n_common(cx, "field_placeholder");
        let value_placeholder = i18n_common(cx, "value_placeholder");
        let member_placeholder = i18n_common(cx, "member_placeholder");
        let score_placeholder = i18n_common(cx, "score_placeholder");

        let mut form_opts = ZedisFormOptions::new(fields)
            .title(i18n_key_tree(cx, "add_key_title"))
            .confirm_label(i18n_common(cx, "confirm"))
            .cancel_label(i18n_common(cx, "cancel"))
            .support_add_fields_mode_on(
                "category",
                1,
                ZedisFormAddFieldsMode::single(value_placeholder.clone(), 0),
            )
            .support_add_fields_mode_on(
                "category",
                2,
                ZedisFormAddFieldsMode::single(member_placeholder.clone(), 0),
            )
            .support_add_fields_mode_on(
                "category",
                3,
                ZedisFormAddFieldsMode::pair(score_placeholder, member_placeholder, 0),
            )
            .support_add_fields_mode_on(
                "category",
                4,
                ZedisFormAddFieldsMode::pair(field_placeholder.clone(), value_placeholder.clone(), 0),
            )
            .support_add_fields_mode_on(
                "category",
                5,
                ZedisFormAddFieldsMode::pair(field_placeholder.clone(), value_placeholder.clone(), 1),
            );
        if let Some(ts_idx) = ts_index {
            form_opts = form_opts.support_add_fields_mode_on(
                "category",
                ts_idx,
                ZedisFormAddFieldsMode::pair(field_placeholder.clone(), value_placeholder.clone(), 0),
            );
        }
        form_opts
            .add_field_placeholder(field_placeholder)
            .add_value_placeholder(value_placeholder)
            .on_dialog_submit(move |values, _window, cx| {
                let category_index = values
                    .get("category")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(0);
                let category = category_list.get(category_index).copied().unwrap_or_default();
                let key = values.get("key").cloned().unwrap_or_default();
                let ttl = values.get("ttl").cloned().unwrap_or_default();

                if let Some(module_spec) = build_module_create_spec(category, &values) {
                    server_state.update(cx, |this, cx| {
                        this.add_module_key(key, ttl, module_spec, cx);
                    });
                    return true;
                }

                let key_type = KeyType::from(category.to_lowercase().as_str());
                let args = build_add_key_args(key_type, &values);
                let field_ttl_secs = (key_type == KeyType::Hash)
                    .then(|| values.get("hash_field_ttl").and_then(|s| ttl_secs(s)))
                    .flatten();

                server_state.update(cx, |this, cx| {
                    this.add_key_with_field_ttl(category.to_string().into(), key, ttl, args, field_ttl_secs, cx);
                });
                true
            })
            .dialog_width(px(520.))
            .open_dialog(window, cx);

        let entity_id = cx.entity_id();
        cx.defer(move |cx| {
            cx.notify(entity_id);
        });
    }
}

pub(super) fn build_module_create_spec(
    category: &str,
    values: &IndexMap<SharedString, SharedString>,
) -> Option<ModuleKeyCreateSpec> {
    match category {
        "TimeSeries" => {
            let retention_ms = values
                .get("ts_retention_ms")
                .and_then(|s| s.trim().parse::<i64>().ok())
                .filter(|r| *r > 0);
            let policy_idx = values
                .get("ts_duplicate_policy")
                .and_then(|s| s.trim().parse::<usize>().ok())
                .unwrap_or(0);
            let duplicate_policy = TS_DUPLICATE_POLICIES.get(policy_idx).map(|p| (*p).to_string());
            let labels: Vec<(String, String)> = extra_pair_rows(values, ADD_KEY_STATIC_FIELDS)
                .into_iter()
                .filter(|(k, _)| !k.trim().is_empty())
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            Some(ModuleKeyCreateSpec::TimeSeries(TsCreate {
                retention_ms,
                duplicate_policy,
                labels,
            }))
        }
        "BloomFilter" => {
            let error_rate = values
                .get("bf_error_rate")
                .and_then(|s| s.trim().parse::<f64>().ok())
                .unwrap_or(0.01);
            let capacity = values
                .get("bf_capacity")
                .and_then(|s| s.trim().parse::<u64>().ok())
                .unwrap_or(1_000);
            let nonscaling = values.get("bf_nonscaling").is_some_and(|s| s == "true" || s == "1");
            Some(ModuleKeyCreateSpec::BloomFilter(BfReserve {
                error_rate,
                capacity,
                nonscaling,
            }))
        }
        "CuckooFilter" => {
            let capacity = values
                .get("cf_capacity")
                .and_then(|s| s.trim().parse::<u64>().ok())
                .unwrap_or(1_024);
            let bucket_size = values
                .get("cf_bucket_size")
                .and_then(|s| s.trim().parse::<u32>().ok())
                .filter(|b| *b > 0);
            Some(ModuleKeyCreateSpec::CuckooFilter(CfReserve { capacity, bucket_size }))
        }
        _ => None,
    }
}

pub(super) fn build_add_key_args(
    key_type: KeyType,
    values: &IndexMap<SharedString, SharedString>,
) -> Vec<SharedString> {
    let seed = key_type.seed_args();
    let get_or_seed = |name: &str, index: usize| -> SharedString {
        values
            .get(name)
            .filter(|s| !s.is_empty())
            .cloned()
            .unwrap_or_else(|| seed.get(index).unwrap_or(&"").to_string().into())
    };
    match key_type {
        KeyType::String => vec![get_or_seed("value", 0)],
        KeyType::List | KeyType::Set => {
            let mut args = vec![get_or_seed("value", 0)];
            for extra in extra_single_rows(values) {
                if !extra.is_empty() {
                    args.push(extra);
                }
            }
            args
        }
        KeyType::Zset => {
            let mut args = vec![get_or_seed("score", 0), get_or_seed("member", 1)];
            for (score, member) in extra_pair_rows(values, ADD_KEY_STATIC_FIELDS) {
                if !member.is_empty() {
                    let score_arg = if score.trim().is_empty() {
                        "0".into()
                    } else {
                        score.trim().to_string().into()
                    };
                    args.push(score_arg);
                    args.push(member);
                }
            }
            args
        }
        KeyType::Hash => {
            let mut args = vec![get_or_seed("hash_field", 0), get_or_seed("hash_value", 1)];
            for (field, value) in extra_pair_rows(values, ADD_KEY_STATIC_FIELDS) {
                if !field.is_empty() {
                    args.push(field);
                    args.push(value);
                }
            }
            args
        }
        KeyType::Json => {
            let json_value = get_or_seed("json_value", 1);
            vec!["$".into(), json_value]
        }
        KeyType::Stream => {
            let mut args = vec![get_or_seed("stream_id", 0)];
            let mut has_dynamic = false;
            for (field, value) in extra_pair_rows(values, ADD_KEY_STATIC_FIELDS) {
                if !field.is_empty() {
                    args.push(field);
                    args.push(value);
                    has_dynamic = true;
                }
            }
            if !has_dynamic {
                args.push(seed.get(1).unwrap_or(&"field").to_string().into());
                args.push(seed.get(2).unwrap_or(&"value").to_string().into());
            }
            args
        }
        _ => seed.into_iter().map(|s| s.to_string().into()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::{build_add_key_args, build_module_create_spec};
    use crate::connection::{BfReserve, CfReserve, TsCreate};
    use crate::states::{KeyType, ModuleKeyCreateSpec};
    use gpui::SharedString;
    use indexmap::IndexMap;
    use zedis_ui::{EXTRA_PAIR_DUP_PREFIX, EXTRA_SINGLE_ROW_PREFIX};

    #[test]
    fn build_add_key_args_collects_multiple_list_and_set_elements() {
        let mut values: IndexMap<SharedString, SharedString> = IndexMap::new();
        values.insert("value".into(), "first".into());
        values.insert(format!("{EXTRA_SINGLE_ROW_PREFIX}0").into(), "second".into());
        values.insert(format!("{EXTRA_SINGLE_ROW_PREFIX}1").into(), "".into());
        values.insert(format!("{EXTRA_SINGLE_ROW_PREFIX}2").into(), "second".into());

        assert_eq!(KeyType::List.create_command(), "RPUSH");
        let list_args = build_add_key_args(KeyType::List, &values);
        assert_eq!(
            list_args,
            vec![
                SharedString::from("first"),
                SharedString::from("second"),
                SharedString::from("second"),
            ]
        );

        let set_args = build_add_key_args(KeyType::Set, &values);
        assert_eq!(
            set_args,
            vec![
                SharedString::from("first"),
                SharedString::from("second"),
                SharedString::from("second"),
            ]
        );
    }

    #[test]
    fn build_add_key_args_collects_multiple_zset_and_hash_elements() {
        let mut zset_values: IndexMap<SharedString, SharedString> = IndexMap::new();
        zset_values.insert("score".into(), "1.5".into());
        zset_values.insert("member".into(), "m1".into());
        zset_values.insert("2.5".into(), "m2".into());
        zset_values.insert(format!("{EXTRA_PAIR_DUP_PREFIX}1:2.5").into(), "m3".into());
        zset_values.insert(format!("{EXTRA_PAIR_DUP_PREFIX}2:").into(), "m4".into());

        let zset_args = build_add_key_args(KeyType::Zset, &zset_values);
        assert_eq!(
            zset_args,
            vec![
                SharedString::from("1.5"),
                SharedString::from("m1"),
                SharedString::from("2.5"),
                SharedString::from("m2"),
                SharedString::from("2.5"),
                SharedString::from("m3"),
                SharedString::from("0"),
                SharedString::from("m4"),
            ]
        );

        let mut hash_values: IndexMap<SharedString, SharedString> = IndexMap::new();
        hash_values.insert("hash_field".into(), "f1".into());
        hash_values.insert("hash_value".into(), "v1".into());
        hash_values.insert("f2".into(), "v2".into());
        hash_values.insert("f3".into(), "v3".into());

        let hash_args = build_add_key_args(KeyType::Hash, &hash_values);
        assert_eq!(
            hash_args,
            vec![
                SharedString::from("f1"),
                SharedString::from("v1"),
                SharedString::from("f2"),
                SharedString::from("v2"),
                SharedString::from("f3"),
                SharedString::from("v3"),
            ]
        );
    }

    #[test]
    fn build_module_create_spec_builds_ts_bf_and_cf() {
        let mut ts_vals: IndexMap<SharedString, SharedString> = IndexMap::new();
        ts_vals.insert("ts_retention_ms".into(), "60000".into());
        ts_vals.insert("ts_duplicate_policy".into(), "2".into());
        ts_vals.insert("host".into(), "web-1".into());
        assert_eq!(
            build_module_create_spec("TimeSeries", &ts_vals),
            Some(ModuleKeyCreateSpec::TimeSeries(TsCreate {
                retention_ms: Some(60_000),
                duplicate_policy: Some("last".to_string()),
                labels: vec![("host".to_string(), "web-1".to_string())],
            }))
        );

        let mut bf_vals: IndexMap<SharedString, SharedString> = IndexMap::new();
        bf_vals.insert("bf_error_rate".into(), "0.005".into());
        bf_vals.insert("bf_capacity".into(), "5000".into());
        bf_vals.insert("bf_nonscaling".into(), "true".into());
        assert_eq!(
            build_module_create_spec("BloomFilter", &bf_vals),
            Some(ModuleKeyCreateSpec::BloomFilter(BfReserve {
                error_rate: 0.005,
                capacity: 5_000,
                nonscaling: true,
            }))
        );

        let mut cf_vals: IndexMap<SharedString, SharedString> = IndexMap::new();
        cf_vals.insert("cf_capacity".into(), "2048".into());
        cf_vals.insert("cf_bucket_size".into(), "4".into());
        assert_eq!(
            build_module_create_spec("CuckooFilter", &cf_vals),
            Some(ModuleKeyCreateSpec::CuckooFilter(CfReserve {
                capacity: 2_048,
                bucket_size: Some(4),
            }))
        );
    }
}
