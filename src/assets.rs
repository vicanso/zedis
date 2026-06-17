use anyhow::anyhow;
use gpui::{App, AssetSource, Result, SharedString};
use gpui_component::{Icon, ThemeRegistry};
use gpui_component_assets::Assets as ComponentAssets;
use rust_embed::RustEmbed;
use std::borrow::Cow;

#[derive(RustEmbed)]
#[folder = "assets"]
#[include = "icons/**/*.svg"]
#[include = "commands.json"]
#[include = "icon.png"]
#[include = "themes/*.json"]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path.is_empty() {
            return Ok(None);
        }
        if let Some(f) = ComponentAssets::get(path) {
            return Ok(Some(f.data));
        }

        Self::get(path)
            .map(|f| Some(f.data))
            .ok_or_else(|| anyhow!(r#"could not find asset at path "{path}""#))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut files: Vec<SharedString> = ComponentAssets::iter()
            .filter_map(|p| p.starts_with(path).then(|| p.into()))
            .collect();

        files.extend(
            Self::iter()
                .filter_map(|p| p.starts_with(path).then(|| p.into()))
                .collect::<Vec<_>>(),
        );

        Ok(files)
    }
}

/// Register the embedded `assets/themes/*.json` theme sets into the global
/// [`ThemeRegistry`] so they appear in the title-bar theme menu. Adapted from
/// gpui-component's `watch_dir` flow for rust-embedded assets — there is no
/// on-disk themes directory at runtime. Must run after `gpui_component::init`.
pub fn register_themes(cx: &mut App) {
    let registry = ThemeRegistry::global_mut(cx);
    for path in Assets::iter().filter(|p| p.starts_with("themes/") && p.ends_with(".json")) {
        let Some(file) = Assets::get(path.as_ref()) else {
            continue;
        };
        let Ok(content) = std::str::from_utf8(&file.data) else {
            continue;
        };
        if let Err(err) = registry.load_themes_from_str(content) {
            tracing::warn!(theme = %path, error = %err, "failed to load embedded theme");
        }
    }
}

pub enum CustomIconName {
    DatabaseZap,
    FileXCorner,
    FilePenLine,
    FilePlusCorner,
    ChevronsDown,
    ChevronUp,
    FileCheckCorner,
    Clock3,
    Zap,
    Network,
    Equal,
    Activity,
    RotateCw,
    CircleCheckBig,
    CircleDotDashed,
    X,
    MemoryStick,
    AudioWaveform,
    Binary,
    ListChecvronsDownUp,
    Lock,
    LockOpen,
    SwatchBook,
    Eraser,
    Save,
    ListCheck,
    Square,
    SquareCheck,
    ListX,
    Snail,
    Rss,
    Laptop,
    HardDrive,
    Radar,
    SunMoon,
    Download,
    Upload,
    GitCompareArrows,
}

impl CustomIconName {
    pub fn path(self) -> SharedString {
        match self {
            CustomIconName::DatabaseZap => "icons/database-zap.svg",
            CustomIconName::FileXCorner => "icons/file-x-corner.svg",
            CustomIconName::FilePenLine => "icons/file-pen-line.svg",
            CustomIconName::FilePlusCorner => "icons/file-plus-corner.svg",
            CustomIconName::ChevronsDown => "icons/chevrons-down.svg",
            CustomIconName::ChevronUp => "icons/chevron-up.svg",
            CustomIconName::FileCheckCorner => "icons/file-check-corner.svg",
            CustomIconName::Clock3 => "icons/clock-3.svg",
            CustomIconName::Zap => "icons/zap.svg",
            CustomIconName::Network => "icons/network.svg",
            CustomIconName::Equal => "icons/equal.svg",
            CustomIconName::Activity => "icons/activity.svg",
            CustomIconName::RotateCw => "icons/rotate-cw.svg",
            CustomIconName::CircleCheckBig => "icons/circle-check-big.svg",
            CustomIconName::CircleDotDashed => "icons/circle-dot-dashed.svg",
            CustomIconName::X => "icons/x.svg",
            CustomIconName::MemoryStick => "icons/memory-stick.svg",
            CustomIconName::AudioWaveform => "icons/audio-waveform.svg",
            CustomIconName::Binary => "icons/binary.svg",
            CustomIconName::ListChecvronsDownUp => "icons/list-chevrons-down-up.svg",
            CustomIconName::Lock => "icons/lock.svg",
            CustomIconName::LockOpen => "icons/lock-open.svg",
            CustomIconName::SwatchBook => "icons/swatch-book.svg",
            CustomIconName::Eraser => "icons/eraser.svg",
            CustomIconName::Save => "icons/save.svg",
            CustomIconName::ListCheck => "icons/list-check.svg",
            CustomIconName::Square => "icons/square.svg",
            CustomIconName::SquareCheck => "icons/square-check.svg",
            CustomIconName::ListX => "icons/list-x.svg",
            CustomIconName::Snail => "icons/snail.svg",
            CustomIconName::Rss => "icons/rss.svg",
            CustomIconName::Laptop => "icons/laptop.svg",
            CustomIconName::HardDrive => "icons/hard-drive.svg",
            CustomIconName::Radar => "icons/radar.svg",
            CustomIconName::SunMoon => "icons/sun-moon.svg",
            CustomIconName::Download => "icons/download.svg",
            CustomIconName::Upload => "icons/upload.svg",
            CustomIconName::GitCompareArrows => "icons/git-compare-arrows.svg",
        }
        .into()
    }
}

impl From<CustomIconName> for Icon {
    fn from(val: CustomIconName) -> Self {
        Icon::empty().path(val.path())
    }
}
