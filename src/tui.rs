//! Interactive terminal UI: a list of misspellings with a detail pane showing
//! context and numbered suggestions. File edits are buffered in memory and
//! written back on save/quit; working-dictionary additions persist immediately.
//!
//! Keys: `j`/`k`/`n`/`N` move · `1`-`9` replace with suggestion · `r` replace
//! manually · `i` ignore (session) · `a` add lowercase · `A` add exact-case ·
//! `s` save · `q` save+quit · `Q` discard+quit · `?` help

use std::collections::{HashMap, HashSet};
use std::io::{self, Stdout};

use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListState, Paragraph, Wrap},
    Frame, Terminal,
};

use crate::check::Misspelling;
use crate::engine::Engine;

type Backend = CrosstermBackend<Stdout>;

const CONTEXT_LINES: usize = 4;

pub fn run(miss: Vec<Misspelling>, engine: Engine) -> Result<()> {
    let mut app = App::new(miss, engine)?;

    enable_raw_mode()?;
    // Guard: restore the terminal even if the event loop panics.
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        }
    }
    let _restore = Restore;

    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let result = app.main_loop(&mut terminal);
    // Normal-path teardown (the guard covers the panic case).
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture);
    result
}

#[derive(Debug)]
enum Mode {
    Normal,
    /// Manual replacement buffer.
    Replace(String),
}

#[derive(Clone)]
struct Entry {
    path: std::path::PathBuf,
    current_offset: usize,
    word_len: usize,
    word: String,
    suggestions: Vec<String>,
}

/// An in-memory editable copy of a checked file with a line-offset index.
struct FileBuf {
    text: String,
    line_starts: Vec<usize>,
}

impl FileBuf {
    fn new(text: String) -> Self {
        let line_starts = compute_line_starts(&text);
        Self { text, line_starts }
    }

    fn replace(&mut self, start: usize, end: usize, with: &str) {
        self.text.replace_range(start..end, with);
        self.line_starts = compute_line_starts(&self.text);
    }

    /// (0-based line, byte column within that line)
    fn locate(&self, byte_offset: usize) -> (usize, usize) {
        let line = self
            .line_starts
            .partition_point(|&s| s <= byte_offset)
            .saturating_sub(1);
        (line, byte_offset - self.line_starts[line])
    }

    fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Text of a 0-based line, without its trailing newline.
    fn line_text(&self, ln: usize) -> &str {
        let start = self.line_starts[ln];
        let end = if ln + 1 < self.line_starts.len() {
            // strip the trailing '\n'
            self.line_starts[ln + 1] - 1
        } else {
            self.text.len()
        };
        let end = end.min(self.text.len());
        // also strip a trailing '\r' (CRLF)
        let end = if end > start && self.text.as_bytes().get(end - 1) == Some(&b'\r') {
            end - 1
        } else {
            end
        };
        &self.text[start..end]
    }
}

fn compute_line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

struct App {
    entries: Vec<Entry>,
    cursor: usize,
    files: HashMap<std::path::PathBuf, FileBuf>,
    dirty: HashSet<std::path::PathBuf>,
    engine: Engine,
    mode: Mode,
    show_help: bool,
    message: Option<String>,
    quit: bool,
}

impl App {
    fn new(miss: Vec<Misspelling>, engine: Engine) -> Result<Self> {
        let mut files: HashMap<std::path::PathBuf, FileBuf> = HashMap::new();
        let mut entries = Vec::with_capacity(miss.len());
        for m in miss {
            let path = m.path.clone();
            if !files.contains_key(&path) {
                match std::fs::read_to_string(&path) {
                    Ok(text) => {
                        files.insert(path.clone(), FileBuf::new(text));
                    }
                    Err(e) => {
                        eprintln!("redink: skipping {}: {e}", path.display());
                        continue;
                    }
                }
            }
            entries.push(Entry {
                path,
                current_offset: m.byte_offset,
                word_len: m.word.len(),
                word: m.word,
                suggestions: m.suggestions,
            });
        }
        Ok(App {
            entries,
            cursor: 0,
            files,
            dirty: HashSet::new(),
            engine,
            mode: Mode::Normal,
            show_help: false,
            message: None,
            quit: false,
        })
    }

