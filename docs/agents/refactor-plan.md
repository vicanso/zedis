# Refactor plan

Drawn up 2026-10-20 from measurements of the tree at `9fb6f80`. It is a
sequence of small, behaviour-preserving changes, one phase at a time; each
step is one change the user reviews and commits. Nothing here changes what
the app does — a step that would is a feature, and goes through the usual
route instead.

## Rules every step follows

- **Behaviour first, structure second.** Before moving code that has no test
  pinning what it does, add one (a UI integration test for a view, a unit test
  for state, a bridge route test). A new test is shown to fail with the code it
  guards taken out — in a `git worktree`, never by reverting a tracked file.
- **Moves are moves.** A step that relocates code changes no logic in the same
  diff, so the review is "the same lines, elsewhere". Renames and logic changes
  are separate steps.
- **Gates**: `make fmt`, `make lint`, `make test`, `make check-web`,
  `make lint-fast`; `make deny` when a manifest changes. Other OSes, the Redis
  matrix and smoke are CI's.
- **Desktop first** (CLAUDE.md): a change made for structure must not change
  what the desktop does; anything touching a target gate runs both
  `make lint` and `make check-web`.
- **Never commit**; each step ends with a one-line commit message offered.
- A step that turns out larger than planned stops and is split, rather than
  landing as one large diff.

## Baseline (2026-10-20)

| Area | Lines | Notes |
|---|---|---|
| `src/views` | 81.0k | 113 files; grouped below |
| `zedis-connection` | 33.1k | + `tests/live.rs` 8.4k in one file |
| `zedis-bridge` | 22.7k | `api.rs` 3.2k (2.2k without tests) |
| `src/states` | 19.6k | `ZedisServerState`: 76 fields, 282 methods (231 not private) over 16 files, 76 `ServerTask` variants |
| `zedis-core` | 12.6k | |
| `src/helpers` | 6.6k | `updater.rs` 1.6k |
| `zedis-db` | 5.7k | |
| `zedis-ui` | 3.9k | 10 tests |
| `zedis-web` | 3.1k | `transport.rs` 2.4k (1.6k without tests) |

Views grouped: key tree + value editors + terminal 25.6k, administration tools
22.1k, observability panels 19.8k, app shell 13.0k (with `root.rs` and the
entry files), web-only views 4.1k.

Functions over 450 lines (checked by hand; a brace counter's false positives
removed):

| Lines | Function |
|---|---|
| 685 | `views/key_tree/delegate.rs` `render_item` |
| 645 | `views/editor/toolbar.rs` `render_select_key` |
| 554 | `views/sidebar.rs` `render_server_list` |
| 552 | `views/key_tree/render.rs` `render_keyword_input` |
| 534 | `views/setting_editor.rs` `new` |
| 525 | `views/servers/form.rs` `add_or_update_server_dialog` |
| 522 | `views/migration_window.rs` `render` (desktop only) |
| 516 | `views/servers.rs` `render` |
| 482 | `zedis-bridge/src/main.rs` `main` |
| 482 | `views/key_tree/actions.rs` `render` (the action wiring) |
| 452 | `root.rs` `render` |

51 functions are over 200 lines by the rough count.

Target gates in `src`: 249 `cfg(not(target_family = "wasm"))` and 136
`cfg(target_family = "wasm")`, against 226 and 71 when CLAUDE.md counted on
2026-09-28 — the browser-only side nearly doubled while the shape CLAUDE.md
asks for is "the web subtracts".

Tests: `src` 373, `zedis-core` 205, `zedis-connection` 399 (unit + live),
`zedis-db` 86, `zedis-ui` 10, `zedis-bridge` 181, `zedis-web` 34.

## Phases

Ordered by risk against payoff: the mechanical splits that teach the routine
come first, the state layer — the largest payoff and the largest risk — once
the routine is proven.

### Phase 1 — the bridge's route file (low risk, mechanical)

`api.rs` held `AppState`, the router, `authorize*`, 30 handlers and 12 test
modules. It stays the parent (the repo's `foo.rs` + `foo/` shape, so every
`crate::api::…` path still resolves) and its routes moved into `api/`, moves
only — done:

- `api.rs` — `AppState`, `router` (which imports the handlers it routes),
  the login-cookie middleware, `ApiError`, `authorize` / `authorize_mcp` /
  `authorize_write`, `visible_to` / `visible_server`, `note_server`; the
  shared test fixture (`mcp_auth_tests::state` / `headers`) and the
  router-level tests (body limit, compression).
- `api/page.rs` — the page's own files: `web_asset`, the base path.
- `api/sign_in.rs` — login, logout.
- `api/status.rs` — health, ready, who is online.
- `api/entries.rs` — the server list and its edits, the write lock, diagnose,
  `assign_owner`.
- `api/commands.rs` — `/v1/exec`, sessions, `forward_values`.
- `api/watch_routes.rs` — the three `/v1/watch` handlers.
- `api/storage.rs` — the bin and the metrics history.

Each test module went with the code it tests; the routed handlers and the
types in their signatures became `pub(super)`. A line-count check over the
eight files showed nothing lost but the moved test's two fixture paths, and
`cargo test -p zedis-bridge` counts 178 passed, 10 ignored before and after.

