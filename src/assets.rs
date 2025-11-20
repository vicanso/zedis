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
            .ok_or_else(|| anyhow!("could not find asset at path \"{path}\""))
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
        }
        .into()
    }
}

impl From<CustomIconName> for Icon {
    fn from(val: CustomIconName) -> Self {
        Icon::empty().path(val.path())
    }
}
