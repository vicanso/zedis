# Flathub packaging

Files here are the source of truth for the Flathub submission
(`flathub/io.github.vicanso.zedis` once accepted):

- `io.github.vicanso.zedis.yml` — flatpak-builder manifest
- `io.github.vicanso.zedis.desktop` — desktop entry (Icon must equal the app-id;
  `assets/zedis.desktop` is the AppImage variant and stays untouched)
- `io.github.vicanso.zedis.metainfo.xml` — AppStream metadata shown in software
  centers; add a `<release>` entry per version

## 1. Prepare a release (scripted)

```bash
./scripts/submit-flathub.sh v0.4.7            # pin manifest + generate cargo-sources.json
./scripts/submit-flathub.sh v0.4.7 --submit   # ... and open the flathub/flathub PR via gh
```

This pins the manifest's git source to the tag (fills the `commit:`
placeholder via `git rev-parse <tag>^{}`) and runs
`scripts/gen-flatpak-sources.sh <tag>`, which mirrors every crate in that
tag's `Cargo.lock` (including git dependencies like GPUI) into
`flatpak/cargo-sources.json` — Flathub builders have no network access.

The tag must already contain the `flatpak/` directory (i.e. v0.4.7 or later)
and the release assets must be published.

## 2. Validate + local build (Linux only)

```bash
appstreamcli validate io.github.vicanso.zedis.metainfo.xml
desktop-file-validate io.github.vicanso.zedis.desktop

flatpak install flathub org.freedesktop.Sdk//24.08 \
  org.freedesktop.Sdk.Extension.rust-stable//24.08
flatpak-builder --user --install --force-clean build-dir io.github.vicanso.zedis.yml
flatpak run io.github.vicanso.zedis
```

## 3. Submit

`./scripts/submit-flathub.sh <tag> --submit` automates the flow from
https://docs.flathub.org/docs/for-app-authors/submission: fork
`flathub/flathub`, branch off `new-pr`, add `io.github.vicanso.zedis.yml` +
`cargo-sources.json`, open the PR against the `new-pr` branch. The
`io.github.*` app-id is verified against the GitHub account automatically.

macOS note: flatpak-builder cannot run on macOS, so skip step 2 and let the
Flathub PR's CI do the build — iterate on the PR if it flags anything.

After acceptance, new releases are shipped by updating the tag/commit (and
`cargo-sources.json` + a metainfo `<release>` entry) in the
`flathub/io.github.vicanso.zedis` repo — consider a small CI job for that.

## Note on SSH keys

The sandbox deliberately does not request `~/.ssh`. Users who tunnel with a
key file need a one-time override:

```bash
flatpak override --user --filesystem=~/.ssh:ro io.github.vicanso.zedis
```
