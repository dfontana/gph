use std::env;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Terminal;
use tui_textarea::TextArea;

struct State {
    file_path: PathBuf,
    owns_temp: bool,
    source: String,
    textarea: TextArea<'static>,
    render: Result<(Vec<u8>, usize, usize), String>,
    error_msg: Option<String>,
}

pub fn run(file: Option<PathBuf>) -> Result<(), String> {
    let mut state = init_state(file)?;

    enable_raw_mode().map_err(|e| e.to_string())?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(|e| e.to_string())?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(|e| e.to_string())?;

    let result = event_loop(&mut terminal, &mut state);

    crate::kitty::delete_all(terminal.backend_mut());
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = disable_raw_mode();

    if state.owns_temp {
        let _ = std::fs::remove_file(&state.file_path);
    }

    result
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut State,
) -> Result<(), String> {
    loop {
        let sz = terminal.size().map_err(|e| e.to_string())?;
        let size = Rect::new(0, 0, sz.width, sz.height);
        let right_pane = compute_right_pane_inner(size);

        terminal
            .draw(|frame| draw_frame(frame, state))
            .map_err(|e| e.to_string())?;

        crate::kitty::delete_all(terminal.backend_mut());
        render_kitty_in_pane(state, right_pane, terminal.backend_mut());

        if !event::poll(Duration::from_millis(100)).map_err(|e| e.to_string())? {
            continue;
        }

        match event::read().map_err(|e| e.to_string())? {
            Event::Key(KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }) => break,
            Event::Key(KeyEvent {
                code: KeyCode::Char('q'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }) => break,
            // Ctrl-E: open $EDITOR (overrides Emacs end-of-line; use End key instead)
            Event::Key(KeyEvent {
                code: KeyCode::Char('e'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }) => {
                open_editor(state, terminal)?;
            }
            Event::Resize(_, _) => {
                terminal.clear().map_err(|e| e.to_string())?;
            }
            ev => {
                if state.textarea.input(ev) {
                    state.source = state.textarea.lines().join("\n");
                    save_file(state)?;
                    rerender(state);
                }
            }
        }
    }
    Ok(())
}

fn init_state(file: Option<PathBuf>) -> Result<State, String> {
    let (file_path, owns_temp) = match file {
        Some(p) => (p, false),
        None => {
            let tmp = env::temp_dir().join(format!("gph-tui-{}.gph", std::process::id()));
            std::fs::write(&tmp, "(graph lr\n  (-> a b)\n)\n")
                .map_err(|e| format!("failed to create temp file: {e}"))?;
            (tmp, true)
        }
    };

    let source = std::fs::read_to_string(&file_path)
        .map_err(|e| format!("cannot read '{}': {e}", file_path.display()))?;
    let textarea = make_textarea(&source, &file_path);

    let mut state = State {
        file_path,
        owns_temp,
        source,
        textarea,
        render: Err("not rendered yet".into()),
        error_msg: None,
    };
    rerender(&mut state);
    Ok(state)
}

fn make_textarea(content: &str, path: &Path) -> TextArea<'static> {
    let lines: Vec<String> = content.lines().map(str::to_owned).collect();
    let mut ta = TextArea::from(lines);
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("untitled");
    ta.set_block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {filename} ")),
    );
    ta.set_line_number_style(Style::default().fg(Color::DarkGray));
    ta
}

fn save_file(state: &State) -> Result<(), String> {
    let mut content = state.source.clone();
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    std::fs::write(&state.file_path, content).map_err(|e| format!("save failed: {e}"))
}

fn rerender(state: &mut State) {
    match crate::render_to_rgba(&state.source) {
        Ok(data) => {
            state.render = Ok(data);
            state.error_msg = None;
        }
        Err(msg) => {
            state.error_msg = Some(msg);
        }
    }
}

fn open_editor(
    state: &mut State,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<(), String> {
    save_file(state)?;

    execute!(terminal.backend_mut(), LeaveAlternateScreen).map_err(|e| e.to_string())?;
    disable_raw_mode().map_err(|e| e.to_string())?;

    let editor = env::var("EDITOR")
        .or_else(|_| env::var("VISUAL"))
        .unwrap_or_else(|_| "vi".into());

    Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"{}\"", state.file_path.display()))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| format!("failed to launch editor '{editor}': {e}"))?;

    enable_raw_mode().map_err(|e| e.to_string())?;
    execute!(terminal.backend_mut(), EnterAlternateScreen).map_err(|e| e.to_string())?;
    terminal.clear().map_err(|e| e.to_string())?;

    state.source =
        std::fs::read_to_string(&state.file_path).map_err(|e| format!("reload failed: {e}"))?;
    state.textarea = make_textarea(&state.source, &state.file_path);
    rerender(state);
    Ok(())
}

fn draw_frame(frame: &mut ratatui::Frame, state: &State) {
    let area = frame.area();

    let halves = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let left_split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(halves[0]);

    // Left: textarea editor
    frame.render_widget(&state.textarea, left_split[0]);

    // Status bar
    let status = match &state.error_msg {
        Some(msg) => Span::styled(format!(" {msg} "), Style::default().fg(Color::Red)),
        None => Span::styled(
            "  Ctrl-E=open in $EDITOR  Ctrl-Q=quit  ",
            Style::default().fg(Color::DarkGray),
        ),
    };
    frame.render_widget(
        Paragraph::new(Line::from(status)).style(Style::default().bg(Color::Black)),
        left_split[1],
    );

    // Right: preview border — kitty renders inside after the frame flush
    frame.render_widget(
        Block::default().borders(Borders::ALL).title(" Preview "),
        halves[1],
    );
}

fn compute_right_pane_inner(size: Rect) -> Rect {
    let halves = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(size);
    Block::default().borders(Borders::ALL).inner(halves[1])
}

fn render_kitty_in_pane(state: &State, pane: Rect, out: &mut impl Write) {
    let Ok((rgba, pw, ph)) = &state.render else {
        return;
    };
    if pane.width == 0 || pane.height == 0 {
        return;
    }
    let _ = write!(out, "\x1b[{};{}H", pane.y + 1, pane.x + 1);
    crate::kitty::display_in_pane(rgba, *pw, *ph, pane.width, pane.height, out);
}
