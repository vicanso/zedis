# Windows code signing (SignPath)

How the Windows release binaries get their Authenticode signature, what lives
where, and what to do on release day. The user-facing side is the *Code
signing policy* section of the README; this file is the maintainer's side.

## What is signed

| Shipped file | Contents | Signature |
| --- | --- | --- |
| `zedis-windows-<arch>.zip` | `zedis.exe` | Authenticode on the exe |
| `zedis-windows-<arch>.msi` | installer, `PFiles\zedis\zedis.exe` inside | Authenticode on the embedded exe **and** on the MSI |
| `zedis-windows-<arch>.msi.zip` | the signed MSI | the MSI's own signature |

Both architectures (x86_64, aarch64) go through the same steps as two
independent signing requests. Nightly builds are not signed.

The signer shown by Windows is **SignPath Foundation**, the certificate
holder; the program name in the UAC prompt comes from the artifact
configuration's `description` (the exe) and the MSI's product name.

## The pipeline (`.github/workflows/publish.yml`, `windows` job)

1. `release` builds `zedis.exe` and packs `zedis.msi` — unchanged.
2. `resolve SignPath policy` picks the policy: tag push → `release-signing`;
   `workflow_dispatch` with `signpath_policy=test-signing` → `test-signing`;
   nightly → none. With the repository variable `SIGNPATH_ORGANIZATION_ID`
   unset the build ships unsigned with a `::warning::` (the state before
   onboarding finishes); set without the `SIGNPATH_API_TOKEN` secret fails.
3. `upload unsigned build for signing` uploads the two files as a GitHub
   Actions artifact. SignPath only signs artifacts: its GitHub App verifies
   that the artifact was produced by this run of this repository.
4. `submit SignPath signing request` submits and waits. `release-signing`
   waits for a person to approve on app.signpath.io (30 minutes, then the
   step times out); `test-signing` returns within a minute.
5. `install signed binaries` replaces `zedis.exe` / `zedis.msi` with the
   signed ones.
6. `verify Authenticode signatures` runs `signtool verify /pa /v` on the exe,
   the MSI, and the exe extracted from the MSI by an administrative install.
   It prints the exe's path inside the MSI — the path
   `artifact-configuration.xml` must name. Under `test-signing` the
   verification failures are expected (the test certificate is untrusted)
   and only warn.
7. The existing smoke test, packaging, checksums (`SHA256SUMS`,
   `latest.json`) and uploads run on the signed files, so the in-app
   updater, Scoop and winget all see the signed MSI.

## One-time setup

### 1. Repository requirements (done — keep them true)

- OSI licence (Apache-2.0), public, not a fork, actively maintained.
- Binaries come from a verifiable CI build of the repository source:
  `publish.yml` on GitHub-hosted runners, from the tagged commit.
- README section **Code signing policy** (both READMEs): the credit line
  *Free code signing provided by SignPath.io, certificate by SignPath
  Foundation*, the committers / reviewers / approvers, and the privacy
  statement. The statement lists every unprompted network path of the app
  (the throttled startup update check and nothing else; the AI endpoint is
  user-configured). A feature that adds one changes README, README_zh and
  SECURITY.md in the same PR — see ADR 7.
- The term *Code signing policy* also appears on the website's Windows
  install card (docs/index.html, docs/zh/index.html) and should stay in the
  release notes footer when releases are written by hand.
- Every account named in the policy has two-factor authentication on GitHub;
  approvers also on app.signpath.io.

### 2. Apply

Form at signpath.org → *Open Source* → apply. Draft answers:

