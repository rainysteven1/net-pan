use ratatui::style::Color;

use crate::provider::ProviderId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Theme {
    pub accent: Color,
    pub on_accent: Color,
    pub focus: Color,
    pub ok: Color,
    pub warn: Color,
    pub err: Color,
    pub comment: Color,
    pub surface: Color,
    pub fg_strong: Color,
}

impl Theme {
    pub(super) const fn application() -> Self {
        let mut theme = Self::with_accent(Color::Black, Color::White);
        theme.focus = Color::Gray;
        theme
    }

    const fn with_accent(accent: Color, on_accent: Color) -> Self {
        Self {
            accent,
            on_accent,
            focus: accent,
            ok: Color::Rgb(80, 250, 123),
            warn: Color::Rgb(241, 250, 140),
            err: Color::Rgb(255, 85, 85),
            comment: Color::Rgb(98, 114, 164),
            surface: Color::Rgb(68, 71, 90),
            fg_strong: Color::Rgb(248, 248, 242),
        }
    }
}

pub(super) fn for_provider(provider: Option<&ProviderId>) -> Theme {
    match provider.map(ProviderId::as_str) {
        Some("google") => Theme::with_accent(Color::Rgb(66, 133, 244), Color::White),
        Some("aliyun") => Theme::with_accent(Color::Rgb(255, 184, 108), Color::Black),
        _ => Theme::with_accent(Color::Rgb(139, 233, 253), Color::Black),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_have_distinct_accents() {
        let google = ProviderId::new("google").unwrap();
        let aliyun = ProviderId::new("aliyun").unwrap();

        assert_ne!(for_provider(Some(&google)), for_provider(Some(&aliyun)));
        assert_ne!(
            Theme::application().accent,
            for_provider(Some(&google)).accent
        );
        assert_ne!(
            Theme::application().accent,
            for_provider(Some(&aliyun)).accent
        );
        assert_eq!(Theme::application().accent, Color::Black);
    }
}
