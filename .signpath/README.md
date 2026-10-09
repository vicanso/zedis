# Windows code signing (SignPath)

How the Windows release binaries get their Authenticode signature, what lives
where, and what to do on release day. The user-facing side is the *Code
signing policy* section of the README; this file is the maintainer's side.

## Where onboarding stands (2026-10-09)

Delete this section once release signing is live. Nothing in it blocks an
ordinary release: until step 5 below a tag ships an unsigned Windows build
with a warning, as it always has.

**Done**

- SignPath accepted the project and created the organization `Zedis [OSS]`
  with a self-signed test certificate, the project `zedis`, the policies
  `test-signing` and `release-signing`, and the CI user `CI builds`.
- GitHub: the secret `SIGNPATH_API_TOKEN` and the variable
  `SIGNPATH_ORGANIZATION_ID` are set, and SignPath's GitHub App is installed
  on this repository.
- `artifact-configuration.xml` is saved as the project's default
  configuration and checked against real files: a signing request submitted
  by hand on `test-signing`, with the released v0.12.4 `zedis.exe` and
  `zedis.msi` zipped at the root, was processed successfully.
- `test-signing` lists `CI builds` (and the maintainer) as Submitters.

**Waiting on SignPath** (support e-mailed on 2026-10-09)

- The predefined *GitHub.com* trusted build system cannot be added from this
  organization: its *Trusted Build Systems* page has no button to add one,
  and *Link* on the project's *Trusted Build Systems* tab offers only
  AppVeyor. Until GitHub.com is linked to the project, do not start the dry
  run — a rejected request fails the Windows job of that nightly.
- `release-signing` is shown as INVALID. Asked whether that is only the
  production certificate that is not there yet.

**Then, in this order**

1. Link GitHub.com to the project (project → *Trusted Build Systems* →
   *Link*) once SignPath has made it available.
2. Dry run on the test certificate (*4. Dry run*).
3. Send SignPath the two signing request links. They review the setup, then
   order and import the production certificate.
4. Check `release-signing`: valid, the production certificate assigned,
   `CI builds` as Submitter, the maintainer as Approver, origin restricted
   to this repository's `v*` tags.
5. Set the repository variable `SIGNPATH_RELEASE_SIGNING` to `true`
   (*5. Switch release signing on*).
6. First signed release: approve both requests within 30 minutes
   (*Release day*), then file the winget manifest that was waiting for a
   signed installer.

## What is signed

| Shipped file | Contents | Signature |
| --- | --- | --- |
| `zedis-windows-<arch>.zip` | `zedis.exe` | Authenticode on the exe |
| `zedis-windows-<arch>.msi` | installer, `PFiles\zedis\zedis.exe` inside | Authenticode on the embedded exe **and** on the MSI |
| `zedis-windows-<arch>.msi.zip` | the signed MSI | the MSI's own signature |

Both architectures (x86_64, aarch64) go through the same steps as two
independent signing requests. Nightly builds are not signed.

The signer shown by Windows is **SignPath Foundation**, the certificate
holder. The description inside the signature — what a UAC prompt shows as
the program name — is not ours to choose: the open source subscription
writes it itself, and its URL, and rejects an artifact configuration that
sets `description` or `description-url` on `<authenticode-sign>`. The
configuration therefore says only *what* to sign.

## The pipeline (`.github/workflows/publish.yml`, `windows` job)

1. `release` builds `zedis.exe` and packs `zedis.msi` — unchanged.
2. `resolve SignPath policy` picks the policy: tag push → `release-signing`;
   `workflow_dispatch` with `signpath_policy=test-signing` → `test-signing`;
   nightly → none. With the repository variable `SIGNPATH_ORGANIZATION_ID`
   unset the build ships unsigned with a `::warning::` (the state before
   onboarding finishes); set without the `SIGNPATH_API_TOKEN` secret fails.
   A tag is signed only when `SIGNPATH_RELEASE_SIGNING` is `true` as well —
   the switch for the stretch between the dry run and the production
   certificate (see *5. Switch release signing on*); until then a tag ships
   unsigned with a `::warning::`, as it did before onboarding.
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

