use crate::color::blend;
use crate::color::is_light;
use crate::color::perceptual_distance;
use crossterm::style::Color as CrosstermColor;
use crossterm::style::query_background_color;

const LIGHT_BG_ALPHA: f32 = 0.01;
const DARK_BG_ALPHA: f32 = 0.04;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnsiColor {
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StdoutColorLevel {
    TrueColor,
    Ansi256,
    Ansi16,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackgroundSource {
    Queried,
    Unavailable,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TintDiagnostics {
    pub stdout_color_level: StdoutColorLevel,
    pub query_error: Option<&'static str>,
    pub queried_bg: Option<(u8, u8, u8)>,
    pub bg_source: BackgroundSource,
    pub effective_bg: Option<(u8, u8, u8)>,
    pub is_light: Option<bool>,
    pub overlay: Option<(u8, u8, u8)>,
    pub alpha: Option<f32>,
    pub blended_rgb: Option<(u8, u8, u8)>,
    pub final_color: Option<AnsiColor>,
}

pub fn stdout_color_level() -> StdoutColorLevel {
    match supports_color::on_cached(supports_color::Stream::Stdout) {
        Some(level) if level.has_16m => StdoutColorLevel::TrueColor,
        Some(level) if level.has_256 => StdoutColorLevel::Ansi256,
        Some(_) => StdoutColorLevel::Ansi16,
        None => StdoutColorLevel::Unknown,
    }
}

pub fn user_message_bg() -> Option<AnsiColor> {
    tint_diagnostics().final_color
}

#[cfg(test)]
pub fn user_message_bg_for(
    query_result: std::io::Result<Option<(u8, u8, u8)>>,
) -> Option<AnsiColor> {
    tint_diagnostics_for(query_result).final_color
}

pub fn tint_diagnostics() -> TintDiagnostics {
    tint_diagnostics_for(query_background_rgb())
}

pub fn tint_diagnostics_for(
    query_result: std::io::Result<Option<(u8, u8, u8)>>,
) -> TintDiagnostics {
    let stdout_color_level = stdout_color_level();
    let (query_error, terminal_bg, bg_source) = match query_result {
        Ok(Some(bg)) => (None, Some(bg), BackgroundSource::Queried),
        Ok(None) => (None, None, BackgroundSource::Unavailable),
        Err(_) => (
            Some("query_background_color failed"),
            None,
            BackgroundSource::Failed,
        ),
    };

    let Some(bg) = terminal_bg else {
        return TintDiagnostics {
            stdout_color_level,
            query_error,
            queried_bg: None,
            bg_source,
            effective_bg: None,
            is_light: None,
            overlay: None,
            alpha: None,
            blended_rgb: None,
            final_color: None,
        };
    };

    let light = is_light(bg);
    let (overlay, alpha) = if light {
        ((0, 0, 0), LIGHT_BG_ALPHA)
    } else {
        ((255, 255, 255), DARK_BG_ALPHA)
    };
    let blended_rgb = blend(overlay, bg, alpha);

    TintDiagnostics {
        stdout_color_level,
        query_error,
        queried_bg: Some(bg),
        bg_source,
        effective_bg: Some(bg),
        is_light: Some(light),
        overlay: Some(overlay),
        alpha: Some(alpha),
        blended_rgb: Some(blended_rgb),
        final_color: best_color_for(blended_rgb, stdout_color_level),
    }
}

#[cfg(test)]
pub fn best_color(target: (u8, u8, u8)) -> Option<AnsiColor> {
    best_color_for(target, stdout_color_level())
}

fn best_color_for(target: (u8, u8, u8), level: StdoutColorLevel) -> Option<AnsiColor> {
    match level {
        StdoutColorLevel::TrueColor => Some(AnsiColor::Rgb(target.0, target.1, target.2)),
        StdoutColorLevel::Ansi256 => nearest_xterm_color(target).map(AnsiColor::Indexed),
        StdoutColorLevel::Ansi16 | StdoutColorLevel::Unknown => None,
    }
}

pub fn query_background_rgb() -> std::io::Result<Option<(u8, u8, u8)>> {
    query_background_color().map(|color| color.and_then(color_to_tuple))
}

fn color_to_tuple(color: CrosstermColor) -> Option<(u8, u8, u8)> {
    match color {
        CrosstermColor::Rgb { r, g, b } => Some((r, g, b)),
        _ => None,
    }
}

fn nearest_xterm_color(target: (u8, u8, u8)) -> Option<u8> {
    xterm_fixed_colors()
        .min_by(|(_, a), (_, b)| {
            perceptual_distance(*a, target)
                .partial_cmp(&perceptual_distance(*b, target))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(index, _)| index)
}

fn xterm_fixed_colors() -> impl Iterator<Item = (u8, (u8, u8, u8))> {
    let cube_steps = [0, 95, 135, 175, 215, 255];

    let cube = (0u8..216u8).map(move |offset| {
        let r = cube_steps[(offset / 36) as usize];
        let g = cube_steps[((offset / 6) % 6) as usize];
        let b = cube_steps[(offset % 6) as usize];
        (16 + offset, (r, g, b))
    });

    let grayscale = (0u8..24u8).map(|offset| {
        let value = 8 + offset * 10;
        (232 + offset, (value, value, value))
    });

    cube.chain(grayscale)
}

#[cfg(test)]
mod tests {
    use super::AnsiColor;
    use super::BackgroundSource;
    use super::best_color;
    use super::tint_diagnostics_for;
    use super::user_message_bg_for;

    #[test]
    fn returns_none_without_terminal_background() {
        assert_eq!(user_message_bg_for(Ok(None)), None);
    }

    #[test]
    fn picks_truecolor_or_ansi_color() {
        let result = best_color((252, 252, 252));
        assert!(matches!(
            result,
            Some(AnsiColor::Rgb(_, _, _)) | Some(AnsiColor::Indexed(_)) | None
        ));
    }

    #[test]
    fn exposes_blended_rgb_in_diagnostics() {
        let diagnostics = tint_diagnostics_for(Ok(Some((255, 255, 255))));
        assert_eq!(diagnostics.queried_bg, Some((255, 255, 255)));
        assert_eq!(diagnostics.blended_rgb, Some((252, 252, 252)));
    }

    #[test]
    fn exposes_query_failure() {
        let diagnostics = tint_diagnostics_for(Err(std::io::Error::other("boom")));
        assert_eq!(diagnostics.bg_source, BackgroundSource::Failed);
        assert_eq!(diagnostics.final_color, None);
    }
}
