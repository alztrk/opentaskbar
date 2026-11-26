//! Bundled assets: Inter font weights, 14 locale flag PNGs, 6 bundled theme packs and licenses.
//!
//! `EmbeddedAssets` plugs directly into `gpui::Application::with_assets`; `load_bundled_fonts`
//! registers the four Inter weights with GPUI's text system. All files are copied verbatim from
//! the C# project (`Fonts/`, `Services/Localization/Flags/`, `themes/`).

use std::borrow::Cow;

use gpui::{App, AssetSource, SharedString};

/// Inter family name exposed to GPUI styles.
pub const INTER_FAMILY: &str = "Inter";

pub const FONT_REGULAR: &[u8] = include_bytes!("../../../../Fonts/Inter-Regular.ttf");
pub const FONT_MEDIUM: &[u8] = include_bytes!("../../../../Fonts/Inter-Medium.ttf");
pub const FONT_SEMIBOLD: &[u8] = include_bytes!("../../../../Fonts/Inter-SemiBold.ttf");
pub const FONT_BOLD: &[u8] = include_bytes!("../../../../Fonts/Inter-Bold.ttf");
pub const FONT_LICENSE: &[u8] = include_bytes!("../../../../Fonts/LICENSE.txt");

/// All four embedded font weights, in ascending weight order.
pub fn bundled_fonts() -> [&'static [u8]; 4] {
    [FONT_REGULAR, FONT_MEDIUM, FONT_SEMIBOLD, FONT_BOLD]
}

/// Registers Inter with GPUI. Call once from app startup before creating windows.
pub fn load_bundled_fonts(cx: &mut App) -> gpui::Result<()> {
    let fonts = bundled_fonts().into_iter().map(Cow::Borrowed).collect();
    cx.text_system().add_fonts(fonts)
}

pub const FLAG_LOCALES: [&str; 14] = [
    "de-DE", "en-US", "es-ES", "fr-FR", "it-IT", "ja-JP", "ko-KR", "nl-NL", "pl-PL", "pt-BR",
    "ru-RU", "tr-TR", "uk-UA", "zh-CN",
];

/// Looks up an embedded flag by locale name (e.g. `tr-TR`). Case-insensitive.
pub fn flag_bytes(locale: &str) -> Option<&'static [u8]> {
    match locale.to_ascii_lowercase().as_str() {
        "de-de" => Some(include_bytes!("../../../../Assets/Flags/de-DE.png")),
        "en-us" => Some(include_bytes!("../../../../Assets/Flags/en-US.png")),
        "es-es" => Some(include_bytes!("../../../../Assets/Flags/es-ES.png")),
        "fr-fr" => Some(include_bytes!("../../../../Assets/Flags/fr-FR.png")),
        "it-it" => Some(include_bytes!("../../../../Assets/Flags/it-IT.png")),
        "ja-jp" => Some(include_bytes!("../../../../Assets/Flags/ja-JP.png")),
        "ko-kr" => Some(include_bytes!("../../../../Assets/Flags/ko-KR.png")),
        "nl-nl" => Some(include_bytes!("../../../../Assets/Flags/nl-NL.png")),
        "pl-pl" => Some(include_bytes!("../../../../Assets/Flags/pl-PL.png")),
        "pt-br" => Some(include_bytes!("../../../../Assets/Flags/pt-BR.png")),
        "ru-ru" => Some(include_bytes!("../../../../Assets/Flags/ru-RU.png")),
        "tr-tr" => Some(include_bytes!("../../../../Assets/Flags/tr-TR.png")),
        "uk-ua" => Some(include_bytes!("../../../../Assets/Flags/uk-UA.png")),
        "zh-cn" => Some(include_bytes!("../../../../Assets/Flags/zh-CN.png")),
        _ => None,
    }
}

pub fn flag_path(locale: &str) -> String {
    format!("flags/{locale}.png")
}

pub const BUNDLED_THEME_IDS: [&str; 6] = [
    "catppuccin",
    "gruvbox",
    "nord",
    "prism",
    "solarized",
    "tokyo-night",
];

