# Patched dependencies

Copies of dependencies swapped in through `[patch.crates-io]` in the root `Cargo.toml`: upstream has fixed them, but our dependency chain cannot take that version yet. They are not workspace members (`[workspace] exclude`), so `make lint`, `make lint-fast` and `make test` leave them alone, and `typos.toml` skips `patches/**`. Once upstream can be used, delete the directory and its `[patch.crates-io]` line, then `cargo update -p <crate>`.

## accesskit_macos 0.26.3: closing a window crashes on Macs with a Touch Bar

**What happens**: on a Mac with a Touch Bar (a MacBookPro17,1, say), closing a second window — About Zedis, a secondary window — ends the app, on both engines (the default `gpui-pre` and the GPUI Fast build), with

```text
*** Terminating app due to uncaught exception 'NSRangeException', reason: 'Cannot remove an observer
<_NSTouchBarFinderObservation 0x…> for the key path "nextResponder" from <NSView 0x…> because it is
not registered as an observer.'
  -[_NSTouchBarFinderObservation invalidate]
  ___NSTouchBarFinderSetNeedsUpdateOnMain_block_invoke_2
  NSDisplayCycleObserverInvoke
```

Macs without a Touch Bar never see it.

**Why**: both of GPUI's macOS backends attach `accesskit_macos::SubclassingAdapter` to a window's content view, which gives the view a dynamic subclass (`AccessKitSubclassOf…`). With a Touch Bar, AppKit observes the key window's responders with KVO (`nextResponder`), and KVO subclasses that subclass in turn (`NSKVONotifying_…`). When the window closes GPUI drops the adapter, and 0.26.3's `Drop` sets the view's class back unconditionally with `object_setClass`, taking KVO's subclass away while its observers are registered; when the Touch Bar later lets go of them (on its next update, at no fixed moment), AppKit throws.

**Upstream**:

- AccessKit [issue #823](https://github.com/accesskit/accesskit/issues/823) (the same crash and analysis), [PR #835](https://github.com/accesskit/accesskit/pull/835) (the fix, merged 2026-10-23), released in **accesskit_macos 0.27.2** (2026-10-25). There is no 0.26.x release with it.
- gpui-kit [issue #3192](https://github.com/longbridge/gpui-kit/issues/3192) and Zed [issue #64819](https://github.com/zed-industries/zed/issues/64819) / [PR #65186](https://github.com/zed-industries/zed/pull/65186) (dropping the dynamic subclass altogether) report the same crash; all open as of 2026-10-26.

**Why not 0.27.2 itself**: gpui-fast-macos and gpui-pre-macos (as of 0.1.11 / 0.3.10) ask for `accesskit_macos = "0.26"` and `accesskit = "0.24"`, and 0.27.2 needs accesskit 0.25. `[patch]` cannot widen a requirement (pointed at 0.27.2, Cargo says the patch was not used), and 0.27.2 given a 0.26 version number does not compile: GPUI's `TreeUpdate`, `ActivationHandler` and `ActionHandler` are accesskit 0.24's types.

**What the copy is**: the 0.26.3 sources from crates.io as published, but `src/subclass.rs`, which is PR #835's: its change to `adapters/macos/src/subclass.rs` applied as it is, which leaves the file byte for byte the same as 0.27.2's `src/subclass.rs` (the PR's new tests are not carried over). The fix: dropping the adapter looks at the view's real class (`object_getClass`; `[view class]` is what KVO says it is), and when KVO has subclassed ours it leaves the class alone and frees AccessKit's state only; our methods, finding the state gone, hand the call to the original class. Without KVO it sets the class back as before.

To make the copy again:

```sh
gh pr diff 835 --repo accesskit/accesskit \
  | awk '/^diff --git a\/adapters\/macos\/src\/subclass.rs/{p=1;print;next} /^diff --git/{p=0} p' > subclass.diff
# in the accesskit_macos-0.26.3 sources:
patch -p3 < subclass.diff
```

**Checking it without a Touch Bar**: Xcode's Touch Bar simulator — Window → Touch Bar → Show Touch Bar (⇧⌘8) — makes AppKit treat every app as on a Mac with one.

- Without the patch: open About Zedis from the app menu and close it with ⌘W: the first time ends the app, with the exception above.
- With the patch: About Zedis opened and closed repeatedly with the simulator showing, then Quit Zedis, exit code 0, on the default engine and on the GPUI Fast build; with the simulator hidden, the same (through the old path that sets the class back).

**When to remove it**: once the GPUI backends gpui-kit uses depend on accesskit_macos 0.27.2 or later (or stop using `SubclassingAdapter`, as Zed #65186 does). Check with the simulator as above before deleting it. If a gpui-kit update moves to another 0.26.x, Cargo says the patch was not used: make the copy again from that version.