| Field | Answer |
| --- | --- |
| Project name | Zedis |
| Repository | https://github.com/vicanso/zedis |
| Website | https://zedis.net |
| Licence | Apache-2.0 |
| Tagline | A native, GPU-accelerated Redis GUI client built in Rust. |
| Description | Zedis is a Redis / Valkey desktop client (macOS, Windows, Linux) built in Rust on GPUI: it opens million-key databases without freezing, decodes values (JSON, Protobuf, images, compressed data) automatically, and ships tools for cluster, sentinel, replication, memory analysis and migration. |
| Contact | the maintainer's GitHub-verified e-mail |
| CI system | GitHub Actions (`.github/workflows/publish.yml`) |
| Download page | https://github.com/vicanso/zedis/releases (also Homebrew cask `zedis`, Scoop `extras/zedis`, AUR `zedis-bin`; a winget manifest follows the first signed release) |
| Reputation | Started November 2025; ~2,000 GitHub stars, ~50 forks, 5 contributors; 37 releases downloaded ~25,000 times in total (figures as of 2026-09; refresh from the repo's About panel and release assets). Packaged by third parties in Homebrew (https://formulae.brew.sh/cask/zedis), the Scoop Extras bucket (https://github.com/ScoopInstaller/Extras/blob/master/bucket/zedis.json), the AUR (`zedis-bin`) and published on crates.io (`zedis-gui`). Developed in the open (issues, PRs under a CLA, security policy); releases are built on GitHub Actions, the macOS builds already signed and notarized. |
| Code signing policy | https://github.com/vicanso/zedis#-code-signing-policy |
| Users | the download badge on the README plus the package-manager installs |
| Primary discovery channel | GitHub (the repository is the home page and download page; stars, releases and the package-manager listings all point there); website https://zedis.net second |
| External contributions | accepted as pull requests, reviewed and merged by a committer (@vicanso); every contributor signs the CLA |

Review is manual (two to six weeks, questions by e-mail). Expect a question
about the update check: point at the privacy statement — the check is a
plain GET of the release manifest, sends only the app version, and has a
Settings switch.

### 3. Configure the SignPath organization (after approval)

SignPath creates the organization and a user; then, on app.signpath.io:

1. **GitHub App** — install [SignPath's GitHub App](https://github.com/apps/signpath)
   on `vicanso/zedis`. It is the source of the "built by this repository"
   verification; without it every request is rejected as untrusted.
2. **Trusted build system** — organization settings → add the predefined
   *GitHub.com* build system, then link it to the project.
3. **Project** — slug **`zedis`** (the workflow hardcodes it), repository URL
   `https://github.com/vicanso/zedis`.
4. **Artifact configuration** — paste `artifact-configuration.xml` from this
   directory as the project's default configuration. Keep the file and the
   pasted copy identical; the file is the reviewed one.
5. **Signing policies** — the project comes with `test-signing` (self-signed
   test certificate, no approval) and `release-signing` (SignPath Foundation
   certificate, manual approval). The slugs must stay exactly those two.
   On `release-signing`, restrict the origin to this repository and to tag
   refs (`v*`) so a build from a branch cannot be release-signed.
6. **API token** — create a CI user with the *Submitter* role on the project
   and generate its API token. Store it as the repository secret
   **`SIGNPATH_API_TOKEN`**.
7. **Organization id** — from the organization's settings page (also the
   GUID in the app.signpath.io URL). Store it as the repository variable
   **`SIGNPATH_ORGANIZATION_ID`**. Setting it turns signing on.

### 4. Dry run on the test certificate

Actions → *Publish* → *Run workflow* → `signpath_policy` = `test-signing`.
Watch the `windows` jobs:

- `submit SignPath signing request` prints the request URL; the request
  should complete without approval.
- `verify Authenticode signatures` prints `zedis.exe inside the MSI: …`. If
  SignPath rejected the request with a "file not found" for the nested exe,
  make `artifact-configuration.xml` (and the pasted copy) use that printed
  path.
- The `::warning::` lines about failed verification are the untrusted test
  certificate — expected here, fatal under `release-signing`.

The dry run signs the nightly-flavoured build from `main`; the signed files
land on the *Development Build (Nightly)* release like any manual nightly.

## Release day

1. Publish the release as usual (the tag push starts `publish.yml`).
2. The two `windows` jobs stop at *submit SignPath signing request*; SignPath
   mails the approvers. Open app.signpath.io → *Signing requests* and approve
   both (x86_64 and aarch64) within 30 minutes. The rest of the job — verify,
   smoke test, packaging, checksums, upload — continues on its own.
3. If a request timed out before it was approved, deny or cancel it on
   SignPath and re-run the failed job: the artifact name carries the run
   attempt, so the re-run uploads a fresh artifact and submits a new request.

`SHA256SUMS` and `latest.json` are aggregated after all platforms finish, so
a late approval delays the update manifest, not just the Windows assets.

## Troubleshooting

| Symptom | Cause / fix |
| --- | --- |
| `::warning::SIGNPATH_ORGANIZATION_ID is not set` on a tag build | the variable is missing (or the name drifted); the release shipped unsigned |
| `SIGNPATH_API_TOKEN secret is missing` | variable set, secret not — add the token or unset the variable |
| SignPath: artifact origin not trusted | GitHub App not installed on the repo, trusted build system not linked to the project, or the job ran outside GitHub-hosted runners |
| SignPath: file `PFiles/zedis/zedis.exe` not found | MSI layout changed; use the path the verify step prints |
| `Resource not accessible by integration` from the action | the job's `permissions` block lost `actions: read` |
| `signtool.exe not found` | the runner image dropped the Windows SDK; install it with the `microsoft/setup-msbuild`-style tooling or pin the image |
| verify fails under `release-signing` | signature or timestamp missing — inspect the request on SignPath before re-running; never publish the unsigned files by hand |

## If the review objects to the default-on update check

The chosen disclosure (ADR 7) keeps the check on by default and describes it
in the policy. The fallback, if SignPath insists on opt-in, is a choice on
the first-launch welcome dialog (`pending_welcome` in `src/main.rs`) that
writes `auto_update_check`; the setting, the throttle and the manual check
in the title bar already exist, so only the dialog and one locale key per
language change.
