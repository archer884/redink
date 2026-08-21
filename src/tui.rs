//! Interactive terminal UI: a list of misspellings with a detail pane showing
//! context and numbered suggestions. File edits are buffered in memory and
//! written back on save/quit; working-dictionary additions persist immediately.
//!
//! Keys: `j`/`k`/`n`/`N` move · `1`-`9` replace with suggestion · `r` replace
//! manually · `i` ignore (session) · `a` add lowercase · `A` add exact-case ·
//! `h`/`H` add compound · `p` add word/phrase prompt (`=` toggles exact case)
//! · `s` save · `q` save+quit · `Q` discard+quit · `?` help

use std::collections::{HashMap, HashSet};
use std::io::{self, Stdout};

use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListState, Paragraph, Wrap},
};

use crate::check;
use crate::check::{Compound, Misspelling};
use crate::engine::{Engine, SUGGEST_LIMIT, Suggest};
use crate::format::{self, Format};
use crate::token::Tokenized;

type Backend = CrosstermBackend<Stdout>;

pub fn run(miss: Vec<Misspelling>, engine: Engine, format: Format) -> Result<()> {
    let mut app = App::new(miss, engine, format)?;

    // Panic hook: tear the terminal down before the default hook prints, so
    // a panic message is never swallowed by the alternate screen.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        original_hook(info);
    }));

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
    let _ = execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    );
    result
}

#[derive(Debug)]
enum Mode {
    Normal,
    /// Manual replacement buffer.
    Replace(String),
    /// Add word/phrase prompt buffer. `sensitive` (shown as an `=` prefix)
    /// registers the entry for exact casing only.
    Add {
        buf: String,
        sensitive: bool,
    },
}

#[derive(Clone)]
struct Entry {
    path: std::path::PathBuf,
    current_offset: usize,
    word_len: usize,
    word: String,
    suggestions: Option<Vec<String>>,
    /// The whole hyphenated compound this part belongs to, if any, kept
    /// current as edits shift and rewrite the text around it.
    compound: Option<Compound>,
}

/// An in-memory editable copy of a checked file with a line-offset index.
struct FileBuf {
    text: String,
    line_starts: check::LineStarts,
}

impl FileBuf {
    fn new(text: String) -> Self {
        let line_starts = check::LineStarts::new(&text);
        Self { text, line_starts }
    }

    fn replace(&mut self, start: usize, end: usize, with: &str) {
        self.text.replace_range(start..end, with);
        self.line_starts = check::LineStarts::new(&self.text);
    }

    /// (0-based line, byte column within that line)
    fn locate(&self, byte_offset: usize) -> (usize, usize) {
        self.line_starts.locate(byte_offset)
    }
}

/// Tokenize file text exactly as the check pipeline does (Markdown-aware
/// skips included), for phrase-context re-checks after dictionary changes.
/// Takes the same `format` the scan ran with — resolving `Auto` here instead
/// would silently disagree with the scan whenever `--format` was given.
fn file_tokens(text: &str, path: &std::path::Path, format: Format) -> Tokenized {
    let skip = format::skip_ranges(text, format.resolve(path));
    crate::token::tokenize_with_lowercase(text, &skip)
}

/// Collapse every run of whitespace (including newlines) into a single space,
/// so a character window spanning paragraph boundaries reads as one flowing
/// line of prose around the misspelled word.
fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    out
}

struct App {
    entries: Vec<Entry>,
    cursor: usize,
    files: HashMap<std::path::PathBuf, FileBuf>,
    dirty: HashSet<std::path::PathBuf>,
    engine: Engine,
    /// The format the scan ran with, reused for phrase-context re-checks.
    format: Format,
    suggest_cache: HashMap<String, Vec<String>>,
    /// Per-file tokenizations for phrase-context re-checks; invalidated for
    /// a file whenever its text changes.
    token_cache: HashMap<std::path::PathBuf, Option<Tokenized>>,
    mode: Mode,
    show_help: bool,
    message: Option<String>,
    quit: bool,
}