/// All embedded asset paths (fonts, flags, theme files, licenses).
pub fn all_paths() -> Vec<&'static str> {
    let mut paths = vec![
        "fonts/Inter-Regular.ttf",
        "fonts/Inter-Medium.ttf",
        "fonts/Inter-SemiBold.ttf",
        "fonts/Inter-Bold.ttf",
        "fonts/LICENSE.txt",
    ];
    paths.extend(FLAG_LOCALES.iter().map(|locale| match *locale {
        "de-DE" => "flags/de-DE.png",
        "en-US" => "flags/en-US.png",
        "es-ES" => "flags/es-ES.png",
        "fr-FR" => "flags/fr-FR.png",
        "it-IT" => "flags/it-IT.png",
        "ja-JP" => "flags/ja-JP.png",
        "ko-KR" => "flags/ko-KR.png",
        "nl-NL" => "flags/nl-NL.png",
        "pl-PL" => "flags/pl-PL.png",
        "pt-BR" => "flags/pt-BR.png",
        "ru-RU" => "flags/ru-RU.png",
        "tr-TR" => "flags/tr-TR.png",
        "uk-UA" => "flags/uk-UA.png",
        "zh-CN" => "flags/zh-CN.png",
        _ => unreachable!(),
    }));
    for id in BUNDLED_THEME_IDS {
        paths.push(theme_json_path(id));
        paths.push(widget_pack_path(id));
    }
    paths.push("themes/prism/LICENSE");
    paths.push("themes/prism/README.md");
    paths
}

pub fn theme_json_path(id: &str) -> &'static str {
    match id {
        "catppuccin" => "themes/catppuccin/theme.json",
        "gruvbox" => "themes/gruvbox/theme.json",
        "nord" => "themes/nord/theme.json",
        "prism" => "themes/prism/theme.json",
        "solarized" => "themes/solarized/theme.json",
        "tokyo-night" => "themes/tokyo-night/theme.json",
        _ => "",
    }
}

pub fn widget_pack_path(id: &str) -> &'static str {
    match id {
        "catppuccin" => "themes/catppuccin/widgets/pack.json",
        "gruvbox" => "themes/gruvbox/widgets/pack.json",
        "nord" => "themes/nord/widgets/pack.json",
        "prism" => "themes/prism/widgets/pack.json",
        "solarized" => "themes/solarized/widgets/pack.json",
        "tokyo-night" => "themes/tokyo-night/widgets/pack.json",
        _ => "",
    }
}

/// Embedded asset source. Pass to `gpui::Application::with_assets(EmbeddedAssets)`.
#[derive(Clone, Copy, Debug, Default)]
pub struct EmbeddedAssets;

