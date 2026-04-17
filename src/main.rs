mod backend;
mod color;
mod pager;
mod rage;
mod render;
mod terminal_palette;
mod unified_diff;

use anyhow::Context;
use anyhow::Result;
use notify::RecommendedWatcher;
use notify::RecursiveMode;
use notify::Watcher;
use std::collections::HashMap;
use std::env;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::io;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[derive(Clone)]
struct DiffSnapshot {
    raw_stdout: String,
    document: unified_diff::Document,
    files: Vec<String>,
    file_line_counts: HashMap<String, usize>,
}

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
    let initial_snapshot = build_snapshot(
        raw_stdout,
        backend,
        &cwd,
        detection.root.as_ref(),
        &args,
    );
    let files = initial_snapshot.files.clone();
    let snapshot = Arc::new(Mutex::new(initial_snapshot));
    let fetch_cwd = cwd.clone();
    let fetch_root = detection.root.clone();
    let fetch_args = args.clone();
    let refresh_rx = spawn_live_refresh(
        backend,
        detection.root.clone().unwrap_or_else(|| cwd.clone()),
        args.clone(),
        cwd.clone(),
        detection.root.clone(),
        Arc::clone(&snapshot),
    );
    let render_snapshot = Arc::clone(&snapshot);
    let fetch_snapshot = Arc::clone(&snapshot);
    let edit_snapshot = Arc::clone(&snapshot);
    let edit_cwd = cwd.clone();
    let edit_root = detection.root.clone();
    let edit_args = args.clone();
    let working_tree_cwd = cwd.clone();
    let working_tree_root = detection.root.clone();
    let working_tree_args = args.clone();
    let list_snapshot = Arc::clone(&snapshot);
    let rendered = pager::page_or_render(
        files,
        force_pager,
        |width, file_filter, palette, gap_states, spinner_frame| {
            let snapshot = render_snapshot.lock().expect("snapshot lock poisoned");
            let filtered = snapshot.document.filter_files(file_filter);
            if render::should_render_side_by_side(width) {
                render::render_document_with_state_and_file_counts(
                    &filtered,
                    width,
                    palette,
                    &snapshot.file_line_counts,
                    gap_states,
                    spinner_frame,
                )
            } else {
                render::render_inline_document_with_state_and_file_counts(
                    &filtered,
                    palette,
                    &snapshot.file_line_counts,
                    gap_states,
                    spinner_frame,
                )
            }
        },
        move |gap| {
            let git_right_blobs = fetch_snapshot
                .lock()
                .expect("snapshot lock poisoned")
                .document
                .git_right_blob_by_path();
            let fetcher = backend::FileFetcher::new(
                backend,
                fetch_cwd.clone(),
                fetch_root.clone(),
                fetch_args.clone(),
                git_right_blobs,
            );
            let content = fetcher.fetch_right_file(&gap.id.file_path)?;
            Ok(slice_lines(&content, gap.start_line, gap.line_count))
        },
        {
            move || {
                list_snapshot
                    .lock()
                    .expect("snapshot lock poisoned")
                    .files
                    .clone()
            }
        },
        move |path| {
            let git_right_blobs = edit_snapshot
                .lock()
                .expect("snapshot lock poisoned")
                .document
                .git_right_blob_by_path();
            let fetcher = backend::FileFetcher::new(
                backend,
                edit_cwd.clone(),
                edit_root.clone(),
                edit_args.clone(),
                git_right_blobs,
            );
            fetcher.resolve_edit_target(path)
        },
        move |path| {
            let fetcher = backend::FileFetcher::new(
                backend,
                working_tree_cwd.clone(),
                working_tree_root.clone(),
                working_tree_args.clone(),
                HashMap::new(),
            );
            fetcher.working_tree_path(path)
        },
        refresh_rx,
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

fn spawn_live_refresh(
    backend: backend::Backend,
    watch_root: PathBuf,
    args: Vec<OsString>,
    cwd: PathBuf,
    root: Option<PathBuf>,
    snapshot: Arc<Mutex<DiffSnapshot>>,
) -> Option<mpsc::Receiver<()>> {
    let config = backend.live_refresh_config(&watch_root, Some(&watch_root), &args)?;
    let (refresh_tx, refresh_rx) = mpsc::channel();

    thread::spawn(move || match config.mode {
        backend::LiveRefreshMode::WatchRecursive => run_watch_refresh_loop(
            backend,
            watch_root,
            args,
            cwd,
            root,
            snapshot,
            refresh_tx,
            config.poll_interval,
        ),
        backend::LiveRefreshMode::Poll => run_poll_refresh_loop(
            backend,
            args,
            cwd,
            root,
            snapshot,
            refresh_tx,
            config.poll_interval,
        ),
    });

    Some(refresh_rx)
}

fn run_watch_refresh_loop(
    backend: backend::Backend,
    watch_root: PathBuf,
    args: Vec<OsString>,
    cwd: PathBuf,
    root: Option<PathBuf>,
    snapshot: Arc<Mutex<DiffSnapshot>>,
    refresh_tx: mpsc::Sender<()>,
    debounce: Duration,
) {
    let (event_tx, event_rx) = mpsc::channel();
    let mut watcher = match RecommendedWatcher::new(
        move |result: notify::Result<notify::Event>| {
            if result.is_ok() {
                let _ = event_tx.send(());
            }
        },
        notify::Config::default(),
    ) {
        Ok(watcher) => watcher,
        Err(_) => {
            run_poll_refresh_loop(backend, args, cwd, root, snapshot, refresh_tx, debounce);
            return;
        }
    };

    if watcher
        .watch(&watch_root, RecursiveMode::Recursive)
        .is_err()
    {
        run_poll_refresh_loop(backend, args, cwd, root, snapshot, refresh_tx, debounce);
        return;
    }

    loop {
        if event_rx.recv().is_err() {
            return;
        }
        while event_rx.recv_timeout(debounce).is_ok() {}
        let _ = refresh_snapshot_if_changed(
            backend,
            &cwd,
            root.as_ref(),
            &args,
            &snapshot,
            &refresh_tx,
        );
    }
}

fn run_poll_refresh_loop(
    backend: backend::Backend,
    args: Vec<OsString>,
    cwd: PathBuf,
    root: Option<PathBuf>,
    snapshot: Arc<Mutex<DiffSnapshot>>,
    refresh_tx: mpsc::Sender<()>,
    interval: Duration,
) {
    loop {
        thread::sleep(interval);
        let _ = refresh_snapshot_if_changed(
            backend,
            &cwd,
            root.as_ref(),
            &args,
            &snapshot,
            &refresh_tx,
        );
    }
}

fn build_snapshot(
    raw_stdout: String,
    backend: backend::Backend,
    cwd: &PathBuf,
    root: Option<&PathBuf>,
    args: &[OsString],
) -> DiffSnapshot {
    let document = unified_diff::parse(&raw_stdout);
    let files = document.file_paths();
    let file_line_counts = build_file_line_counts(backend, cwd, root, args, &document, &files);
    DiffSnapshot {
        raw_stdout,
        document,
        files,
        file_line_counts,
    }
}

fn build_file_line_counts(
    backend: backend::Backend,
    cwd: &PathBuf,
    root: Option<&PathBuf>,
    args: &[OsString],
    document: &unified_diff::Document,
    files: &[String],
) -> HashMap<String, usize> {
    let fetcher = backend::FileFetcher::new(
        backend,
        cwd.clone(),
        root.cloned(),
        args.to_vec(),
        document.git_right_blob_by_path(),
    );

    files.iter()
        .filter_map(|path| {
            fetcher
                .fetch_right_file(path)
                .ok()
                .map(|content| (path.clone(), content.lines().count()))
        })
        .collect()
}

fn refresh_snapshot_if_changed(
    backend: backend::Backend,
    cwd: &PathBuf,
    root: Option<&PathBuf>,
    args: &[OsString],
    snapshot: &Arc<Mutex<DiffSnapshot>>,
    refresh_tx: &mpsc::Sender<()>,
) -> Result<()> {
    let output = backend.run(args)?;
    let raw_stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    let mut snapshot = snapshot.lock().expect("snapshot lock poisoned");
    if snapshot.raw_stdout == raw_stdout {
        return Ok(());
    }

    *snapshot = build_snapshot(raw_stdout, backend, cwd, root, args);
    let _ = refresh_tx.send(());
    Ok(())
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
