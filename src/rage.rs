use crate::backend;
use crate::render;
use crate::terminal_palette;
use crate::terminal_palette::AnsiColor;
use anyhow::Context;
use anyhow::Result;
use crossterm::terminal;
use std::env;
use std::ffi::OsString;
use std::io;
use std::io::IsTerminal;
use std::io::Write;
use std::path::Path;

pub fn run(cwd: &Path, args: &[OsString]) -> Result<i32> {
    let stdout_is_tty = io::stdout().is_terminal();
    let terminal_size = terminal::size().ok();
    let render_mode = render::RenderMode::detect();
    let detection = backend::detect_details(cwd);
    let tint = terminal_palette::tint_diagnostics_for(terminal_palette::query_background_rgb());

    let mut out = String::new();
    out.push_str("mdiff rage\n");
    out.push_str(&format!("cwd: {}\n", cwd.display()));
    out.push_str(&format!("backend: {:?}\n", detection.backend));
    out.push_str(&format!(
        "repository root: {}\n",
        detection
            .root
            .as_deref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<none>".into())
    ));
    out.push_str(&format!(
        "backend command: {}\n",
        detection.backend.command_preview(args)
    ));
    out.push_str(&format!("stdout is tty: {stdout_is_tty}\n"));
    out.push_str(&format!(
        "terminal size: {}\n",
        terminal_size
            .map(|(cols, rows)| format!("{cols}x{rows}"))
            .unwrap_or_else(|| "<unknown>".into())
    ));
    out.push_str(&format!(
        "side by side: {} (threshold > 120, width={})\n",
        render_mode.side_by_side, render_mode.width
    ));
    out.push_str(&format!(
        "supports-color stdout level: {:?}\n",
        tint.stdout_color_level
    ));
    out.push_str(&format!(
        "crossterm bg query error: {}\n",
        tint.query_error.unwrap_or("<none>")
    ));
    out.push_str(&format!(
        "queried bg (8-bit rgb): {}\n",
        format_rgb(tint.queried_bg)
    ));
    out.push_str(&format!("bg source: {:?}\n", tint.bg_source));
    out.push_str(&format!(
        "effective bg: {}\n",
        format_rgb(tint.effective_bg)
    ));
    out.push_str(&format!(
        "is light background: {}\n",
        tint.is_light
            .map(|value| value.to_string())
            .unwrap_or_else(|| "<unknown>".into())
    ));
    out.push_str(&format!("overlay rgb: {}\n", format_rgb(tint.overlay)));
    out.push_str(&format!(
        "overlay alpha: {}\n",
        tint.alpha
            .map(|value| format!("{value:.2}"))
            .unwrap_or_else(|| "<none>".into())
    ));
    out.push_str(&format!(
        "blended tint rgb: {}\n",
        format_rgb(tint.blended_rgb)
    ));
    out.push_str(&format!(
        "final tint color: {}\n",
        format_ansi_color(tint.final_color)
    ));

    for name in [
        "TERM",
        "COLORTERM",
        "COLORFGBG",
        "TERM_PROGRAM",
        "TMUX",
        "INSIDE_EMACS",
    ] {
        out.push_str(&format!("env {name}: {}\n", format_env(name)));
    }

    io::stdout()
        .write_all(out.as_bytes())
        .context("failed to write rage output")?;
    io::stdout()
        .flush()
        .context("failed to flush rage output")?;

    Ok(0)
}

fn format_env(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| "<unset>".into())
}

fn format_rgb(rgb: Option<(u8, u8, u8)>) -> String {
    rgb.map(|(r, g, b)| format!("({r}, {g}, {b})"))
        .unwrap_or_else(|| "<none>".into())
}

fn format_ansi_color(color: Option<AnsiColor>) -> String {
    match color {
        Some(AnsiColor::Rgb(r, g, b)) => format!("Rgb({r}, {g}, {b})"),
        Some(AnsiColor::Indexed(index)) => format!("Indexed({index})"),
        None => "<none>".into(),
    }
}