Then `main.rs`'s 482-line `main`, now 155 — what is left is the order things
happen in — done: `print_help`, `setting` (a flag, else its environment
variable), `trusted_proxy` and `open_audit` (the two that decide something,
taking the settings as arguments so a test can give them; the reading of argv
stays in `main`), `with_audit_webhook`, `report_proxy`,
`import_configured_users`, `report_accounts`, `web_root`, and the minute's
sweep as `Housekeeping::run` (the struct holds the handles; `run`
destructures them into the names the loop always used, so its body is the
same lines). Two tests pin the decisions (`a_trusted_header_needs_the_proxies_it_is_believed_from`,
`the_audit_goes_where_the_settings_say_and_a_wrong_one_stops_the_bridge`),
each shown to fail in a worktree with its arm taken out; the line check over
the file finds nothing gone but the glue (the sweeper's clones, the closure,
the comments that became the functions' docs).

And `accounts.rs` (3.0k, 2.0k without tests), looked at before cutting, and
then cut — both done:

| Block | Lines | Leans on |
|---|---|---|
| constants, the `Signup` setting | 77–131 | — |
| stored records, `McpToken`, `parse_sha256` | 133–278 | `auth::ServerRule` |
| `Entry` (an account as held: rules parsed, token hashes) | 280–320 | records |
| views (`AccountView`, `TokenView`, `InviteView`, `SignupView`) | 322–416 | records |
| `AccountChange`, `AccountError`, `Basic`, `AccountOp` | 418–500 | records |
| `State`, `Verified`, `Accounts`, `Inner` | 503–597 | all of the above |
| password hashing, name / password / rule checks, `new_code` | 598–675 | argon2 |
| `impl Accounts` | 677–1478 | `State` through `change` |
| `Setup`, `LocalSetup`, `restrict` | 1481–1630 | `new_code`, `admin_count`, `auth::same` |
| `LocalAccounts` (`bridge-accounts.json`) | 1633–1732 | records, `AccountOp`, `restrict` |
| the `--users-file` / `ZEDIS_BRIDGE_USERS` import | 1735–1977 | records, `hash_password` |
| `MemoryAccounts`, test constructors | 1979–2075 | (`auth.rs`'s tests use `MemoryAccounts`) |
| 27 tests | 2077–3022 | |

The 800-line `impl Accounts` is one cache and several families, and every
family writes through one method: `change(decide)` decides on a `State`,
applies the ops to the store at the version it read and decides again when
another process moved it. So the families can be `impl Accounts` blocks in
child modules — a child sees the parent's private `State`, `Entry` and
`change` — while the cache stays where it is. The families: the cache
(`load`, `with`, `read`, `change`, `reload`, `refresh`, `catch_up`,
`version_held`), the queries (`has` … `mcp_token_count`), signing in
(`account_for_token`, `check_basic`, `accepts_password`), administration
(`import`, `make_admin`, `set_password`, `update`, `delete`), MCP tokens
(`issue_token`, `revoke_token`), invitations (`invite`, `invites`,
`invite_new`, `invite_revoke`, `register`, `reset_password`,
`purge_expired_invites`) and sign-up requests (`signups`, `request_signup`,
`approve_signup`, `reject_signup`, `purge_expired_signups`).

The cut, moves only, `accounts.rs` staying the parent and re-exporting what
other modules name (every `crate::accounts::…` path stays as it is; 1.2k
lines, 0.4k of them its tests):

- `accounts/records.rs` — the stored records, `AccountView` / `TokenView`,
  `AccountOp`, `AccountChange`, `AccountError`, `Basic`.
- `accounts/password.rs` — hashing, the name / password / rule checks,
  `new_code`, with its test.
- `accounts/tokens.rs` — issue, revoke and the bearer lookup, with the two
  token tests.
- `accounts/invitations.rs` — `InviteView` and the invitation family, with
  its three tests.
- `accounts/signups.rs` — the `Signup` setting, `SignupView` and the request
  family, with its four tests.
- `accounts/setup.rs` — `Setup`, `LocalSetup`, with its test.
- `accounts/local.rs` — `LocalAccounts`, `AccountsFile`, with its test.
- `accounts/users_file.rs` — the import and the test constructors that read
  its forms (`Accounts::from_toml` / `from_inline`), with its five tests.
- `accounts.rs` keeps the constants, `McpToken` and `parse_sha256` (which
  `Entry` builds), `Entry` / `State` / `Accounts`, the cache, the queries,
  signing in, administration, `restrict` (both stores use it),
  `MemoryAccounts` and the shared test helpers, with ten tests.

Kept where they were, against the first sketch, so that nothing had to be
made more visible than it was: `McpToken` (private fields `Entry` sets) and
`restrict`; the views went with their families instead of `records.rs`. What
did change visibility: `verify_password` and `new_code` (`pub(super)`, the
parent and two children call them) and the five test helpers the children's
tests share (`basic`, `refused`, `with_admin`, `invite`, `register_full`),
which the children import by name so the moved tests' bodies are unchanged.
No test needed writing first: each family already had its own (the last
administrator, a change another process made first, a locked account, the
request cap, the token expiry, the unreadable file). The line check over the
nine files finds nothing gone but the three `// ----` section lines the
children's docs replace, and the 27 tests are the same 27 by name.

Acceptance: every bridge test passes unchanged in body; `cargo test -p
zedis-bridge` counts the same tests before and after.

### Phase 2 — the longest view functions

The eleven functions above, one per step, by extracting the sections they
already draw into named methods (`render_item` → the row's chips, its
badges, its menu; `render_select_key` → name strip, fact chips, actions;
`setting_editor::new` → one builder per section). No layout change: the
screenshot before and after is the same, and the ids the UI tests find stay.

Before each: a UI integration test that finds the controls the function
draws (ids, labels, focus after the obvious interaction) if none exists.

`key_tree/actions.rs` `render` is the action wiring for the whole tree: split
by action family (delete, TTL, export, tags, navigation) into handler methods
the `on_action` closures call.

Done so far:

- `key_tree/delegate.rs` `render_item`, 685 → 206 lines (what is left is
  the row's own frame and its facts). The sections went to functions beside
  it — `load_more_row`, `row_icon`, `ttl_chip`, `depth_guides`, `row_name`
  (the label with its tooltip), `ttl_slot`, `row_hover_delete`, and
  `inline_delete_button`, the one delete button the two slots had a copy
  each of — and the right-click menu to `key_tree/row_menu.rs`: a `RowMenu`
  of the row's facts whose `build` is the four sections it already had
  (operations, export, tag and note, copy / rename / star). `id` and
  `edit_tag_id`, two clones of the same key, are its one `id`. Two UI tests
  came first: `a_rows_menu_offers_what_the_row_and_the_access_allow` (each
  item of seven menus clicked, the actions they send compared in order — a
  key, a key that never expires, a folder, two picks and one, read-only)
  and `a_key_row_offers_its_delete_on_hover` (where the inline delete is
  and is not, which hover shows it, what it sends). Each was shown to fail
  in a worktree with the original code mutated: the never-expiring key's
  Remove TTL, the TTL slot's hover scope, the delete beside the checkbox,
  the read-only folder's Refresh. Not compared: pixels — the tests read
  ids, visibility and dispatched actions.
- `editor/toolbar.rs` `render_select_key`, 645 → 64 lines: the value's
  facts read once into a `KeyFacts` (`key_facts`), the controls as methods
  (`save_button`, `ttl_control`, `reload_button`, `copy_and_favorite`), the
  name-and-chips strip as `name_and_facts`, and the "…" menu to
  `editor/more_menu.rs`: a `MoreActions` decided by `more_actions` (what is
  offered for this key, value and connection) whose `build` is the menu's
  three groups (the value's actions, its history, the key's). Three UI tests
  came first: `a_strings_bar_offers_its_controls_and_menus`,
  `a_read_only_bar_offers_what_reads` and
  `a_collection_and_a_first_load_offer_less` — which controls are there and
  which disabled, and every item of the TTL, reload and "…" menus clicked,
  in order, with the editor inside a view that captures each `EditorAction`
  and `KeyOpAction` on its way down and stops it, so nothing an item opens
  opens. Shown to fail with the original mutated: Import on a read-only
  connection, Remove TTL on a key that never expires, a reload interval
  dropped, the "…" button enabled during the first load. Not reachable from
  the tests: the name, encoding and heat strip (plain `div`s, which the test
  window does not observe) and the two history submenus' contents (their
  rows are a session's saves). The two history submenus are still two
  copies that differ in an icon, a label and an action; folding them is a
  change of its own.
- `sidebar.rs` `render_server_list`, 554 → 138 lines: what every row is
  drawn with in a `ListLook` (`list_look`: the colours, the tab gestures'
  hints, whether the sidebar is the rail), the Home band as `home_band`, a
  group's header as `group_header`, a connection as `server_row`. What has
  to run in the list's order — the next monogram of the rail, the
  connection's link, its `ServerMenu` — stays in the loop and is handed to
  the row. The file had no test; two came first:
  `the_list_draws_home_its_rows_and_closes_a_group` (the rows there are, a
  header's click closing its group, the rail showing every connection and
  no collapse control) and
  `a_row_opens_its_connection_and_its_menu_follows_the_link` (a click, ⌘
  and ⌘⇧ on a row and on Home; the menu's items in order with the link
  item for each health). The sections are set on the sidebar itself, not
  through the server list, which is the process's and shared by every test.
  Shown to fail with the original mutated: Disconnect offered offline, a
  closed group's rows drawn, ⌘⇧ not forcing a new tab, the collapse control
  in the rail.
- `key_tree/render.rs` `render_keyword_input`, 552 → 148 lines: its two
  menus to `key_tree/bar_menus.rs` — `FilterMenu`, the search box's (the
  three lists that lead back to a key, read as the menu opens; the query
  modes; the type, tag and TTL filters; the sort), one method a submenu,
  and `TreeMoreMenu`, the "…" — and the filter chips (`active_filters`),
  Clear selection and New key as functions. Three UI tests came first:
  `the_bar_offers_its_controls_and_both_menus` reads every item of both
  menus and of every submenu (a submenu opens on hover, under the
  `"submenu"` scope, and is read item by item from a menu opened afresh),
  `a_filter_is_a_chip_that_takes_it_off` (and the query modes not pickable
  with a regex keyword — turned on through the server state, since the tree
  reads it back on every rebuild), `the_bar_follows_access_picks_and_the_server`.
  Shown to fail with the original mutated: the query modes pickable under a
  regex, an auto-refresh interval dropped, New key allowed read-only, the
  tag chip missing, the keyword kept across servers.
- `setting_editor.rs` `new`, 534 → 85 lines: one builder a section in
  `setting_editor/fields.rs` (`appearance_fields`, `date_time_fields`,
  `key_behavior_fields`, `redis_fields`, `ai_fields`, `system_fields`), each
  reading its own settings, building its fields and pushing what saves
  them, and answering a struct `new` destructures into the names its
  `Self { … }` always used. Not one line of the original is gone. Two UI
  tests came first, on the wiring rather than on pixels:
  `every_field_starts_from_its_setting_and_saves_when_left` (each field set
  and left — `InputEvent::Blur` — and the setting read back, against what
  the setter itself makes of the value: the scan count clamped and shown
  clamped, thresholds under their floor not kept, an unusable proxy put
  back, the font size set right) and `each_list_saves_what_is_picked`.
  Shown to fail with the original mutated: the scan count unclamped, the
  date layout not saved, an unusable proxy left in the field, the depth
  stepping by two.
- `servers/form.rs` `add_or_update_server_dialog`, 525 → 120 lines: the
  field list cut at its runs, in its order — `address_fields`,
  `sentinel_fields` (with the master-name fetch's handler as
  `fetch_master_names`), `note_fields`, `tls_fields`, `ssh_fields`,
  `connection_fields`, `safety_fields`, `key_tree_fields` — and joined back
  in that order, so every tab lists its fields as before; the desktop's
  Test and Diagnose buttons as functions, their `cfg` moved from the
  statement to the item. The test that came first is a round trip,
  `an_entry_comes_back_from_its_form_as_it_went_in`: an entry with every
  field filled opens in the form and is saved back unchanged, but for the
  two the store stamps (`updated_at`, `sort_order`). The save writes the
  file through `unblock`, on a thread the test scheduler does not drive:
  the first version of the test passed alone and aborted `make test` ("your
  test is not deterministic") — it now calls `allow_parking` and waits for
  `ServerListUpdated`. Shown to fail with the original mutated: a field not
  filled from the entry, Diagnose missing.
- `servers.rs` `render`, 516 → 158 lines: a group's section as
  `group_section`, a card as `server_card` (218 lines — one card, kept
  whole), the toolbar as `toolbar`, what they share in a `PageLook`. The
  page reads the process's server list, so its test,
  `the_page_draws_a_card_per_connection_and_its_actions`, adds three entries
  of its own (`set_servers_cache`, the list put back on drop), narrows the
  page to them with its search box, and holds `test_support::hold_the_server_list`
  — a lock every test that changes the list now takes, since a change is a
  read, a change and a write. Shown to fail with the original mutated: the
  first card's Up enabled, a card's click not connecting, Clone offered
  read-only.
- `key_tree/actions.rs` `render`, 482 → 27 lines: its four listeners as
  functions (`on_query_mode`, `on_type_filter`, `on_tree_action` — the
  dispatch, 154 lines of one arm an action — and `on_editor_action`), and
  what an action does to keys to `key_tree/key_actions.rs`, one function an
  arm: tag and note, favorites and recent keys, delete, Remove TTL, set TTL
  and export, each for the picks, a folder or one key. The picks' read, five
  copies of the same eight lines, is `picked_keys`. Two UI tests came first:
  `the_trees_actions_set_what_they_name` (the query modes, the type and TTL
  filters, the regex switch, the selection) and
  `the_trees_actions_ask_what_they_have_to` (which question each action
  opens — none, a confirm, the TTL dialog — the picks' only when there are
  picks). The first sends no server: a selected server makes a query mode a
  write of the session file, off the scheduler's threads. Shown to fail
  with the original mutated: the TTL dialog for no picks, the Hash filter
  picking sets, the multi-key confirm inverted, the regex switch not set.
- `root.rs` `render`, 452 → 81 lines: what a frame does before it draws
  (the placement saved, notices left to say, the pending prompts, the font
  size, a tab asked for) as `prepare_frame`, and the fourteen action
  listeners as functions in `root/handlers.rs` (`on_theme` … `on_nav`;
  `on_server_tool`, 140 lines, is one arm a tool). Three UI tests came
  first: `the_roots_actions_change_what_they_name` (language, zoom, theme,
  the Protos and Scripts pages, a tool's route), `the_roots_tabs_follow_their_actions`
  (a tab built on the next frame, ⌘1, Close, ⌘W) and
  `the_root_opens_its_dialogs` (the welcome card, Esc closing a dialog
  first, the tools that are dialogs). Shown to fail with the original
  mutated: a language saved as another, ⌘1 not switching, Esc not closing
  the dialog, Monitor going to Config.
- `migration_window.rs` `render`, 522 → 56 lines: what the sections are
  drawn with read once into a `JobLook`, and the sections to
  `migration_window/sections.rs` — `header`, `status_section`,
  `export_section` (destination, format, prefix), `copy_section`,
  `import_section`, `log_section`, `footer`. The copy and the import each
  had the same dry-run block: the sample box is `sample_block`, the sample
  list `preview_sample`, and the summary, which differed only in its three
  locale keys, `preview_summary(running, summary, idle)` — the split
  script asserts the copies were the same lines before folding them. The
  window had no test; three came first:
  `an_export_goes_to_a_file_or_to_another_server` (format, prefix filter,
  destination, targets, copying onto itself, the conflict policy, a dry
  run's Cancel), `a_copy_handed_its_target_opens_on_it` and
  `an_import_asks_for_its_file_before_it_previews_or_starts` (and Esc
  closes the window). A radio group's own id is not something a test can
  find — only its radios — so "not drawn" is counted through the radios'
  paths (`radios`); the first version asked for the group and passed
  whether it was there or not, which the mutation run showed. Shown to
  fail with the original mutated: nothing to export still offered, Preview
  before a file, the copy's policy not shown, a format offered for a copy,
  Esc ignored, copying onto itself allowed, Cancel not stopping the dry
  run. Not reachable from the tests: a running or finished job (the phase
  is set by the worker only, so Reveal and the locked controls are
  unchecked) and the plain labels — title, summaries, the log.

### Phase 3 — the state layer (largest payoff, highest risk)

`ZedisServerState` is the source of truth for one connection and has grown to
76 fields and 282 methods (231 of them not private) over 16 files.
Analysis first, as a written map, then steps:

1. Map every field to the methods that read and write it, and group them:
   the selected key and its value, the key list and scan, link health and
   reconnects, server features and probes, the write lock, report findings,
   metrics and the heartbeat, terminal / channel mode. The map goes into this
   file before any code moves.
2. For each group with few cross-links, extract a sub-struct owned by
   `ZedisServerState` (the `DesktopOnly` pattern in `root/desktop.rs`), its
   methods moved onto it; public methods first delegate, so call sites are a
   later, separate step.
3. `ServerTask` (76 variants) is looked at with the map: whether a variant per
   operation still earns its place, or the task identity can be grouped.

Acceptance per step: the state's tests and the views' UI tests pass
unchanged; events (`ServerEvent`) fire in the same order for the same action
— where a test does not already show that, one is added first.

#### The field map (step 1, 2026-10-20)

Found by the compiler, not by a pattern: in a copy, every field was renamed
and `cargo check --lib --tests` run, so each "no field" error is one access,
whatever the receiver is called (`self`, a callback's `this` or `state`).
722 accesses: 653 in methods, 69 in tests, none anywhere else. The fields are
private, so that is all of them. 72 methods touch no field and only call
other methods. The 323 counted earlier included the other types' methods in
the same files.

The groups, with `server_id` and `db` left out as the identity nearly every
method reads (54 and 14 methods). "Alone" counts the methods that touch this
group's fields and no other group's; "with others" leaves out `select` and
`reset`, which touch every group because they start and end a session.

| Group | Fields | Methods | Alone | With others |
|---|---|---|---|---|
| lock — the write lock and access mode | 3 | 11 | 9 | none |
| prefs — the tree and editor settings resolved for this connection | 6 | 9 | 7 | `sync_tree_ttl` (scan) |
| panels — what panels left for the session | 3 | 10 | 6 | `cluster_reshard`, `cluster_rebalance` (link) |
| terminal | 1 | 5 | 2 | `change_channel_mode`, `select_key` (value, scan) |
| metrics — what the heartbeat and DBSIZE brought | 10 | 18 | 12 | `refresh_redis_info` (link, facts), `refresh_dbsize_now` (link), `reload_topology` (facts), `flush_database` (scan, value) |
| link — health, heartbeat pacing, load retries | 16 | 24 | 17 | `refresh_redis_info` (metrics, facts), `refresh_dbsize_now` (metrics), `begin_task` (scan), the two cluster tasks (panels) |
| facts — what a connect learned | 8 | 25 | 17 | `refresh_redis_info` (metrics, link), `reload_topology` (metrics), `get_value`, `scan_keys` (scan, value), `handle_auto_refresh` (scan), `refresh_value_header` (value) |
| scan — the loaded keys and the scan | 19 | 49 | 36 | 11, below |
| value — the open key, its value, its history | 7 | 72 | 61 | 10, below |
| errors | 1 | 1 | 1 | none |

The fields of each:

- **lock**: `access_mode`, `write_unlocked_until`, `unlock_task`.
- **prefs**: `key_separator`, `key_scan_count`, `max_key_tree_depth`,
  `auto_expand_threshold`, `show_key_tree_ttl`, `soft_wrap`.
- **panels**: `report_findings`, `info_snapshot`, `reshard_progress`.
- **terminal**: `terminal`.
- **metrics**: `redis_info`, `dbsize`, `last_dbsize_refreshed_at`,
  `dbsize_refresh_inflight`, `dbsize_refresh_again`,
  `last_slow_logs_checked_at`, `last_slow_log_count`, `slow_logs`,
  `slow_logs_generation`, `client_pause`.
- **link**: `server_status`, `connection_health`, `last_connection_error`,
  `ping_failures`, `manually_offline`, `link_failure`, `last_offline_notice`,
  `last_link_notice`, `background`, `last_background_refresh`,
  `heartbeat_retry_at`, `last_heartbeat_at`, `heartbeat_in_flight`,
  `load_retry_pending`, `load_retries`, `load_retry_running`.
- **facts**: `version`, `nodes`, `nodes_description`, `databases`,
  `supports_rejson`, `supports_search`, `features`, `sentinel_masters`.
- **scan**: `keyword`, `cursors`, `scanning`, `scan_completed`,
  `scan_times`, `scan_epoch`, `key_tree_id`, `loaded_prefixes`,
  `scanning_prefixes`, `complete_before_refresh`, `incomplete_prefixes`,
  `keys`, `listed_keys`, `key_ttls`, `keys_superseded_by_next_batch`,
  `type_filter`, `regex_keyword`, `query_mode`, `search_history`.
- **value**: `key`, `value`, `value_epoch`, `size_gate_bypassed`,
  `size_gate_preview`, `size_gate_pending`, `histories`.
- **errors**: `error_messages`.

Where the groups meet, field by field (a `*` is a write):

- scan and value meet in the key operations that change the tree and the
  open key together: `select_key` (`keys*`, `key*`, `value*`, `histories`,
  `terminal*`), `rename_key`, `forget_deleted_keys`, `batch_set_ttl_keys`,
  `batch_set_ttl_folder` (`key`, `key_tree_id*`, `key_ttls*`),
  `flush_database` (both, and `dbsize*`), `get_value` and `scan_keys` (both,
  and `features` / `nodes`).
- `handle_auto_refresh` reads `nodes` for a scan; `refresh_value_header`
  reads `features` for the value's header.
- `begin_task` sets `scanning` beside `server_status`.
- `refresh_redis_info`, the heartbeat, is the one method across link,
  metrics and facts: the pacing fields, `redis_info*`, `dbsize*`, the slow
  log, and `nodes`.
- `refresh_dbsize_now` and the cluster reshard / rebalance read
  `manually_offline` before they go out.
- `sync_tree_ttl` reads `show_key_tree_ttl` to decide whether to fill
  `key_ttls`.

Through calls rather than fields: `can` (facts) asks `readonly` (lock);
`toggle_readonly` (lock) and `settle_task` (link) add to `error_messages`;
`note_link_error` (link) reads `nodes_description`; `note_ping_result`
(link) runs `on_link_restored` (value); `resume_heartbeat`,
`kill_running_script` and `unpause_clients` run `refresh_redis_info`.

What `reset` keeps, which a sub-struct has to keep too: it does not touch
`db`, `databases`, `search_history`, the six prefs, `access_mode`,
`supports_rejson`, `supports_search`, `features`, `query_mode`, `soft_wrap`,
`server_status`, `load_retries`, `load_retry_running`, `last_offline_notice`,
`last_link_notice`, `background`, `last_background_refresh` and
`error_messages`. `select` sets all but the last five again — `databases`,
`access_mode` and the two `supports_*` only once its connect lands,
`search_history` only when the history can be read. So a group's reset is
not its `Default`: `lock` keeps `access_mode`, `link` seven of its sixteen
fields, `facts` four of its eight, and `prefs` is not reset at all.
`clear_if_removed` resets without a `select` after it, so there the kept
values stay as the removed server left them.

What this means for step 2, in order:

1. **lock** first: three fields, nothing else touches them, nine of its
   eleven methods are its own. Four are plain reads (`readonly`,
   `strict_readonly`, `unlock_remaining`, `write_gate`); the five that
   notify, spawn or time out stay on the state and call into it.
2. **prefs**: six fields `select` resolves from the entry and Settings and
   nothing but `sync_tree_ttl` reads outside their getters. The resolving
   moves with them.
3. **panels** and **terminal**: plain getters and setters; small, low value,
   done when they are in the way.
4. **metrics**, then **link**: most of their own methods are plain
   (`finish_dbsize_refresh`, `request_dbsize_refresh`, `take_load_retry`,
   `link_problem`, `note_link_failure`, `set_background`); the heartbeat,
   `refresh_redis_info`, stays on the state and reaches into both.
5. **facts**: the feature matrix (`can`, `blocked_by`, `command_block`,
   `panel_block`) is the core of it and plain.
6. **scan** and **value** last: the largest, and eight key operations change
   both. They are worth splitting only together with `key.rs` (2.6k lines),
   which is where all eight live.

#### Steps 2 and 3, done (2026-10-20)

The state is its identity (`server_id`, `db`), seven sub-structs — each in a
module of `server/` — and five fields left as they were:

| Field | Type, module | What moved onto it | Fields private |
|---|---|---|---|
| `lock` | `WriteLock`, `write_lock.rs` | the access mode and the window: `readonly`, `strict_readonly`, `remaining`, `open`, `close`, `refuse`, `toggle`, `forget_window`, `keep_window_from` | 3 of 3 |
| `prefs` | `ConnectionPrefs`, `prefs.rs` | `resolve_tree` (what `select` did inline: the entry's settings, else the app's) and the getters' clamping | 6 of 6 |
| `readings` | `Readings`, `readings.rs` | the `DBSIZE` bookkeeping (`request_dbsize_refresh`, `finish_dbsize_refresh`, `note_dbsize`, `note_beat_dbsize`, `flushed`), the slow-log sample, the client pause, `reset` | 10 of 10 |
| `link` | `Link`, `link.rs` | the heartbeat's pacing (`beat_due`, with the backoff and its constants), the ping's outcome (`note_ping` → `PingOutcome`), `go_offline`, the load retries, the two notice throttles (one of them written out twice before), `problem`, `reset` | 4 of 16 — the heartbeat callback, `select`, the task dispatch and the tests read the rest |
| `facts` | `ServerFacts`, `facts.rs` | the feature matrix's questions (`blocked_by`, `panel_block`, `command_block`), `supports`, `set_topology`, `note_connect`, `reset` | 8 of 8 |
| `scan` | `KeyScan`, `scan.rs` | `reset` (the scan's half of `reset_scan`), `extend`, `forget_key`, `forget_keys_where`, `forget_server`, `renew_tree_id` (twelve copies of the line before), `listed_count` | none — `key.rs`'s scan engine reads and writes all of them from its tasks |
| `open` | `OpenKey`, `open_key.rs` | `next_epoch`, `close` (two copies before), `forget_server`, `try_get_mut_key_value` | none — every editor reads them |

Left as fields: `report_findings`, `reshard_progress` and `info_snapshot`
(three panels' leftovers with nothing in common but a getter and a setter
each), `terminal` (a bool) and `error_messages`. A struct around them would
name nothing.

`reset` now reads as what it is: each group's own reset, each keeping
exactly what it kept before (*What `reset` keeps*, above). Field writes inside
one update are not observable in between — events are delivered after it —
so gathering a group's lines into one call changes nothing anybody sees.

How it was moved: each group's fields nested by a script, every access
rewritten at the span the compiler reported (desktop, tests and the wasm
build); then the group's methods that need no `Context` moved onto it, the
state's public methods kept as one-line delegates, so no caller outside
`states/server` changed. What emits, spawns or times out
stayed on the state, and the visibility of each field is what the compiler
asked for: every field made private, `pub(super)` put back only where it
reported one reached from outside.

Tests that came first, each shown to fail in a worktree with the code it
guards mutated (`test_support::record_events` is the event recorder they
share):

- `the_write_window_opens_closes_and_runs_out_in_order` — lock. Mutated: no
  announcement on re-lock; a window opened on a strict connection; the timer
  kept *and* the expiry's guard removed, one round, since each alone is
  covered by the other.
- `a_selected_connection_takes_its_tree_settings_from_settings` — prefs,
  through `select`. The entry's own settings, which `select` cannot reach
  without dialing, are `an_entry_s_own_tree_settings_win_over_the_apps` on
  `resolve_tree` after the move.
- `a_forced_probe_forgets_the_matrix_before_it_asks` — facts.
- `a_cleared_slow_log_is_a_new_generation_and_a_pause_shows_until_lifted` —
  readings.
- `the_heartbeat_moves_the_health_and_announces_only_a_change` and
  `a_beat_goes_out_only_when_it_is_due` — link. The second's last step at
  first proved nothing (a tab back in the foreground is not throttled whether
  or not its window was reset), which the mutation run showed; it now sends
  the tab back to the background.
- `a_new_query_announces_the_emptied_tree_and_a_refresh_does_not` — scan.

`note_command_error` has no test: it needs a `redis::RedisError`, which the
GUI crate cannot build. Only the path of the field it writes changed.

Step 3, `ServerTask`: a variant per operation stays. It is the task's name in
the logs and the category of its error line (`[delete_key] …`), which the
status bar shows; grouping variants would change what the user reads. What
it also carried was a classification written twice — the background
refreshes, in `settle_task` and in the status bar — and that is
`ServerTask::is_background_refresh` now, with a test.

Found on the way: `an_entry_comes_back_from_its_form_as_it_went_in` (phase 2)
waited five seconds for the list's write and ran out once when the whole
suite ran on a loaded machine; it waits thirty. That was the wrong reading:
it failed again in phase 6 after the full thirty, with the form still open
and nothing saved. The click on Save had missed — see phase 6.

### Phase 4 — the target-gate audit

Inventory the 136 browser-only gates by file and by kind (a whole item, a
statement inside a function, an import). For each statement-level gate:
move it up to an item or module gate, or behind a named seam (CLAUDE.md,
*Desktop first*). Update CLAUDE.md's counts when done. This is the step most
likely to find a desktop arm that was edited to fit the browser; such a find
is reported, not silently fixed.

#### Done (2026-10-20)

The inventory, by what the gate stands on (`src`, `#[cfg(target_family =
"wasm")]` only):

| | Before | After |
|---|---|---|
| a whole function, const, static or impl | 81 | 87 |
| an import | 26 | 25 |
| a module | 5 | 7 |
| a statement, a `let`, a block or a match arm inside a function | 24 | 1 |
| total | 136 | 120 |

Of the 120: 71 are the browser arm of a same-signature pair, 25 imports, 7
modules, 16 items a browser arm needs (its consts, its fetchers, the shortcut
flag) and one statement, the Topology arm of `content.rs`'s panel match,
which is the shape CLAUDE.md prescribes for a panel the browser does not have
and stays. `cfg(not(target_family = "wasm"))`: 253 → 252.

What the 23 statement gates became:

- **Two functions with one signature, desktop first**, where a function's
  whole body was the two arms: `uses_command_key`, the root's
  `export_diagnostics` and `persist_window_state`, the config page's
  `ensure_config_docs`, the memory analysis's `start_ai_analysis`, the
  terminal's `ask_ai`, `save_app_state`. Projected onto what the desktop
  compiles (comments and the browser's items dropped, whitespace removed),
  all seven read exactly as before.
- **A named seam**, where the arms were statements in a longer body:
  `kit_files` (the asset list), `leave_report` (the panic hook),
  `load_large_assets` and `open_logs_folder` (`lib.rs`), `seal_secret` /
  `unseal_secret` (the AI key and the proxy, four accessors),
  `proxy_setting_accepted` (Settings), `sharing_fields` (the server form's
  *Shared* box, an empty list on the desktop) and the title bar's
  `application_items`. The desktop's differences are those calls and nothing
  else.
- **A `web` module**, where the browser's side was state of its own:
  `i18n_loader/web.rs` (the fetched locale maps — nine gates to two) and
  `states/trash_store/web.rs` (the bin on the bridge — eight to two). The
  Metrics history's two loaders joined the writers they belong beside,
  `stat/persist.rs` and `stat/persist_web.rs`.

The four `cfg!(not(target_family = "wasm"))` in views — the Kill button in
the status bar, the Lua library and the Functions panel, and the memory
analysis's AI button — ask `helpers/this_build.rs` now
(`kills_runaway_scripts`, `has_ai_assistant`), as CLAUDE.md asks of a button
a build lacks.

Left as they are: item pairs that sit side by side and read as pairs
(`pacing.rs`, `this_build.rs`, `export.rs`, the compare window's and
Settings' dialog form). Gathering them into a module would part each desktop
arm from its twin.

Tests first where the moved code had none: the AI key and the proxy are kept
encrypted and read back plain (`a_secret_setting_is_kept_encrypted_and_read_back_plain`),
the desktop saves a proxy it can use (`the_desktop_saves_a_proxy_it_can_use`),
a listing names the kit's icons beside the app's
(`a_listing_names_the_kit_s_icons_beside_ours`), the hook's report is found
at the next launch (`a_report_the_hook_leaves_is_found_at_the_next_launch`),
and `the_desktop_has_all_of_it` holds the two new answers. The first and the
third pass on the tree before the change too; each of the seven ways of
taking a desktop arm out (no encryption, no decryption, only an empty proxy,
no kit icons, no file for the report, no Kill button, no AI button) fails
exactly the test that guards it. The wasm build's warnings are the same 43,
by message and file, as before the change.

**The history check.** For each statement pair, the commit that added the
browser arm (`git log -S`) was read for desktop lines it changed. One
desktop arm was edited to fit: `set_ai_api_key` used to encrypt only a key
it would keep, and since the web build (`dbcb4aa`, 2026-09-18) it encrypts
first and drops the result when the key is empty — so clearing the key
reaches the master key, which on a machine with no other secret is the
first keychain read of the session. Reported, not fixed here (see
*Progress*). The rest only moved desktop lines into blocks or helpers
(`platform_info` for `os_info`, `register_embedded_mono_fonts`,
`when_locale_ready`), each giving the same result.

### Phase 5 — the connection layer (analysis first)

1. A check that each public operation module (`*_ops`, `keyspace`,
   `server_ops`, …) returns typed structs and has a live test — a script over
   `lib.rs`'s exports and `tests/live.rs`; the gaps become a list.
2. `tests/live.rs` (8.4k lines, one file) into `tests/live/` modules by
   feature with the shared fixtures in one module. **Test names stay exactly
   as they are**: `integration.yml` filters on them (`dragonfly_*`,
   `garnet_*`, the lanes' own selections).
3. `manager/client.rs` (1.8k), `config.rs` (1.5k without tests),
   `ssh_tunnel.rs` (1.6k): read for seams; split only where a seam is clear.
   This layer's correctness is the product's — structure changes here wait
   for the matrix to run on them.

#### Step 1: the operations and their live tests (2026-10-21)

An operation here is a `pub fn` of `zedis-connection` that takes a
`ServerDb` or a `ClusterNode`. There are 210, and every one answers a type
of its own or a plain value — none hands back a `redis::Value`. 176 are
called by a live test, two more (`latency_doctor`, `memory_doctor`) only
through one that is, and `lock_writes` / `unlock_writes` are the bridge's
seam pair, no command on either side. That leaves 28 with no live test,
each a gap in the rule CLAUDE.md states (*It gets a live test in the same
change*):

| Area | Operations | What a test needs |
|---|---|---|
| Server-wide, destructive | `flush_db`, `flush_all`, `function_flush`, `script_flush`, `acl_load`, `failover_abort` | a server of its own (the busy or the replication lane's), not the standalone one the other tests share |
| Functions | `function_list`, `function_dump`, `function_restore`, `function_fcall` | standalone; `function_load` / `function_delete` already are |
| ACL | `acl_list` | standalone |
| Cluster membership | `cluster_meet`, `cluster_forget`, `node_migrate_slots` | a node to add and take away again; `node_migrate_slots` is behind `cluster_migrate_slots`' dialects |
| Keyspace | `publish` | standalone, with a subscription to hear it |
| Panels | `key_size_distributions`, `sample_memory_usage`, `memory_stats` | standalone (`INFO keysizes` is Redis 8 / Valkey 8.1) |
| Search | `ft_create`, `ft_alter_add`, `ft_dropindex`, `ft_aggregate`, `ft_profile` | the module lanes; the existing tests build their indexes with raw `FT.CREATE` |
| Streams | `stream_ack_delete` (`XACKDEL`, Redis 8.2), `stream_nack` (`XNACK`, Redis 8.8) | the `redis-8.10` lane, gated by their floors |
| JSON | `json_get`, `json_set`, `json_merge` | the module lanes |

Writing them is new tests, not structure: each is a change of its own.

#### Step 2: `tests/live.rs` into `tests/live/` (2026-10-21)

Fifteen modules by feature beside `main.rs`, which keeps the crate's doc,
the imports and the fixtures (`skip_unless!`, `register`, the server
builders, `supports`…): `connect`, `transport`, `keys`, `values`,
`transfer`, `server`, `sessions`, `scripts`, `acl`, `replication`,
`sentinel`, `cluster`, `cluster_admin`, `stack`, `compatible`. The file's
own section markers had drifted (the TLS and SSH tests sat under
*replication*, the logical copy under *redis-stack*), so the grouping is by
what each test exercises; a helper went with its only user. The three test
bodies two lanes share (`scan_sees_every_type_it_wrote`,
`collections_page_and_write_through_their_operations`,
`check_batch_ttl_conditions`) are `pub(super)` in their feature's module and
imported by `compatible` and `cluster`. Every other line moved unchanged
(a line-by-line count of the old file against the new ones differs only in
the headers, the imports, the `mod` lines, those three `pub(super)` and the
eight section markers; the Dragonfly and Garnet section's prose became
`compatible`'s module doc).

The test binary is still `live`, and the 94 test names are the same; what a
listing prints gains the module (`keys::standalone_scan_sees_…`). CI selects
by substring — the whole suite on the matrix, `dragonfly` / `garnet` on the
smoke lanes — and both filters select the same three tests as before. Run
locally against `scripts/it/up.sh` (Redis 8.6.1, every scenario but SSH,
ports moved to `IT_PORT_BASE=46379` / `IT_CLUSTER_BASE=47000`): 94 passed,
of which 18 skipped for a lane that was not there (modules, Dragonfly,
Garnet, SSH, a foreign flavor) or a Valkey-only floor.

#### Step 3: the seams (read 2026-10-21; nothing moved yet)

Each of the three has seams a move can follow without touching a line of
logic. They are moves of whole items into submodules, `impl` blocks for a
type's methods, the shape the views and `states/server` already have.

- **`manager/client.rs`** (1,832 lines, no tests of its own: the operations'
  live tests cover it) is one `impl RedisClient` of 1,785 lines. By what the
  methods do:
  - `client/describe.rs` — what the client is: the identity accessors,
    `nodes_description`, `build_slot_map`, `build_topology`,
    `node_label_server`. About 400 lines.
  - `client/fan_out.rs` — reaching every master: the three
    `query_async_masters*`, `pipeline_per_key`, `unlike_keys*`,
    `set_ttl_keys_scattered`, `delete_verb`. About 300.
  - `client/scan.rs` — `first_scan`, `scan_nodes`, `scan`, `scan_matching`,
    `scan_values_round`, `sample_scan_memory_usage`. About 500.
  - What stays: the server and key reads the panels ask for (slow log,
    hotkeys, command stats, slot stats, `INFO keysizes`, memory usage,
    object meta, previews, `DBSIZE`, the heartbeat probe). About 550.
- **`config.rs`** (1,590 lines before its tests):
  - `config/transfer.rs` — export and import: `to_export_json*`, the JSON,
    URI, multi-entry and Redis Insight imports, with the URI parsing they
    use (`URI_SCHEMES` … `keep_first_host`). About 450.
  - `config/form.rs` — the server form's mapping: `from_form_data` and the
    `*_from_form_value` / `*_index` helpers. About 180.
  - `config/store.rs` — the list: `SERVER_CONFIG_MAP`, `get_servers` /
    `get_server`, the `set_servers_cache` and `save_servers` pairs (both
    arms together), the secrets' sealing, the redacted TOML. About 300.
  - What stays: `RedisServer`, its resolvers, secrets and endpoints. The
    host-field, secret and upgrade-fixture tests go with what they test.
- **`ssh_tunnel.rs`** (1,413 lines before its tests):
  - `ssh_tunnel/host_keys.rs` — `ClientHandler`'s key check, the host key
    prompt and the known_hosts files. About 200.
  - `ssh_tunnel/target.rs` — `SshTarget` and its resolution from the form
    and `~/.ssh/config`, ProxyJump included. About 170.
  - `ssh_tunnel/auth.rs` — key, password and agent authentication, the RSA
    hash negotiation, the remembered agent key. About 290.
  - `ssh_tunnel/monitor.rs` — `MONITOR` read through a tunnel. About 120.
  - What stays: the runtime, the session cache, the forward channel and
    `TunnelDial`.
  - Found on the way: `install_crypto_provider`, the insecure certificate
    verifier and `open_single_sni_tls_connection` are TLS without SSH and
    sit here only because the tunnel's TLS needed them first. Their place is
    beside `config/tls.rs`; that is a move across modules (paths inside the
    crate change, `lib.rs`'s exports do not) and a step of its own.

The order, when the matrix has run on step 2: `ssh_tunnel.rs` first (its
lane is the one the local topology can run least of, so it most needs the
matrix), then `config.rs`, then `client.rs`. Each is one change, verified
like step 2: the line-by-line count, the public paths unchanged, the live
suite locally, then the matrix.

#### Step 3, done (2026-10-21)

Done before the matrix had run on step 2, at the user's word: the two go to
the matrix together when they are pushed. Locally the
whole topology ran — this time with the SSH scenario, so the tunnel tests
ran too (Redis 8.6.1, ports at 46379 / 47000) — before and after the three
splits: 94 passed both times, with the same twenty skips (modules,
Dragonfly, Garnet, a foreign flavor, the Valkey-only floors).

| File | Now | Submodules |
|---|---|---|
| `ssh_tunnel.rs` | 817, with its tests | `host_keys` 225, `target` 191, `auth` 325, `monitor` 142 |
| `config.rs` | 1,356, with its tests | `transfer` 431, `form` 233, `store` 325 (`tls` was there) |
| `manager/client.rs` | 534 | `describe` 418, `fan_out` 386, `scan` 579 |

Every path another file used is re-exported where it was (the host key
prompt, the tunnelled `MONITOR`, the SSH target's resolution; the form's
indexes, the list's functions with their wasm and native arms, `ImportError`),
so no file outside the three changed. The URI parsing stayed in `config.rs`,
the form and the import both reading it. The line-by-line count differs
only in the headers, the `mod` / `use` lines and `pub(super)` on what the
core of each file and its tests reach: `authenticate`, `ClientHandler`'s two
fields and four helpers the SSH tests call; the list's `RedisServers` field
and three helpers, and two of the form's, for `config.rs`'s tests.
`client.rs` needed none — every method it lost is called only beside where
it went — and lost not a line. The wasm build's warnings are the same but
two, `redact_servers` and `RedisServers`, which now say `config/store.rs`.

Left for a step of its own, as above: the TLS pieces in `ssh_tunnel.rs`
that have nothing to do with SSH.

#### The TLS pieces, moved (2026-10-21)

`tls_connection.rs`, native only like `ssh_tunnel.rs`: the insecure
certificate verifier, `install_crypto_provider`, the connector built from an
entry's TLS settings (`build_tls_connector`), the server name it checks
(`tls_server_name`) and `open_single_sni_tls_connection` — 195 lines, and
`ssh_tunnel.rs` is 655. It is not beside `config/tls.rs` after all, which is
the TLS *material* an entry carries and compiles for both targets; this is
the dialing, which the browser never does. `lib.rs` re-exports
`install_crypto_provider` from there (the wasm build keeps its no-op), the
tunnel imports the connector and the server name, and `authenticate_redis`
became `pub(crate)` for the direct connection. Before the move the one path
no test held — that the certificate is checked against the configured server
name, not the dialed host — got a live test,
`tls_checks_the_certificate_against_the_server_name_it_was_given`
(`transport`): `localhost` is accepted on 127.0.0.1, `wrong.example` refused.
It fails with the name taken out of the check. The live suite after the
move: 95 passed — the 94 and the new test — with the same skips.

#### The 28 operations, tested (2026-10-21)

Seven new tests and five extended ones; where a test already sent the raw
command an operation wraps, it now calls the operation.

| Operations | Test | Runs on |
|---|---|---|
| `flush_db`, `flush_all`, `script_flush`, `function_flush` | `busy_server_flushes_empty_what_they_name` (new, `server`), with `function_restore … FLUSH` | the `busy` server, which the runaway-script test now takes turns on (`BUSY_SERVER`) |
| `function_list`, `function_fcall`, `function_dump`, `function_restore` | `standalone_functions_list_call_dump_and_restore` (new, `scripts`) | standalone, `floors::FUNCTIONS` |
| `acl_list`, `acl_load` | `acl_log_dryrun_genpass_and_the_aclfile_gate` | standalone |
| `failover_abort` | `replication_pair_promotes_relinks_and_fails_over` | the pair, `floors::FAILOVER` |
| `cluster_meet`, `cluster_forget` | `cluster_meet_and_forget_are_told_to_every_master` (new, `cluster_admin`) | cluster, under `CLUSTER_SLOTS` |
| `node_migrate_slots` | `cluster_migrates_slots_atomically_on_valkey_9_and_redis_8_4`, whose way back goes through it | cluster, `floors::ATOMIC_SLOT_MIGRATION` |
| `publish` | `standalone_channel_subscription_delivers_both_kinds_of_pubsub`, plain and sharded | standalone |
| `key_size_distributions` | `standalone_info_keysizes_buckets_types`, and its empty answer without the section | standalone |
| `sample_memory_usage`, `memory_stats` | `standalone_memory_panels_sample_keys_and_read_memory_stats` (new, `server`) | standalone |
| `stream_ack_delete`, `stream_nack` | `standalone_stream_ack_delete_and_nack_settle_one_pending_entry` (new, `values`) | standalone, `STREAM_REF_POLICIES` / `STREAM_NACK` |
| `json_get`, `json_set`, `json_merge` | `stack_json_document_is_read_written_whole_and_merged` (new) | the module lanes; `JSON.MERGE` where the server has it |
| `ft_create`, `ft_alter_add`, `ft_aggregate`, `ft_profile`, `ft_dropindex` | `stack_search_index_is_created_altered_aggregated_and_dropped` (new) | the module lanes; `FT.ALTER` / `FT.PROFILE` where the server has them, `DD` by `valkey_lane()` |

What the harness cannot show, said in the tests: `ACL LOAD` only as the
refusal of a server without an `aclfile`; `FAILOVER ABORT` only as the
refusal with nothing to abort (an idle pair finishes a failover before an
abort could land); `CLUSTER MEET` only with a member every master already
has, and `CLUSTER FORGET` only with an id none of them knows — there is no
spare node to add and take away; `ASYNC` against `SYNC` on the two flushes
not at all, the cache being empty at once either way.

Run locally: the whole suite on the topology (Redis 8.6.1), 102 passed —
the 95 and seven new; the standalone, busy and replication tests also on
Redis 6.2.20 and on Valkey 9.0.0, and the standalone ones on Redis 8.10.2,
the only server here with `XNACK`. Each test fails with its operation
broken (21 changes in a worktree, one at a time: a command swapped, an
argument dropped, a policy mapped wrong — `SILENT` for `XNACK`'s `FAIL`,
`APPEND` for `FUNCTION RESTORE`'s `REPLACE` and `FLUSH`, the end
`node_migrate_slots` is told on), every one caught. The JSON and search
tests have not run anywhere yet — no module here — and are written to the
command lists valkey-search and valkey-json publish (no `FT.ALTER`,
`FT.PROFILE` or `JSON.MERGE`; `FT.AGGREGATE` groups an indexed field
without a `LOAD`, as its own tests do), so a red module lane after the push
points at them first.

### Phase 6 — the shell and the panels

- `status_bar.rs` (2.0k without tests): the tools menu, the server status
  chip and the metric chips into submodules.
- `root.rs`: its `render` was split in phase 2; what is left is the tab
  strip and the overlays as methods, if they still read long.
- Observability panels: confirm each read-only grid is a `ZedisTextTable` and
  each sampling loop a `PanelPoll`; a panel that still has its own is a step.
- Administration tools: confirm every destructive action goes through
  `confirm_dangerous_command` / the `.danger()` shape.

#### The status bar (2026-10-21)

`status_bar.rs` (2,122 lines with its tests) keeps the view, its snapshot of
the server, the database select, `BarFit` and `render`; four submodules hold
the rest, each an `impl ZedisStatusBar` block of whole methods:

| File | What | Lines |
|---|---|---|
| `status_bar/tools_menu.rs` | the Tools menu, its gates and entries | 412 |
| `status_bar/server_status.rs` | the left side: tag, node counts and their tooltip, database, read-only, the key count | 448 |
| `status_bar/telemetry.rs` | the right side: the link, latency and the metric chips | 350 |
| `status_bar/editor_controls.rs` | Soft Wrap, the format and the Viewer, side by side or folded | 193 |

The line-by-line count differs only in the headers, the `mod` and `use`
lines, and `pub(super)` on what the other files reach (five render methods,
`show_editor_controls`, `format_nodes*`, `format_latency`, `VIEWER_MODES`,
`ToolsMenuGates` and its fields); `product_line`, which the troubleshooting
report uses, is re-exported where it was. Its test moved with it.

No test built the bar before. Two UI tests came first: the bar draws the
server's context and the four chips, and no Kill button or editor control
it should not (`the_bar_draws_the_server_s_context_and_its_telemetry`); the
Tools menu, walked group by group with the arrow key, lists the panels in
their order, and on a read-only connection keeps the writes with the reason
they are off (`the_tools_menu_groups_the_panels_and_says_why_a_write_is_off`).
Five ways of breaking the bar (a chip's id, the Kill button's condition, the
read-only gate, the groups' order, the scan button) each fail one of them.

Found on the way: the server form's test that had waited thirty seconds
failed again in a full run, after the thirty. Run five times at once it
failed three, each time with the form still open and the list empty: the
click on Save had missed. A dialog's entrance animation runs on the wall
clock, a click is aimed from one frame and lands a few frames on, and on a
loaded machine the button had moved by then. `init_ui` now reduces motion,
as gpui-component's own dialog tests do, so the entrance settles on its
first frame; the same five-at-once run passed ten times in ten.

#### The root (2026-10-21)

`root.rs` was 1,440 lines; it is 946, of which 275 are its UI tests. Two
moves:

- **`root/tabs.rs`** — the workspace tab strip: `ContentTab`, `TabAction`,
  the drag payload and preview, `MAX_TABS`, activating, closing (one, the
  others, to the right, those of removed connections), moving, persisting,
  `render_tab_bar`, the two pure functions behind them and their unit tests,
  and `restore_tabs`, the strip a session ended with.
- **`new`** went from 235 lines to 40: the restore is `tabs::restore_tabs`,
  the global events the root answers for are `handlers::on_global_event`,
  following the system appearance is `handlers::on_window_appearance`, and
  the housekeeping loop is `start_housekeeping`. Each body moved as it was.

The line-by-line count differs only in the headers, the `mod` / `use`
lines, the calls that replaced the moved bodies, `pub(super)` on what the
root and its handlers reach (eight methods, `ContentTab::new` and its three
fields) and one line rustfmt joined at the shallower indent.

Three UI tests came first, for what the strip did that nothing pinned: a
drag moves a tab and the active one with it, Open in New Tab finds the tab
already on that connection, Close Right and Close Others leave what they
name, each written down (`the_tab_strip_moves_finds_and_closes_its_tabs`); a
selection is its tab's connection from then on, and one past the limit opens
in place with a notice (`a_tab_follows_its_selection_and_one_past_the_limit_opens_in_place`);
a session's tabs come back on their databases, without the one whose
connection is gone, with the remembered one active
(`a_session_s_tabs_come_back_and_the_remembered_one_is_active`). Nine ways
of breaking those paths each fail one of them. The notice is counted, not
read: a notification has no label a test can find, and a tab on a server
that does not exist puts up a notice of its own, so the limit is reached on
Home tabs.

#### The panels and the administration tools, checked (2026-10-21)

Read-only grids: every one is a `ZedisTextTable`. The table and list
delegates still written by hand are the editable grids CLAUDE.md names
(stream entries, proto bindings, the script library), the hex viewer and
the key tree.

Sampling loops: Server Load and Hot Keys are `PanelPoll`s. Three loops are
their own — the cluster Load tab, the Latency tab and the command log —
because each samples while one tab of its panel is showing and stops when
the tab changes, and the Load tab's fetch needs the view's master list,
where `PanelPoll`'s takes only the server and database. All three take
their interval from `pacing` and skip a round in the background, which is
what the rule is for, so they stay. The Pub/Sub panel's five-second loop is
a repaint of its status line, not a poll.

Confirms: of the sixty alerts in `src/views`, forty-four are `.danger()` or
go through `danger_confirm.rs`; most of the rest change nothing that cannot
be changed back (`BGSAVE`, `CONFIG SET`, a copy, starting `MONITOR`, an
import that never overwrites). What is left is listed under *Progress* as
findings: they are behaviour — Return would answer Cancel — not structure.

#### `kv_table.rs`, `memory_analysis.rs`, `slowlog_editor.rs`, `terminal.rs` (2026-11-01)

The four view files over ~1,800 lines split along their natural seams while
keeping each parent `.rs` file as the module root:

| Parent / Submodule | Responsibility | Lines |
|---|---|---|
| `kv_table.rs` (was 2,268) | `ZedisKvTable`, `ZedisKvDelegate` (`TableDelegate`), `define_kv_editor!`, unit & UI tests | 970 |
| `kv_table/entry_panel.rs` | `PreviewMode`, JSON/tree preview helpers, row selection and right-hand edit/add panel | 563 |
| `kv_table/operations.rs` | `parse_bulk_rows`, CSV export, bulk add, row add/remove/filter, key operation dialogs, row context menu | 415 |
| `kv_table/toolbar.rs` | footer toolbar (`KEYWORD_INPUT_WIDTH`, `render_footer`) | 263 |
| `kv_table/layout.rs` | pane split math, width probe, column sizing (`fit_columns`, `new_columns`, `estimated_width`) | 171 |
| `memory_analysis.rs` (was 2,003) | `ZedisMemoryAnalysis`, construction, subscriptions, unit tests | 904 |
| `memory_analysis/sampler.rs` | `RdbSource`, `fetch_keysizes`, live scan loop, schema probe, RDB file picker and offline RDB parser | 674 |
| `memory_analysis/tables.rs` (was 195) | `PrefixRow`, `SingleKeyRow`, sort modes, prefix breadcrumb, type/encoding table rebuilds, table builders | 431 |
| `memory_analysis/export.rs` | AI memory diagnosis, CSV exports, data dictionary (Markdown / JSON) and schema dialog launcher | 235 |
| `slowlog_editor.rs` (was 2,030) | `PerformanceTab`, `ZedisSlowlogEditor`, settings banner, `Render`, unit tests | 774 |
| `slowlog_editor/table.rs` | `SlowLogRow`, latency correlation chips, `SCRIPT SHOW` dialog, window filter pill, row filtering | 668 |
| `slowlog_editor/latency.rs` (was 600) | `LATENCY LATEST` / `HISTORY` polling, event expansion, sparkline detail, tracking enable/reset | 666 |
| `slowlog_editor/top_commands.rs` | `CommandAggRow`, per-command aggregation, toolbar actions, CSV/JSON export, `SLOWLOG`/`COMMANDLOG RESET` | 571 |
| `slowlog_editor/commandlog.rs` | Valkey 8.1 `COMMANDLOG` size-log switch and 30s polling | 173 |
| `terminal.rs` (was 1,830) | `ZedisTerminal`, banner constants, `Render`, unit tests | 864 |
| `terminal/execution.rs` | command execution, blocking/stream command refusals, write-gate & danger confirms, `?` AI assistant, Batch mode | 478 |
| `terminal/transcript.rs` | `TranscriptEntry`, `Block`, reply format rendering, `MULTI`/`EXEC` folding, scrollback cap, copy/save output | 340 |
| `terminal/completion.rs` | `ReverseSearchState`, command auto-completion, history navigation, `Ctrl+R` reverse search | 215 |

### Phase 7 — the browser entry and the helpers

- `zedis-web/src/transport.rs` (1.6k without tests): one module per trait it
  implements (`BridgeTransport`, `BridgeStorage`, `BridgeServerStore`,
  `BridgeAccounts`, `BridgePageData`), the shared request helpers in one.
- `helpers/updater.rs` (1.6k): the check, the manifest, the download and the
  install as modules.

#### The transport (2026-10-21)

`zedis-web/src/transport.rs` was 2,368 lines; it keeps `HttpBridgeTransport`,
the requests every seam makes (`post`, `call`, `failure`, `too_large`,
`segment`, signing in again, starting work unwatched) and the tests, and
each trait it carries for `zedis-connection` is a submodule:

| File | Trait | Lines |
|---|---|---|
| `transport/commands.rs` | `BridgeTransport`: `/v1/exec`, sessions, the write window, `/v1/watch`, the diagnosis, signing out | 302 |
| `transport/storage.rs` | `BridgeStorage`: the bin and the Metrics history | 171 |
| `transport/servers.rs` | `BridgeServerStore`, and the list read with its secrets named | 167 |
| `transport/accounts.rs` | `BridgeAccounts` and the fourteen views it reads | 555 |
| `transport/page_data.rs` | `BridgePageData`, `page_seed` and the ordered write queue | 251 |

The line-by-line count differs only in the headers, the `mod` / `use`
lines, one more `impl HttpBridgeTransport` (three inherent blocks' methods
went to three modules, and the two left empty went) and `pub(super)` on what
the tests reach.

Three tests came first, for the parts no test had sent a request through:
saving the list entry by entry — the unchanged one not sent, the edited one
put with its kept secret named, the new one posted, the gone one deleted,
then the list read again (`the_list_is_saved_entry_by_entry`); the bin's and
the history's routes, bodies and reply (`the_bin_and_the_metrics_are_kept_under_their_server`);
and a session, the write window and a watch with its confirmation and its
release (`a_session_the_write_window_and_a_watch_are_asked_for_by_their_routes`).
Seven ways of breaking them each fail one.

#### The updater (2026-10-21)

`helpers/updater.rs` was 1,641 lines; it keeps the hosts and timeouts, the
requests that try GitHub and then the mirror (`http_get_string`,
`http_get_string_mirrored`, the mirror's URLs) and the tests, over four
submodules of whole items:

| File | What | Lines |
|---|---|---|
| `updater/check.rs` | whether there is a newer release on this build's channel, and its notes | 384 |
| `updater/manifest.rs` | `latest.json` and the API's list, each file read off its name, the one this machine installs | 201 |
| `updater/download.rs` | the installer from either host, capped, kept only when its hash matches | 125 |
| `updater/install.rs` | handing it to the OS; on macOS the in-place swap | 306 |

The public functions are re-exported where they were (`relaunch` under the
same macOS gate it has), so `helpers.rs` and `root/desktop.rs` are
unchanged. The line-by-line count differs only in the headers, the `mod` /
`use` lines and `pub(super)` on what `check` and the tests reach — every
field of the four release structs among them — and the signatures rustfmt
wrapped for it.

Two tests came first, against a server on 127.0.0.1 with the proxy set to
direct: a download is kept only when its bytes are the ones named, with the
progress reaching the server's size
(`a_download_is_kept_only_when_its_bytes_are_the_ones_named`), and a text
the origin cannot give comes from the mirror, saying so
(`the_mirror_answers_what_the_origin_cannot`). Four ways of breaking them
each fail one. The configured proxy is the process's, and the app state's
secret test sets one, so both hold `test_support::hold_the_proxy`. Linux
and Windows compile the moved code in CI only; its gates are the ones it had.

### Phase 8 — core and storage

Low risk, low priority: a pass over `zedis-core` and `zedis-db` for dead code,
duplication with `zedis-connection`, and the storage rules (container-level
`serde(default)`, a bad row skipped and never deleted).

#### Done (2026-10-21)

- **Dead code.** Of the public items in the two crates, two were named by
  nothing: `zedis_core::json::empty_object` and `ParamKind::is_vector` in
  `search_params.rs`. Neither had a hand-written twin to take its place, so
  both went. `RdbParser::rdb_version` is read only by the parser's own
  tests, which is what it is for, and stays; `init_database_for_tests` is
  already `#[cfg(test)]`.
- **Duplication with `zedis-connection`.** None. The names the crates share
  are different functions: the codecs' `float` / `int` / `pairs` read
  binary formats where `reply`'s read RESP, the two `classify_reply`s
  classify a probe and a `KILL`, and the connection crate's `string.rs`
  re-exports `zedis-core`'s string helpers on purpose. Its
  `redis_value_to_string` is the terminal's text rendering, not a second
  `reply::display`: the two print a nil and an array differently.
- **Storage rules.** Every stored value type carries a container-level
  `#[serde(default)]` but `KeyMetadata`, whose two fields each had one: it
  has one now, so a field added later cannot make an existing note
  unreadable (nothing reads differently today). Left as they are:
  `StoredEnvelope`, the versioned envelope (its version defaults to 1, not
  0), and `LuaScriptExport`, the clipboard's import format, which is never a
  stored row. Every loader skips a row it cannot read or returns the error
  without writing; none deletes one. The bin's purge removes expired
  entries by the time in their key and never decodes them, which is its
  retention working, not a bad row being dropped.

## Out of scope

Behaviour changes, UI redesign, new dependencies, performance work without a
benchmark before and after, and anything that would rename a stored table,
a wire field or a test name CI selects by.

## Progress

| Phase | Step | Status | Commit |
|---|---|---|---|
| 1 | `api.rs` split | done | `3fd44e2` |
| 1 | `main.rs` setup functions | done | `3fd44e2` |
| 1 | `accounts.rs` seams (analysis) | done | `3fd44e2` |
| 1 | `accounts.rs` split | done | `3fd44e2` |
| 2 | eleven long functions | done (with the bridge's `main`, phase 1) | seven in `5590760`; the tree's actions, the root's and the migration window's `render` in `e22ca9a` |
| 3 | `ZedisServerState` field map | done: groups, where they meet, what `reset` keeps, the order for step 2 | `e22ca9a` |
| 3 | sub-structs | done: seven groups; the panels' three fields, `terminal` and the errors left as fields | `e22ca9a` |
| 3 | `ServerTask` | done: a variant per operation kept; the background refreshes named once | `e22ca9a` |
| 4 | gate inventory | done: 23 of 24 statement gates up to items, seams or modules; the four `cfg!` asks go through `this_build` | `b22963a` |
| 5 | operations ↔ live tests check | done: 210 operations, all typed; 28 without a live test, listed | `cb3cb50` |
| 5 | `tests/live.rs` split | done: fifteen modules and `main.rs`, the 94 names kept | `cb3cb50` |
| 5 | `client.rs`, `config.rs`, `ssh_tunnel.rs` split | done: ten submodules, the live suite with SSH before and after; the matrix checks it on push | `0604a68` |
| 5 | TLS out of `ssh_tunnel.rs` | done: `tls_connection.rs`, one live test of the server name first | (pending) |
| 5 | the 28 operations without a live test | done: seven new tests, five extended; 6.2, 8.6, 8.10 and Valkey 9 locally, the module lanes on push | (pending) |
| 6 | status bar | done: four submodules, two UI tests first; UI tests reduce motion | `2ffdd87` |
| 6 | root | done: the tab strip into `root/tabs.rs`, `new` into named parts, three UI tests first | `b54fde5` |
| 6 | panels, administration tools | checked: the grids and loops follow the rules; five confirms listed as findings | `b54fde5` |
| 6 | `kv_table.rs`, `memory_analysis.rs`, `slowlog_editor.rs`, `terminal.rs` split | done: 11 new submodules + 2 extended, all 4 parent files under 1,000 lines | (pending) |
| 7 | transport | done: one submodule per trait, three tests first | `2d90e1f` |
| 7 | updater | done: check, manifest, download, install; two tests first | `c78ad4e` |
| 8 | core, db pass | done: two dead helpers gone, no duplication, `KeyMetadata` given its container default | `52facc6` |

Findings that are bugs rather than structure are listed here as they turn up,
and fixed as their own changes. All four were fixed on 2026-10-21; under
each, what holds it — a test or check shown to fail with the fix taken out
in a worktree — or why nothing can.

- Phase 4: `ZedisAppState::set_ai_api_key` encrypts an empty key before
  dropping it, which reaches the master key (a keychain read where nothing
  else has needed it). The fix is the order it had before `dbcb4aa`:
  `if api_key.is_empty() { None } else { Some(seal_secret(api_key)) }`.
  **Fixed** that way. No test: the master key is cached per process, so
  whether this call reached it cannot be seen from a test in the same binary.
- Phase 6: the editor's per-value auto refresh (`ZedisEditor::start_auto_refresh`)
  reloads the open value on its interval in a background tab and in an
  unattended app, where the key tree's auto refresh, the heartbeat and the
  sampling panels stop (ADR 5) — `handle_auto_refresh` asks `is_background()`,
  `reload_value` does not.
  **Fixed**: the loop skips a round while `is_background()`;
  `a_values_auto_refresh_waits_while_its_tab_is_in_the_background`
  (`views/editor.rs`) counts the reloads in front, in the background and
  back in front.
- Phase 6: confirms that destroy something and are not `.danger()`, so
  Return goes ahead where the one-key delete's answers Cancel:
  - the key tree's folder delete (`key_tree/folder_delete.rs`), which deletes
    every key under the prefix;
  - the topology's Forget node (`topology/cluster_nodes.rs`) — the terminal
    classifies `CLUSTER FORGET` as destructive (`DangerKind::ClusterReset`);
  - Sentinel's Remove and Reset (`topology/sentinel.rs`);
  - the clients panel's Kill, Kill selected and Kill by filter
    (`clients_manager.rs`).

  The first two are the clearest; each is one change with a UI test that
  Return cancels.
  **Fixed**: all five are `.danger()` with the verb on OK (the clients
  panel's three through one `confirm_kill`). `ZedisDialog` has its own test
  that a destructive confirm answers Return with Cancel and a click with OK
  (`a_destructive_confirm_answers_return_with_cancel`), and each confirm one
  that it is opened in that shape — the folder delete in
  `the_trees_actions_ask_what_they_have_to`, Forget, Reset and Remove in
  `forgetting_a_node_and_resetting_or_removing_a_master_are_destructive_confirms`,
  Kill in `a_kill_is_asked_as_a_destructive_confirm`.
- Phase 6: the Sentinel confirms' bodies are English in every locale — the
  Failover, Reset and Remove prompts in `topology/sentinel.rs` are `format!`
  strings, where their titles are `i18n_topology` keys. Seen while fixing the
  confirms above.
  **Fixed**: `topology.sentinel_{failover,reset,remove}_confirm_body` in all
  eight locales, with the master's name as `%{name}`, read like
  `forget_confirm_body`. A body is drawn text, not an accessibility label, so
  no UI test reads it; what holds it is `tests/locale_keys.rs`, whose orphan
  check fails as soon as a body is a `format!` again (one put back in a
  worktree: `every_en_key_is_reachable_from_source` names the key).
