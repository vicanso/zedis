# 14. Production's writes are a window, not a switch

Date: 2026-09-26 · Status: accepted

## Context

A Prod-tagged entry escalates the confirmation of *destructive* commands and
nothing else: a `SET` or a `DEL` typed into the wrong tab goes straight
through. The two guards that would catch it are both the user's to set and
neither fits: *Confirm Writes* asks on every write, which is why it stays
off, and *read-only* (`SafeMode`) is a switch that, once flipped for one fix,
stays flipped for the rest of the session. The desired shape was named
plainly: production locked by default, unlocked for a quarter of an hour at
a time, locked again by itself.

Two things constrained the design. Any new mechanism had to be one the
browser build enforces as well, since a page-side lock is a suggestion to a
script. And the browser has no channel for a per-command confirmation
today: `BridgeRequest.confirm` is never set, the desktop's dialogs confirm
*before* sending, so a refusal from the bridge (`428`) has nowhere to be
answered from.

## Decision

The lock is `SafeMode` that re-engages. An entry whose writes are locked —
`RedisServer::write_locked()`: its `write_lock` setting, else its Prod tag —
connects in `SafeMode`. The status bar's lock button, on such an entry, does
not toggle: it asks the lock's question (`DangerKind::WriteLocked`, the
server's name on production, like the destructive commands) and opens a
window of `WRITE_UNLOCK_SECS`; the button then shows the minutes left and
closes the window on a click; a timer closes it otherwise and says so, since
the person who opened it may be mid-thought.

The bridge keeps the same window, per account and entry (`policy::Unlocks`),
opened by `POST /v1/servers/{id}/unlock` with the same confirmation and
closed by `DELETE`. A non-read outside the window is answered
`Confirm { WriteLocked }` — a question like the rest, so a script that
answers it (with the name, on production) gets that one command through;
the page never needs the per-command channel, because it opens the window
first through the `unlock_writes` / `lock_writes` seam pair (a no-op on the
desktop, where the app's own `SafeMode` is the enforcement). A window the
bridge refuses to open closes on the page too: a page that believes itself
unlocked while every write is refused would be the worse state.

Inside the window, writes are plain `Allow` and the audit log carries one
`unlocked` line, not one per write — the person confirmed the window, not
each write. A destructive command keeps its own question inside the window.

The form asks one thing of four answers, on a Safety tab of its own —
follow the tag, allowed, locked, read-only — over the two stored fields
(`write_lock`, `readonly`). Read-only and the lock are the same axis at two
strengths, and as two controls they could both be set, which made the lock
button open a window on a read-only entry; as one choice read-only simply
wins (`write_locked()` is false for a read-only entry). A radio rather than
boxes because the default *follows the tag*, which a box could not say: it
would freeze the answer at the moment the form opened, before the tag was
picked. The tag sits first on that tab, so the choice reads downward.

## Consequences

- Behaviour change: an existing Prod entry starts locked after this version.
  README says so, and the entry's *Write lock* setting turns it off.
- The window's length is one constant, `WRITE_UNLOCK_SECS`, used by the
  desktop's timer, the bridge's store and the dialog's wording.
- `SafeMode`'s toggle is unchanged for every other entry; a read-only account
  or ACL user (`StrictReadOnly`) has no window to open and gets no dialog.
- The channel the lock did not need was opened right after, for the
  operations that do: a `ServerDb` may carry the answer a dialog collected
  (`confirmed(name)`), and every bridge request the operation makes sends
  it — flush, folder and multi-key delete, `CONFIG SET`, a confirmed
  terminal line. *Confirm Writes* stays the terminal's rule on both sides:
  the bridge applies it to session requests only, which are the terminal's,
  since an editor's write is a question the page could not answer.
