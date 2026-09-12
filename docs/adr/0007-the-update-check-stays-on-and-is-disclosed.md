# 7. The update check stays on by default, and the code signing policy discloses it

Date: 2026-09-12 · Status: accepted

## Context

Windows builds are Authenticode-signed through SignPath Foundation's free
open-source programme. Its terms require a *Code signing policy* section in
the README with either a link to a privacy policy or the stock sentence
"This program will not transfer any information to other networked systems
unless specifically requested by the user or the person installing or
operating it."

Zedis checks for updates on startup by default: at most once every two days
(`UPDATE_CHECK_INTERVAL`), a plain GET of the release manifest from GitHub
Releases with the app version as the `User-Agent`, switchable off in
Settings. Nobody asked for that request, so the stock sentence would be
false. SECURITY.md's scope paragraph, written before the updater existed,
already made the same false claim ("no outbound network calls unless you
configure the AI analysis").

Two ways out: make the check opt-in — a first-launch prompt, off until the
user says yes — or keep it on and write down what it actually does.

## Decision

Keep the check on by default and disclose it. The README's *Code signing
policy* carries a privacy statement that starts from the stock sentence and
lists the two exceptions: the throttled update check (what is sent, how to
turn it off, that the download only happens on a click) and the AI endpoint
(only after the user configures one, only the text they hand it). That
section is the privacy policy SignPath's terms allow instead of the stock
sentence; SECURITY.md points at it.

Why not opt-in: the request carries nothing about the user or their data;
the switch, the throttle and the manual check already exist; and a prompt
before the first window is one more dialog most people click through
without reading — the disclosure in the policy is what a reviewer or a
corporate installer actually reads. Should SignPath's review insist on
opt-in, the fallback is a choice on the welcome dialog (`pending_welcome`
in `main.rs`) that writes `auto_update_check` — a dialog change, nothing
underneath it.

## Consequences

- The privacy statement is part of the product. A feature that makes an
  unprompted network request — telemetry, crash upload, remote images, a
  default AI endpoint — changes the statement in README.md, README_zh.md
  and SECURITY.md in the same PR, or does not ship.
- The updater's request stays minimal: no machine identifiers, no install
  id, nothing beyond the version in the `User-Agent`. A richer request is a
  policy change, not an implementation detail.
- Tooling that reads the policy (SignPath's reviewers, package maintainers)
  can take the section at its word; the runbook in `.signpath/README.md`
  keeps the maintainer's side in step.