impl App {
    fn new(miss: Vec<Misspelling>, engine: Engine, format: Format) -> Result<Self> {
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
                compound: m.compound,
            });
        }
        Ok(App {
            entries,
            cursor: 0,
            files,
            dirty: HashSet::new(),
            engine,
            format,
            suggest_cache: HashMap::new(),
            token_cache: HashMap::new(),
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
        if key.kind != KeyEventKind::Press {
            return;
        }
        // Ctrl-C cancels from any mode: discard edits and quit immediately.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        if self.show_help {
            self.show_help = false;
            return;
        }
        if matches!(self.mode, Mode::Add { .. }) {
            self.handle_add_key(key);
            return;
        }
        if matches!(self.mode, Mode::Replace(_)) {
            self.handle_replace_key(key);
            return;
        }
        match key.code {
            KeyCode::Char('q') => match self.save_all() {
                Ok(_) => self.quit = true,
                Err(e) => {
                    self.message = Some(format!(
                        "save failed (not quitting) — {e:#}; q retry, Q discard"
                    ));
                }
            },
            KeyCode::Char('Q') => {
                self.quit = true;
            }
            KeyCode::Char('s') => match self.save_all() {
                Ok(n) => self.message = Some(format!("saved {n} file(s)")),
                Err(e) => self.message = Some(format!("save failed — {e:#}")),
            },
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char('j') | KeyCode::Down | KeyCode::Char('n') => self.cursor_next(),
            KeyCode::Char('k') | KeyCode::Up | KeyCode::Char('N') => self.cursor_prev(),
            KeyCode::Char('i') => self.ignore(),
            KeyCode::Char('a') => self.add_focused(false, false),
            KeyCode::Char('A') => self.add_focused(false, true),
            KeyCode::Char('h') => self.add_focused(true, false),
            KeyCode::Char('H') => self.add_focused(true, true),
            KeyCode::Char('p') => {
                if let Some(w) = self.cur_word() {
                    self.mode = Mode::Add {
                        buf: w.to_string(),
                        sensitive: false,
                    };
                }
            }
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
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Mode::Replace(b) = &mut self.mode {
                    b.push(c);
                }
            }
            _ => {}
        }
    }

    fn handle_add_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                let (buf, sensitive) = match &self.mode {
                    Mode::Add { buf, sensitive } => (buf.clone(), *sensitive),
                    _ => unreachable!(),
                };
                self.mode = Mode::Normal;
                if !buf.trim().is_empty() {
                    self.commit_add(&buf, sensitive);
                }
            }
            KeyCode::Backspace => {
                if let Mode::Add { buf, .. } = &mut self.mode {
                    buf.pop();
                }
            }
            KeyCode::Char('=') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Toggle exact-case instead of inserting a literal `=`.
                if let Mode::Add { sensitive, .. } = &mut self.mode {
                    *sensitive = !*sensitive;
                }
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Mode::Add { buf, .. } = &mut self.mode {
                    buf.push(c);
                }
            }
            _ => {}
        }
    }

    /// Register the prompt text on the appropriate layer and refresh entries.
    fn commit_add(&mut self, text: &str, sensitive: bool) {
        let norm: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let stem = crate::dict::canonical(&norm);
        let layer = if sensitive { "exact case" } else { "any case" };
        self.engine.add_phrase(text, sensitive);
        self.persist_working();
        self.retain_unresolved();
        self.message = Some(format!(
            "added \u{201c}{stem}\u{201d} ({layer}) to {}",
            self.engine.working_path().display()
        ));
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

    fn cur_suggestion(&mut self, n: usize) -> Option<String> {
        self.ensure_suggestions_for_current();
        let e = self.entries.get(self.cursor)?;
        e.suggestions.as_ref()?.get(n - 1).cloned()
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
        self.token_cache.remove(&path);

        self.entries.remove(idx);
        for e in self.entries.iter_mut() {
            if e.path != path {
                continue;
            }
            if delta != 0 && e.current_offset >= end {
                e.current_offset = (e.current_offset as isize + delta).max(0) as usize;
            }
            let Some(compound) = &mut e.compound else {
                continue;
            };
            let compound_end = compound.byte_offset + compound.text.len();
            if compound.byte_offset >= end {
                if delta != 0 {
                    compound.byte_offset = (compound.byte_offset as isize + delta).max(0) as usize;
                }
            } else if compound.byte_offset <= start && end <= compound_end {
                // The edit landed inside this compound — this entry is a
                // sibling part of the token just corrected. Splice the same
                // change into its text, or `h` here would register a compound
                // that still contains the typo. Working from positions rather
                // than searching for the old text keeps this exact even when a
                // compound repeats a part ("teh-teh").
                let at = start - compound.byte_offset;
                compound
                    .text
                    .replace_range(at..at + entry.word_len, new_word);
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
        let stem = crate::dict::canonical(&word);
        self.engine.ignore_session(&word);
        self.retain_unresolved();
        self.message = Some(format!("ignored \u{201c}{stem}\u{201d} for this session"));
    }

    /// Register the focused token in the working dictionary and drop every
    /// entry the addition resolves. `whole_compound` adds the entire
    /// hyphenated token rather than the flagged part (so `Tzeya-Gan` clears
    /// both `Tzeya` and `Gan`); `sensitive` registers it for exact casing only.
    /// This is the `a`/`A`/`h`/`H` keys, which differ only in those two flags.
    fn add_focused(&mut self, whole_compound: bool, sensitive: bool) {
        let Some(entry) = self.entries.get(self.cursor) else {
            return;
        };
        let token = match (whole_compound, &entry.compound) {
            (true, Some(compound)) => compound.text.clone(),
            _ => entry.word.clone(),
        };
        let stem = crate::dict::canonical(&token);
        if sensitive {
            self.engine.add_cs(&token);
        } else {
            self.engine.add_ci(&token);
        }
        self.persist_working();
        self.retain_unresolved();
        let layer = if sensitive { "exact case" } else { "any case" };
        self.message = Some(format!(
            "added \u{201c}{stem}\u{201d} ({layer}) to {}",
            self.engine.working_path().display()
        ));
    }

    fn persist_working(&mut self) {
        if let Err(e) = self.engine.save_working() {
            self.message = Some(format!("could not save working dict: {e}"));
        }
    }

    /// Drop entries now accepted by any dictionary layer. An entry is removed
    /// if either its part word is accepted OR the compound it belongs to (as a
    /// whole) is accepted — so adding `Tzeya-Gan` clears both the `Tzeya` and
    /// `Gan` parts — or if it has become phrase-covered in context (adding
    /// `per se` clears a flagged `se` that stands inside the phrase).
    fn retain_unresolved(&mut self) {
        let mut token_cache = std::mem::take(&mut self.token_cache);
        let mut keep: Vec<bool> = Vec::with_capacity(self.entries.len());
        for e in &self.entries {
            let part_ok = self.engine.check(&e.word);
            let compound_ok = e
                .compound
                .as_ref()
                .is_some_and(|c| self.engine.check(&c.text));
            keep.push(!(part_ok || compound_ok) && !self.entry_phrase_covered(e, &mut token_cache));
        }
        self.token_cache = token_cache;
        let mut keep = keep.into_iter();
        self.entries.retain(|_| keep.next().unwrap_or(true));
        if self.cursor >= self.entries.len() {
            self.cursor = self.entries.len().saturating_sub(1);
        }
    }

    /// True if the entry's token forms a known phrase bigram with a neighbour
    /// in the file — the same context check `check::phrase_covered` ran at
    /// scan time, redone now that the phrase layers may have changed.
    fn entry_phrase_covered(
        &self,
        entry: &Entry,
        cache: &mut HashMap<std::path::PathBuf, Option<Tokenized>>,
    ) -> bool {
        if !cache.contains_key(&entry.path) {
            let toks = self
                .files
                .get(&entry.path)
                .map(|buf| file_tokens(&buf.text, &entry.path, self.format));
            cache.insert(entry.path.clone(), toks);
        }
        let Some(t) = cache.get(&entry.path).and_then(|t| t.as_ref()) else {
            return false;
        };
        // The entry offset may point inside a compound token (it is a part
        // start), and offsets shift after replacements — find the token whose
        // span contains the current word span.
        let word_end = entry.current_offset + entry.word_len;
        let Some(i) = t.tokens.iter().position(|tok| {
            tok.byte_range.start <= entry.current_offset && word_end <= tok.byte_range.end
        }) else {
            return false;
        };
        check::phrase_covered(
            i,
            &t.tokens,
            &t.lowercase,
            &t.gap_clean,
            self.engine.phrase_bigrams(),
            self.engine.phrase_bigrams_cs(),
        )
    }

    /// Write every dirty file and the working dictionary. Files that fail to
    /// save stay dirty so a later save retries them; the first call to fail
    /// returns the collected errors.
    fn save_all(&mut self) -> Result<usize> {
        let dirty: Vec<_> = self.dirty.iter().cloned().collect();
        let mut saved = 0usize;
        let mut failures: Vec<String> = Vec::new();
        for path in &dirty {
            let Some(buf) = self.files.get(path) else {
                continue;
            };
            match crate::fsutil::write_atomic(path, buf.text.as_bytes()) {
                Ok(()) => {
                    self.dirty.remove(path);
                    saved += 1;
                }
                Err(e) => failures.push(format!("{}: {e}", path.display())),
            }
        }
        if let Err(e) = self.engine.save_working() {
            failures.push(format!("working dict: {e}"));
        }
        if failures.is_empty() {
            Ok(saved)
        } else {
            anyhow::bail!("{}", failures.join("; "))
        }
    }

    fn ensure_suggestions_for_current(&mut self) {
        if let Some(entry) = self.entries.get_mut(self.cursor)
            && entry.suggestions.is_none()
        {
            let sugs = self
                .suggest_cache
                .entry(entry.word.clone())
                .or_insert_with(|| {
                    // One focused word at a time: the extra ~1ms of ngram
                    // search is invisible here and buys the best answer for a
                    // badly mangled word.
                    let mut sugs = self.engine.suggest(&entry.word, Suggest::Thorough);
                    sugs.truncate(SUGGEST_LIMIT);
                    sugs
                })
                .clone();
            entry.suggestions = Some(sugs);
        }
    }

    fn draw(&mut self, f: &mut Frame<'_>) {
        self.ensure_suggestions_for_current();

        let area = f.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // title
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
                Line::from(format!("{}:{}  {}", e.path.display(), line, e.word))
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

        // Context — a window of characters centered on the misspelling, so the
        // word is always visible even when its paragraph is hundreds of words.
        let width = chunks[0].width as usize;
        let (context, file_label) = self.context_lines(width);
        let title = match file_label {
            Some(f) => format!("context \u{2014} {f}"),
            None => "context".to_string(),
        };
        let ctx_block = Block::default().borders(Borders::ALL).title(title);
        let para = Paragraph::new(context)
            .wrap(Wrap { trim: false })
            .block(ctx_block);
        f.render_widget(para, chunks[0]);

        // Suggestions / input
        let block = Block::default().borders(Borders::ALL).title("suggestions");
        let para = Paragraph::new(self.suggestion_lines()).block(block);
        f.render_widget(para, chunks[1]);
    }

    /// A character window centered on the focused misspelling. Manuscript
    /// drafts typically put one whole paragraph on a single line, so a
    /// line-based window would push the word off-screen; instead we take a
    /// fixed budget of characters on each side, collapse all whitespace runs
    /// (including newlines) into single spaces, and mark truncation with `…`.
    fn context_lines(&self, width: usize) -> (Vec<Line<'static>>, Option<String>) {
        let Some(entry) = self.entries.get(self.cursor) else {
            return (vec![Line::from("(no misspellings)")], None);
        };
        let Some(buf) = self.files.get(&entry.path) else {
            return (vec![Line::from("(file unavailable)")], None);
        };
        let text = buf.text.as_str();
        let word_start = entry.current_offset;
        let word_end = entry.current_offset + entry.word_len;

        let cols = width.max(20).saturating_sub(2); // leave room for borders
        // ~1.5 wrapped rows on each side of the word keeps it near the vertical
        // center of the pane yet visible without scrolling.
        let half = cols + cols / 2;

        let mut before_start = word_start.saturating_sub(half);
        while before_start > 0 && !text.is_char_boundary(before_start) {
            before_start -= 1;
        }
        let mut after_end = (word_end + half).min(text.len());
        while after_end < text.len() && !text.is_char_boundary(after_end) {
            after_end += 1;
        }

        let (line0, _) = buf.locate(word_start);
        let before = collapse_ws(&text[before_start..word_start]);
        let word = &text[word_start..word_end];
        let after = collapse_ws(&text[word_end..after_end]);

        let lead = if before_start > 0 { "\u{2026} " } else { "" };
        let trail = if after_end < text.len() {
            " \u{2026}"
        } else {
            ""
        };

        let spans = vec![
            Span::styled(
                format!("L{} ", line0 + 1),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(format!("{lead}{before}")),
            Span::styled(
                word.to_string(),
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("{after}{trail}")),
        ];
        let label = entry
            .path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string());
        (vec![Line::from(spans)], label)
    }

    fn suggestion_lines(&self) -> Vec<Line<'static>> {
        if let Mode::Replace(buf) = &self.mode {
            return vec![Line::from(format!("replace with: {}_", buf))];
        }
        if let Mode::Add { buf, sensitive } = &self.mode {
            // The `=` prefix mirrors the working-dict file format and shows
            // the exact-case state without cursor movement.
            let eq = if *sensitive { "=" } else { "" };
            return vec![
                Line::from(format!("add: {eq}{buf}_")),
                Line::from("Enter add \u{00b7} Esc cancel \u{00b7} = exact case"),
            ];
        }
        let Some(entry) = self.entries.get(self.cursor) else {
            return vec![Line::from("(nothing selected)")];
        };

        let mut lines = Vec::new();
        let suggestions = entry.suggestions.as_deref().unwrap_or(&[]);
        if suggestions.is_empty() {
            lines.push(Line::from("(no suggestions)"));
            lines.push(Line::from("r \u{2014} type a replacement"));
        } else {
            for (i, s) in suggestions.iter().take(SUGGEST_LIMIT).enumerate() {
                lines.push(Line::from(format!(" {}) {}", i + 1, s)));
            }
        }
        if let Some(comp) = &entry.compound {
            lines.push(Line::from(""));
            lines.push(Line::from(format!(
                "part of {} \u{2014} h add whole, H exact-case",
                comp.text
            )));
        }
        lines
    }

    fn draw_footer(&self, f: &mut Frame<'_>, area: Rect) {
        let hint = "j/k move \u{00b7} 1-9 replace \u{00b7} r replace \u{00b7} i ignore \u{00b7} a/A add word \u{00b7} h/H add compound \u{00b7} p add word/phrase \u{00b7} s save \u{00b7} q quit \u{00b7} ? help";
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
        let key_w = HELP_ROWS
            .iter()
            .map(|(k, _)| k.chars().count())
            .max()
            .unwrap_or(0);
        let desc_w = HELP_ROWS
            .iter()
            .map(|(_, d)| d.chars().count())
            .max()
            .unwrap_or(0);
        let gap = " ".repeat(HELP_GAP);

        let key_style = Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD);
        let head_style = Style::default().fg(Color::Cyan);

        let mut lines: Vec<Line> = Vec::with_capacity(HELP_ROWS.len() + 2);
        for (keys, desc) in HELP_ROWS {
            if keys.is_empty() {
                // Section heading, preceded by a blank line except at the top.
                if !lines.is_empty() {
                    lines.push(Line::from(""));
                }
                lines.push(Line::from(Span::styled(format!(" {desc}"), head_style)));
                continue;
            }
            lines.push(Line::from(vec![
                Span::styled(format!("  {keys:<key_w$}"), key_style),
                Span::raw(format!("{gap}{desc}")),
            ]));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            " press any key to close",
            Style::default().add_modifier(Modifier::DIM),
        )));

        // Size to the content: two columns plus indent, gap, padding, borders.
        let width = (key_w + HELP_GAP + desc_w + 5) as u16;
        let height = lines.len() as u16 + 2;
        let area = centered(width, height, f.area());

        // Clear first, or the panes underneath show through the overlay.
        f.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" keys ");
        let para = Paragraph::new(lines).block(block);
        f.render_widget(para, area);
    }
}

