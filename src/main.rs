mod backend;
mod color;
mod pager;
mod rage;
mod render;
mod terminal_palette;
mod unified_diff;

use anyhow::Context;
use anyhow::Result;
use std::env;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::io;
use std::io::Write;

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            eprintln!("mdiff: {err:#}");
            std::process::exit(2);
        }
    }
}

fn run() -> Result<i32> {
    let mut args: Vec<OsString> = env::args_os().skip(1).collect();
    let cwd = env::current_dir().context("failed to determine the current directory")?;
    let rage_mode = take_flag(&mut args, OsStr::new("--rage"));
    let force_pager =
        take_flag(&mut args, OsStr::new("-P")) || take_flag(&mut args, OsStr::new("--pager"));
    if rage_mode {
        return rage::run(&cwd, &args);
    }

    let detection = backend::detect_details(&cwd);
    let backend = detection.backend;
    let output = backend
        .run(&args)
        .with_context(|| format!("failed to run {}", backend.describe()))?;

    if !output.stderr.is_empty() {
        io::stderr()
            .write_all(&output.stderr)
            .context("failed to write backend stderr")?;
    }

    let raw_stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let document = unified_diff::parse(&raw_stdout);
    let files = document.file_paths();
    let fetcher = backend::FileFetcher::new(
        backend,
        cwd,
        detection.root,
        args.clone(),
        document.git_right_blob_by_path(),
    );
    let rendered = pager::page_or_render(
        files,
        force_pager,
        |width, file_filter, palette, gap_states, spinner_frame| {
            let filtered = document.filter_files(file_filter);
            if render::should_render_side_by_side(width) {
                render::render_document_with_state(
                    &filtered,
                    width,
                    palette,
                    gap_states,
                    spinner_frame,
                )
            } else {
                render::render_inline_document_with_state(
                    &filtered,
                    palette,
                    gap_states,
                    spinner_frame,
                )
            }
        },
        move |gap| {
            let content = fetcher.fetch_right_file(&gap.id.file_path)?;
            Ok(slice_lines(&content, gap.start_line, gap.line_count))
        },
    )?;

    if let Some(rendered) = rendered {
        io::stdout()
            .write_all(rendered.as_bytes())
            .context("failed to write rendered diff")?;
    }

    io::stdout().flush().context("failed to flush stdout")?;
    io::stderr().flush().context("failed to flush stderr")?;

    Ok(output.status.code().unwrap_or(1))
}

fn take_flag(args: &mut Vec<OsString>, flag: &OsStr) -> bool {
    let original_len = args.len();
    args.retain(|arg| arg != flag);
    args.len() != original_len
}

fn slice_lines(content: &str, start_line: usize, line_count: usize) -> Vec<String> {
    content
        .lines()
        .skip(start_line.saturating_sub(1))
        .take(line_count)
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::slice_lines;
    use super::take_flag;
    use std::ffi::OsStr;
    use std::ffi::OsString;

    #[test]
    fn removes_rage_flag_from_arguments() {
        let mut args = vec![
            OsString::from("--cached"),
            OsString::from("--rage"),
            OsString::from("--stat"),
        ];

        assert!(take_flag(&mut args, OsStr::new("--rage")));
        assert_eq!(
            args,
            vec![OsString::from("--cached"), OsString::from("--stat")]
        );
    }

    #[test]
    fn removes_short_pager_flag_from_arguments() {
        let mut args = vec![
            OsString::from("-P"),
            OsString::from("--cached"),
            OsString::from("--stat"),
        ];

        assert!(take_flag(&mut args, OsStr::new("-P")));
        assert_eq!(
            args,
            vec![OsString::from("--cached"), OsString::from("--stat")]
        );
    }

    #[test]
    fn removes_long_pager_flag_from_arguments() {
        let mut args = vec![
            OsString::from("--cached"),
            OsString::from("--pager"),
            OsString::from("--stat"),
        ];

        assert!(take_flag(&mut args, OsStr::new("--pager")));
        assert_eq!(
            args,
            vec![OsString::from("--cached"), OsString::from("--stat")]
        );
    }

    #[test]
    fn slices_requested_line_range() {
        assert_eq!(
            slice_lines("one\ntwo\nthree\nfour\n", 2, 2),
            vec!["two".to_owned(), "three".to_owned()]
        );
    }
}