Approval is not the certificate. SignPath creates the organization
(`Zedis [OSS]`) with a **self-signed test certificate** (`Test certificate
2026`, purpose *Test signing*, subject and issuer both `Test certificate for
'Zedis [OSS]'`) and nothing Windows trusts. The certificate issued to
SignPath Foundation is ordered and imported only after they have reviewed a
setup that signs — so everything below, and the dry run after it, is done on
the test certificate first.

Two e-mails arrive, in either order: the invitation to the organization, and
a request to confirm the CI user's e-mail address. Create the SignPath
account, accept the invitation with it, and only then confirm the CI user's
address — the confirmation cannot be completed before the invitation is
accepted. Then, on app.signpath.io:

1. **GitHub App** — install [SignPath's GitHub App](https://github.com/apps/signpath)
   on `vicanso/zedis`. It is the source of the "built by this repository"
   verification; without it every request is rejected as untrusted.
2. **Trusted build system** — add the predefined *GitHub.com* build system
   to the organization (*Trusted Build Systems* in the side bar), then link
   it on the project's *Trusted Build Systems* tab. That is what SignPath's
   documentation says; in this organization neither place offered GitHub.com
   and SignPath had to be asked (see *Where onboarding stands*).
3. **Project** — slug **`zedis`** (the workflow hardcodes it), repository URL
   `https://github.com/vicanso/zedis`.
4. **Artifact configuration** — paste `artifact-configuration.xml` from this
   directory as the project's default configuration (the project's
   *Artifact Configurations* tab; it comes with one named `Initial version`
   that signs a single exe and has to be replaced). *Edit* on that row
   changes only the name, slug and description, and *Open XML* only shows
   the XML. Saving validates it — that is where the subscription's rule
   against `description` / `description-url` shows up. Keep the file and the
   pasted copy identical; the file is the reviewed one.
5. **Signing policies** — the project comes with `test-signing` (self-signed
   test certificate, no approval) and `release-signing` (SignPath Foundation
   certificate, manual approval). The slugs must stay exactly those two.
   On `release-signing`, restrict the origin to this repository and to tag
   refs (`v*`) so a build from a branch cannot be release-signed.
   `release-signing` is shown as INVALID until the production certificate
   is assigned to it; do not make it valid with the test certificate.
   The workflow names the *policies*, never a certificate: the test
   certificate's own slug (`test_certificate_2026`) appears nowhere in this
   repository. What has to hold is that `test-signing` uses it.
6. **API token** — the CI user (the one whose address was confirmed above;
   create one if the organization came without) needs the *Submitter* role
   on both signing policies. Generate its API token and store it as the
   repository secret **`SIGNPATH_API_TOKEN`** — the secret first.
7. **Organization id** — from the organization's settings page (also the
   GUID in the app.signpath.io URL). Store it as the repository variable
   **`SIGNPATH_ORGANIZATION_ID`**, after the secret: the variable is what
   turns signing on, and set without the secret it fails the build. It turns
   on the dry run only; tags stay unsigned until step 5.

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

The nested path is the one thing in `artifact-configuration.xml` that only
SignPath can confirm, and a dry run is half an hour. The quicker check, and
the one to repeat whenever `wix/main.wxs` moves a file: on the project's
*Artifact Configurations* tab, *Sign artifact* on the default
configuration's row, policy `test-signing`, and upload a zip with a released
`zedis.exe` and `zedis.msi` at its root — the layout the workflow uploads. A
request that is processed has found every path the configuration names; a
wrong one fails and names the path. (An interactive user can submit only
while they are among the policy's Submitters.)

When the dry run has signed both architectures, write back to SignPath with
the signing request links: that is what their review of the setup looks at
before the production certificate is ordered.

### 5. Switch release signing on

Once SignPath has imported the production certificate and `release-signing`
uses it, set the repository variable **`SIGNPATH_RELEASE_SIGNING`** to
`true`. From the next tag on, the two `windows` jobs wait for an approval
(see *Release day*). Not before: a tag signed against a policy with no
certificate fails the Windows job, and the checksums, `latest.json` and the
mirror all wait on it.

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
| `::warning::SIGNPATH_RELEASE_SIGNING is not 'true'` on a tag build | expected until the production certificate is imported; afterwards the variable was never set (step 5) and the release shipped unsigned |
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
