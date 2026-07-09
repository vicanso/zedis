# Zedis website (GitHub Pages)

Static landing page served from this directory.

## Enable Pages

Repository → **Settings → Pages**:

- Source: **Deploy from a branch**
- Branch: `main` (or your default)
- Folder: **`/docs`**

### Custom domain: `zedis.net`

`docs/CNAME` is set to `zedis.net`. After push:

1. **Pages → Custom domain** → enter `zedis.net` → Save  
2. Wait for DNS check (green) → enable **Enforce HTTPS**

**DNS at your registrar** (apex / root domain) — four **A** records for `@` / `zedis.net`:

| Type | Name | Value |
| --- | --- | --- |
| A | `@` | `185.199.108.153` |
| A | `@` | `185.199.109.153` |
| A | `@` | `185.199.110.153` |
| A | `@` | `185.199.111.153` |

Optional **www** (recommended redirect/alias):

| Type | Name | Value |
| --- | --- | --- |
| CNAME | `www` | `vicanso.github.io` |

Then either set Custom domain to `www.zedis.net` and redirect apex→www in DNS/GitHub, or keep apex `zedis.net` and point www as above (GitHub can serve both if configured in Pages).

**Final URLs (after DNS + HTTPS):**

- https://zedis.net/
- https://zedis.net/zh/

Until DNS propagates, the site still works at `https://vicanso.github.io/zedis/`.

## Layout

| Path | Content |
| --- | --- |
| `index.html` | English landing (redirects browser `zh*` → `zh/`) |
| `zh/index.html` | Chinese landing |
| `styles.css` | Shared styles |
| `images/*.png` | Screenshot assets (local; no user-attachments CDN) |
| `FEATURES.md` / `FEATURES_zh.md` | Full feature docs (linked from the site) |

Language preference is stored in `localStorage` key `zedis-lang` (`en` \| `zh`) when the user clicks the language switcher.

## Hero video

Uses the same user-attachments URL as the README:

```text
https://github.com/user-attachments/assets/36135174-16df-473b-8756-ea5931ec3c4b
```

If the video fails to load, the hero falls back to a static screenshot.

**Preview tip:** open via `http://127.0.0.1:...`, not `file://`.

## Local preview

```bash
# from repo root
python3 -m http.server 8080 --directory docs
# open http://127.0.0.1:8080/
```