    fn main_loop(&mut self, terminal: &mut Terminal<Backend>) -> Result<()> {
        while !self.quit {
            terminal.draw(|f| self.draw(f))?;
            if self.entries.is_empty() && self.message.is_none() {
                self.message = Some("nothing left — press s to save, q to quit".into());
            }
            let ev = event::read()?;
            if let Event::Key(key) = ev {
                self.handle_key(key);
            }
        }
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if self.show_help {
            self.show_help = false;
            return;
        }
        if matches!(self.mode, Mode::Replace(_)) {
            self.handle_replace_key(key);
            return;
        }
        match key.code {
            KeyCode::Char('q') => {
                self.save_all();
                self.quit = true;
            }
            KeyCode::Char('Q') => {
                self.quit = true;
            }
            KeyCode::Char('s') => {
                let n = self.save_all();
                self.message = Some(format!("saved {n} file(s)"));
            }
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char('j') | KeyCode::Down | KeyCode::Char('n') => self.cursor_next(),
            KeyCode::Char('k') | KeyCode::Up | KeyCode::Char('N') => self.cursor_prev(),
            KeyCode::Char('i') => self.ignore(),
            KeyCode::Char('a') => self.add_ci(),
            KeyCode::Char('A') => self.add_cs(),
            KeyCode::Char('r') => {
                if let Some(w) = self.cur_word() {
                    self.mode = Mode::Replace(w.to_string());
                }
            }
            KeyCode::Char(c @ '1'..='9') => {
                let n = (c as u8 - b'1' + 1) as usize;
                if let Some(sug) = self.cur_suggestion(n) {
                    self.apply_replacement(&sug);
                }
            }
            KeyCode::Esc => self.message = None,
            _ => {}
        }
    }

