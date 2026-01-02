use anyhow::anyhow;
use gpui::{AssetSource, Result, SharedString};
use gpui_component::Icon;
use gpui_component_assets::Assets as ComponentAssets;
use rust_embed::RustEmbed;
use std::borrow::Cow;

#[derive(RustEmbed)]
#[folder = "assets"]
#[include = "icons/**/*.svg"]
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

pub enum CustomIconName {
    Key,
    DatabaseZap,
    FileXCorner,
    FilePenLine,
    FilePlusCorner,
    ChevronsLeftRightEllipsis,
    ChevronsDown,
    ChevronUp,
    FileCheckCorner,
    Clock3,
    Zap,
    Network,
    Equal,
    Activity,
    Languages,
    RotateCw,
    CircleCheckBig,
    CircleDotDashed,
    X,
    MemoryStick,
    AudioWaveform,
    Binary,
    ALargeSmall,
    ListChecvronsDownUp,
}

impl CustomIconName {
    pub fn path(self) -> SharedString {
        match self {
            CustomIconName::Key => "icons/key.svg",
            CustomIconName::DatabaseZap => "icons/database-zap.svg",
            CustomIconName::FileXCorner => "icons/file-x-corner.svg",
            CustomIconName::FilePenLine => "icons/file-pen-line.svg",
            CustomIconName::FilePlusCorner => "icons/file-plus-corner.svg",
            CustomIconName::ChevronsLeftRightEllipsis => "icons/chevrons-left-right-ellipsis.svg",
            CustomIconName::ChevronsDown => "icons/chevrons-down.svg",
            CustomIconName::ChevronUp => "icons/chevron-up.svg",
            CustomIconName::FileCheckCorner => "icons/file-check-corner.svg",
            CustomIconName::Clock3 => "icons/clock-3.svg",
            CustomIconName::Zap => "icons/zap.svg",
            CustomIconName::Network => "icons/network.svg",
            CustomIconName::Equal => "icons/equal.svg",
            CustomIconName::Activity => "icons/activity.svg",
            CustomIconName::Languages => "icons/languages.svg",
            CustomIconName::RotateCw => "icons/rotate-cw.svg",
            CustomIconName::CircleCheckBig => "icons/circle-check-big.svg",
            CustomIconName::CircleDotDashed => "icons/circle-dot-dashed.svg",
            CustomIconName::X => "icons/x.svg",
            CustomIconName::MemoryStick => "icons/memory-stick.svg",
            CustomIconName::AudioWaveform => "icons/audio-waveform.svg",
            CustomIconName::Binary => "icons/binary.svg",
            CustomIconName::ALargeSmall => "icons/a-large-small.svg",
            CustomIconName::ListChecvronsDownUp => "icons/list-chevrons-down-up.svg",
        }
        .into()
    }
}

impl From<CustomIconName> for Icon {
    fn from(val: CustomIconName) -> Self {
        Icon::empty().path(val.path())
    }
}