/// Rows of the help overlay: `(keys, description)`. An empty `keys` marks a
/// section heading.
const HELP_ROWS: &[(&str, &str)] = &[
    ("", "move"),
    ("j / n / \u{2193}", "next misspelling"),
    ("k / N / \u{2191}", "previous misspelling"),
    ("", "fix"),
    ("1-9", "replace with that suggestion"),
    ("r", "type a replacement (Enter accept, Esc cancel)"),
    ("i", "ignore this word for the session"),
    ("", "dictionary"),
    ("a / A", "add word \u{2014} lowercase / exact case"),
    (
        "h / H",
        "add whole compound \u{2014} lowercase / exact case",
    ),
    ("p", "add word or phrase (prompt; = toggles exact case)"),
    ("", "session"),
    ("s", "save all edited files now"),
    ("q", "save and quit"),
    ("Q / Ctrl-C", "discard edits and quit"),
    ("?", "this help"),
];

/// Space between the key column and its description.
const HELP_GAP: usize = 3;

/// A `width` x `height` rectangle centered in `area`, clamped to fit.
fn centered(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::WorkingDict;
    use crate::testutil;

    fn test_engine(path: std::path::PathBuf) -> Engine {
        let sys = crate::sysdict::resolve_embedded();
        let dict = crate::engine::load_dictionary(&sys.aff, &sys.dic).unwrap();
        Engine::new(dict, WorkingDict::default(), path)
    }

    fn app_with_one_entry(path: &std::path::Path, engine: Engine) -> App {
        let miss = vec![Misspelling {
            path: path.to_path_buf(),
            line: 1,
            col: 1,
            byte_offset: 0,
            word: "cdoe".to_string(),
            suggestions: None,
            compound: None,
        }];
        App::new(miss, engine, Format::Auto).unwrap()
    }

    #[test]
    fn save_failure_reports_and_keeps_file_dirty() {
        let s = testutil::scratch("tui-save");
        let good = s.path("good.md");
        std::fs::write(&good, "cdoe").unwrap();
        // `blocker` is a regular file, so nothing under it can be written.
        let blocked = s.path("blocker").join("f.md");
        std::fs::write(s.path("blocker"), b"").unwrap();

        let work = s.path("work.dic");
        let engine = test_engine(work.clone());
        let mut app = App {
            entries: Vec::new(),
            cursor: 0,
            files: HashMap::from([
                (good.clone(), FileBuf::new("cdoe".to_string())),
                (blocked.clone(), FileBuf::new("cdoe".to_string())),
            ]),
            dirty: HashSet::from([good.clone(), blocked.clone()]),
            engine,
            format: Format::Auto,
            suggest_cache: HashMap::new(),
            token_cache: HashMap::new(),
            mode: Mode::Normal,
            show_help: false,
            message: None,
            quit: false,
        };
        let result = app.save_all();
        assert!(result.is_err(), "save should report the failure");
        // The good file saved and left the dirty set; the blocked one stays.
        assert!(!app.dirty.contains(&good));
        assert!(app.dirty.contains(&blocked), "failed file must stay dirty");
        assert_eq!(std::fs::read(&good).unwrap(), b"cdoe");

        let loaded = crate::dict::load(&work).unwrap();
        assert!(loaded.is_empty());
    }

    /// Fixing one part of a hyphenated compound has to update what its
    /// siblings believe the compound says — otherwise `h` on a sibling
    /// registers a coinage still containing the typo just corrected. The
    /// repeated-part case ("teh-teh") is what rules out patching by search.
    #[test]
    fn sibling_compounds_track_the_edit() {
        let s = testutil::scratch("tui-compound");
        let path = s.path("c.md");
        std::fs::write(&path, "teh-teh rode forth\n").unwrap();

        let engine = test_engine(s.path("work.dic"));
        let miss = crate::check::check_file(&path, Format::Auto, &engine).unwrap();
        assert_eq!(miss.len(), 2, "expected both parts flagged: {miss:?}");
        assert!(miss.iter().all(|m| m.compound.is_some()));

        let mut app = App::new(miss, engine, Format::Auto).unwrap();
        // Fix the *second* part: patching by string search would rewrite the
        // first one and produce "the-teh" instead of "teh-the".
        app.cursor = 1;
        app.apply_replacement("the");

        assert_eq!(app.entries.len(), 1, "the fixed part should be gone");
        let survivor = &app.entries[0];
        assert_eq!(survivor.word, "teh");
        assert_eq!(
            survivor.compound.as_ref().map(|c| c.text.as_str()),
            Some("teh-the"),
            "sibling compound went stale"
        );
        assert_eq!(app.files[&path].text, "teh-the rode forth\n");
    }

    #[test]
    fn save_success_clears_dirty() {
        let s = testutil::scratch("tui-save-ok");
        let good = s.path("good.md");
        std::fs::write(&good, "cdoe").unwrap();

        let engine = test_engine(s.path("work.dic"));
        let mut app = app_with_one_entry(&good, engine);
        app.dirty.insert(good.clone());
        assert_eq!(app.save_all().unwrap(), 1);
        assert!(app.dirty.is_empty());
    }
}