    fn handle_replace_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                let w = match &self.mode {
                    Mode::Replace(b) => b.clone(),
                    _ => unreachable!(),
                };
                self.mode = Mode::Normal;
                if !w.is_empty() {
                    self.apply_replacement(&w);
                }
            }
            KeyCode::Backspace => {
                if let Mode::Replace(b) = &mut self.mode {
                    b.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Mode::Replace(b) = &mut self.mode {
                    b.push(c);
                }
            }
            _ => {}
        }
    }

    fn cursor_next(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        self.cursor = (self.cursor + 1).min(self.entries.len() - 1);
    }

    fn cursor_prev(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn cur_word(&self) -> Option<&str> {
        self.entries.get(self.cursor).map(|e| e.word.as_str())
    }

    fn cur_suggestion(&self, n: usize) -> Option<String> {
        let e = self.entries.get(self.cursor)?;
        e.suggestions.get(n - 1).cloned()
    }

    /// Replace the current word with `new_word`, shifting later offsets.
    fn apply_replacement(&mut self, new_word: &str) {
        let idx = self.cursor;
        let entry = match self.entries.get(idx).cloned() {
            Some(e) => e,
            None => return,
        };
        let start = entry.current_offset;
        let end = start + entry.word_len;
        let path = entry.path.clone();
        let delta = new_word.len() as isize - entry.word_len as isize;

        if let Some(buf) = self.files.get_mut(&path) {
            buf.replace(start, end, new_word);
        }
        self.dirty.insert(path.clone());

        self.entries.remove(idx);
        if delta != 0 {
            for e in self.entries.iter_mut() {
                if e.path == path && e.current_offset >= end {
                    e.current_offset =
                        (e.current_offset as isize + delta).max(0) as usize;
                }
            }
        }
        if self.cursor >= self.entries.len() && !self.entries.is_empty() {
            self.cursor = self.entries.len() - 1;
        }
        self.message = Some(format!("replaced with \u{201c}{new_word}\u{201d}"));
    }

    fn ignore(&mut self) {
        let Some(word) = self.cur_word().map(str::to_string) else {
            return;
        };
        self.engine.ignore_session(&word);
        self.retain_unresolved();
        self.message = Some(format!("ignored \u{201c}{word}\u{201d} for this session"));
    }

    fn add_ci(&mut self) {
        let Some(word) = self.cur_word().map(str::to_string) else {
            return;
        };
        self.engine.add_ci(&word);
        self.persist_working("add");
        self.retain_unresolved();
        self.message = Some(format!(
            "added \u{201c}{word}\u{201d} (any case) to {}",
            self.engine.working_path().display()
        ));
    }

    fn add_cs(&mut self) {
        let Some(word) = self.cur_word().map(str::to_string) else {
            return;
        };
        self.engine.add_cs(&word);
        self.persist_working("add");
        self.retain_unresolved();
        self.message = Some(format!(
            "added \u{201c}{word}\u{201d} (exact case) to {}",
            self.engine.working_path().display()
        ));
    }

    fn persist_working(&mut self, _label: &str) {
        if let Err(e) = self.engine.save_working() {
            self.message = Some(format!("could not save working dict: {e}"));
        }
    }

    /// Drop entries now accepted by any dictionary layer; keep cursor valid.
    fn retain_unresolved(&mut self) {
        let engine = &self.engine;
        self.entries.retain(|e| !engine.check(&e.word));
        if self.cursor >= self.entries.len() {
            self.cursor = self.entries.len().saturating_sub(1);
        }
    }

    fn save_all(&mut self) -> usize {
        let dirty: Vec<_> = self.dirty.iter().cloned().collect();
        let n = dirty.len();
        for path in &dirty {
            let Some(buf) = self.files.get(path) else {
                continue;
            };
            if let Err(e) = std::fs::write(path, &buf.text) {
                self.message = Some(format!("save failed for {}: {e}", path.display()));
            }
        }
        self.dirty.clear();
        let _ = self.engine.save_working();
        n
    }

    fn draw(&mut self, f: &mut Frame<'_>) {
        let area = f.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),  // title
                Constraint::Min(5),    // body
                Constraint::Length(2), // footer
            ])
            .split(area);

        self.draw_title(f, chunks[0]);

        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(chunks[1]);
        self.draw_list(f, body[0]);
        self.draw_detail(f, body[1]);

        self.draw_footer(f, chunks[2]);

        if self.show_help {
            self.draw_help(f);
        }
    }

    fn draw_title(&self, f: &mut Frame<'_>, area: Rect) {
        let total = self.entries.len();
        let pos = if total == 0 {
            0
        } else {
            self.cursor.min(total - 1) + 1
        };
        let title = format!(
            " redink \u{2014} error {} of {} \u{2014} dict: {}",
            pos,
            total,
            self.engine.working_path().display(),
        );
        let para = Paragraph::new(title).style(Style::default().add_modifier(Modifier::REVERSED));
        f.render_widget(para, area);
    }

    fn draw_list(&mut self, f: &mut Frame<'_>, area: Rect) {
        let items: Vec<Line> = self
            .entries
            .iter()
            .map(|e| {
                let line = self
                    .files
                    .get(&e.path)
                    .map(|b| b.locate(e.current_offset).0 + 1)
                    .unwrap_or(0);
                Line::from(format!(
                    "{}:{}  {}",
                    e.path.display(),
                    line,
                    e.word
                ))
            })
            .collect();

        let mut state = ListState::default();
        state.select(if self.entries.is_empty() {
            None
        } else {
            Some(self.cursor.min(self.entries.len() - 1))
        });

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title("misspellings"))
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("\u{25b6} ");
        f.render_stateful_widget(list, area, &mut state);
    }

    fn draw_detail(&self, f: &mut Frame<'_>, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(11)])
            .split(area);

        // Context
        let context = self.context_lines();
        let ctx_block = Block::default().borders(Borders::ALL).title("context");
        let para = Paragraph::new(context).wrap(Wrap { trim: false }).block(ctx_block);
        f.render_widget(para, chunks[0]);

        // Suggestions / input
        let block = Block::default().borders(Borders::ALL).title("suggestions");
        let para = Paragraph::new(self.suggestion_lines()).block(block);
        f.render_widget(para, chunks[1]);
    }

    fn context_lines(&self) -> Vec<Line<'static>> {
        let Some(entry) = self.entries.get(self.cursor) else {
            return vec![Line::from("(no misspellings)")];
        };
        let Some(buf) = self.files.get(&entry.path) else {
            return vec![Line::from("(file unavailable)")];
        };
        let (line0, col0) = buf.locate(entry.current_offset);
        let start = line0.saturating_sub(CONTEXT_LINES);
        let end = (line0 + CONTEXT_LINES + 1).min(buf.line_count());

        let mut out = Vec::new();
        for ln in start..end {
            let text = buf.line_text(ln);
            if ln == line0 {
                let mid = col0.min(text.len());
                let after = (col0 + entry.word_len).min(text.len());
                let before = &text[..mid];
                let word = &text[mid..after];
                let tail = &text[after..];
                out.push(Line::from(vec![
                    Span::raw(format!("{:>4} ", ln + 1)),
                    Span::raw(before.to_string()),
                    Span::styled(
                        word.to_string(),
                        Style::default()
                            .fg(Color::White)
                            .bg(Color::Red)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(tail.to_string()),
                ]));
            } else {
                out.push(Line::from(format!("{:>4} {}", ln + 1, text)));
            }
        }
        out
    }

    fn suggestion_lines(&self) -> Vec<Line<'static>> {
        if let Mode::Replace(buf) = &self.mode {
            return vec![Line::from(format!("replace with: {}_", buf))];
        }
        let Some(entry) = self.entries.get(self.cursor) else {
            return vec![Line::from("(nothing selected)")];
        };
        if entry.suggestions.is_empty() {
            return vec![
                Line::from("(no suggestions)"),
                Line::from(""),
                Line::from("r \u{2014} type a replacement"),
            ];
        }
        let mut lines = Vec::new();
        for (i, s) in entry.suggestions.iter().take(9).enumerate() {
            lines.push(Line::from(format!(" {}) {}", i + 1, s)));
        }
        lines
    }

    fn draw_footer(&self, f: &mut Frame<'_>, area: Rect) {
        let hint = "j/k move \u{00b7} 1-9 replace \u{00b7} r replace \u{00b7} i ignore \u{00b7} a/A add \u{00b7} s save \u{00b7} q quit \u{00b7} ? help";
        let dirty = if self.dirty.is_empty() {
            String::new()
        } else {
            format!(" \u{00b7} {} unsaved", self.dirty.len())
        };
        let msg = self.message.clone().unwrap_or_default();
        let line1 = Line::from(format!("{hint}{dirty}"));
        let line2 = if msg.is_empty() {
            Line::from("")
        } else {
            Line::from(msg)
        };
        let para = Paragraph::new(vec![line1, line2]);
        f.render_widget(para, area);
    }

    fn draw_help(&self, f: &mut Frame<'_>) {
        let area = centered(60, 70, f.area());
        let text = vec![
            Line::from("redink \u{2014} keybindings"),
            Line::from(""),
            Line::from("  j k n N    move between misspellings"),
            Line::from("  1-9         replace with Nth suggestion"),
            Line::from("  r           type a replacement (Enter/Esc)"),
            Line::from("  i           ignore this word for the session"),
            Line::from("  a           add lowercase (case-insensitive)"),
            Line::from("  A           add exact-case (case-sensitive)"),
            Line::from("  s           save all edited files now"),
            Line::from("  q           save and quit"),
            Line::from("  Q           discard edits and quit"),
            Line::from(""),
            Line::from("press any key to close"),
        ];
        let para = Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL).title("help"))
            .alignment(ratatui::layout::Alignment::Center);
        f.render_widget(para, area);
    }
}

fn centered(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let pop = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area)[1];
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(pop)[1]
}