impl AssetSource for EmbeddedAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let key = path.replace('\\', "/");
        let bytes: Option<&'static [u8]> = match key.as_str() {
            "fonts/Inter-Regular.ttf" => Some(FONT_REGULAR),
            "fonts/Inter-Medium.ttf" => Some(FONT_MEDIUM),
            "fonts/Inter-SemiBold.ttf" => Some(FONT_SEMIBOLD),
            "fonts/Inter-Bold.ttf" => Some(FONT_BOLD),
            "fonts/LICENSE.txt" => Some(FONT_LICENSE),
            "flags/de-DE.png" => Some(include_bytes!("../../../../Assets/Flags/de-DE.png")),
            "flags/en-US.png" => Some(include_bytes!("../../../../Assets/Flags/en-US.png")),
            "flags/es-ES.png" => Some(include_bytes!("../../../../Assets/Flags/es-ES.png")),
            "flags/fr-FR.png" => Some(include_bytes!("../../../../Assets/Flags/fr-FR.png")),
            "flags/it-IT.png" => Some(include_bytes!("../../../../Assets/Flags/it-IT.png")),
            "flags/ja-JP.png" => Some(include_bytes!("../../../../Assets/Flags/ja-JP.png")),
            "flags/ko-KR.png" => Some(include_bytes!("../../../../Assets/Flags/ko-KR.png")),
            "flags/nl-NL.png" => Some(include_bytes!("../../../../Assets/Flags/nl-NL.png")),
            "flags/pl-PL.png" => Some(include_bytes!("../../../../Assets/Flags/pl-PL.png")),
            "flags/pt-BR.png" => Some(include_bytes!("../../../../Assets/Flags/pt-BR.png")),
            "flags/ru-RU.png" => Some(include_bytes!("../../../../Assets/Flags/ru-RU.png")),
            "flags/tr-TR.png" => Some(include_bytes!("../../../../Assets/Flags/tr-TR.png")),
            "flags/uk-UA.png" => Some(include_bytes!("../../../../Assets/Flags/uk-UA.png")),
            "flags/zh-CN.png" => Some(include_bytes!("../../../../Assets/Flags/zh-CN.png")),
            "themes/catppuccin/theme.json" => {
                Some(include_bytes!("../../../../themes/catppuccin/theme.json"))
            }
            "themes/catppuccin/widgets/pack.json" => Some(include_bytes!(
                "../../../../themes/catppuccin/widgets/pack.json"
            )),
            "themes/gruvbox/theme.json" => {
                Some(include_bytes!("../../../../themes/gruvbox/theme.json"))
            }
            "themes/gruvbox/widgets/pack.json" => Some(include_bytes!(
                "../../../../themes/gruvbox/widgets/pack.json"
            )),
            "themes/nord/theme.json" => Some(include_bytes!("../../../../themes/nord/theme.json")),
            "themes/nord/widgets/pack.json" => {
                Some(include_bytes!("../../../../themes/nord/widgets/pack.json"))
            }
            "themes/prism/theme.json" => {
                Some(include_bytes!("../../../../themes/prism/theme.json"))
            }
            "themes/prism/widgets/pack.json" => {
                Some(include_bytes!("../../../../themes/prism/widgets/pack.json"))
            }
            "themes/prism/LICENSE" => Some(include_bytes!("../../../../themes/prism/LICENSE")),
            "themes/prism/README.md" => Some(include_bytes!("../../../../themes/prism/README.md")),
            "themes/solarized/theme.json" => {
                Some(include_bytes!("../../../../themes/solarized/theme.json"))
            }
            "themes/solarized/widgets/pack.json" => Some(include_bytes!(
                "../../../../themes/solarized/widgets/pack.json"
            )),
            "themes/tokyo-night/theme.json" => {
                Some(include_bytes!("../../../../themes/tokyo-night/theme.json"))
            }
            "themes/tokyo-night/widgets/pack.json" => Some(include_bytes!(
                "../../../../themes/tokyo-night/widgets/pack.json"
            )),
            _ => None,
        };
        Ok(bytes.map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        let prefix = path.replace('\\', "/");
        let prefix = prefix.trim_end_matches('/');
        let entries: Vec<SharedString> = all_paths()
            .into_iter()
            .filter(|candidate| {
                prefix.is_empty()
                    || candidate.starts_with(&format!("{prefix}/"))
                    || candidate
                        .strip_prefix(prefix)
                        .is_some_and(|rest| rest.starts_with('/'))
            })
            .map(SharedString::from)
            .collect();
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_assets_embedded() {
        assert_eq!(bundled_fonts().len(), 4);
        assert!(bundled_fonts().iter().all(|bytes| bytes.len() > 100_000));
        assert_eq!(FLAG_LOCALES.len(), 14);
        assert!(
            FLAG_LOCALES
                .iter()
                .all(|locale| flag_bytes(locale).is_some())
        );
        assert_eq!(all_paths().len(), 5 + 14 + 12 + 2);
    }

    #[test]
    fn asset_source_lists_and_loads_every_path() {
        let assets = EmbeddedAssets;
        let paths = all_paths();
        for path in paths {
            assert!(assets.load(path).unwrap().is_some(), "missing {path}");
        }
        assert_eq!(assets.list("themes").unwrap().len(), 14);
        assert_eq!(assets.list("flags").unwrap().len(), 14);
        assert!(assets.load("flags/xx-XX.png").unwrap().is_none());
    }
}
