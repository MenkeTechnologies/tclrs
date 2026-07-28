//! The interactive REPL: the same evaluation as [`crate::repl`], driven by a
//! `reedline` line editor instead of `read_line`.
//!
//! ```text
//! ─( 14:52:07 )──< command 3 >──────────────────────{ tclrs 0.1.0 }─
//! tclrs❯ proc double {x} {
//! ····❯   expr {$x * 2}
//! ····❯ }
//! ```
//!
//! What the editor adds over the plain loop:
//!
//! * **Multi-line editing.** A command left open is answered by the parser, not
//!   by counting braces: [`crate::repl::incomplete`] is the whole of the
//!   `Validator`, so the editor and the evaluator agree on what "finished"
//!   means by construction.
//! * **Completion.** Tab offers what the compiler would accept in that
//!   position — command names at the head of a command, an ensemble's
//!   subcommands after `string` / `array` / `dict` / `info`, and the
//!   interpreter's own variables after `$`. The vocabulary comes from
//!   [`tclrs::names`], which is assembled from the compiler's tables.
//! * **History**, at `~/.tclrs/history`, and emacs or vi keys — see
//!   [`resolve_mode`].
//! * **Procedures that outlive the line that defined them.** See [`Session`].
//!
//! Only a terminal gets any of this. A script arriving on a pipe is still read
//! by [`crate::repl::run`], which prints nothing of its own.

use std::borrow::Cow;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use nu_ansi_term::{Color, Style};
use reedline::{
    default_emacs_keybindings, default_vi_insert_keybindings, default_vi_normal_keybindings,
    ColumnarMenu, Completer, EditMode, Emacs, ExampleHighlighter, FileBackedHistory, KeyCode,
    KeyModifiers, Keybindings, MenuBuilder, Prompt, PromptEditMode, PromptHistorySearch,
    PromptHistorySearchStatus, Reedline, ReedlineEvent, ReedlineMenu, Signal, Span, Suggestion,
    ValidationResult, Validator, Vi,
};

use tclrs::cursor::{self, Context};
use tclrs::{names, Interp, Script};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How many commands the history file keeps.
const HISTORY_LIMIT: usize = 5_000;

// ── session ──────────────────────────────────────────────────────────────

/// The procedures defined so far, in definition order.
///
/// A procedure is compiled into the chunk of the script that defines it, so it
/// lives exactly as long as that evaluation — which in a REPL is one line. The
/// reference interpreter keeps it for the session, so this does too, by keeping
/// the *text* of each definition and prefixing the accumulated set to every
/// later evaluation. `proc` has no effect beyond defining, so replaying one is
/// not observable; what is observable is that `double` still answers on the
/// line after it was written.
///
/// A definition that is written again replaces the one before it, rather than
/// joining it — two definitions of one name in a single script are refused by
/// the compiler, and a REPL is a place where redefining is ordinary.
///
/// Coroutines are not carried this way: `coroutine` starts a body running, and
/// replaying that would run it again.
#[derive(Default)]
struct Session {
    procs: Vec<(String, String)>,
}

impl Session {
    /// The definitions to prefix to the next evaluation, oldest first. Empty
    /// until something has been defined, so an ordinary line is evaluated
    /// exactly as typed.
    fn prelude(&self) -> String {
        let mut out = String::new();
        for (_, definition) in &self.procs {
            out.push_str(definition);
            out.push('\n');
        }
        out
    }

    /// Forget every procedure `script` defines, so that a redefinition does not
    /// meet its predecessor in the prelude. Called before the evaluation, since
    /// the new text is the one that must survive.
    fn forget_redefined(&mut self, script: &Script) {
        for (name, _) in definitions(script) {
            self.procs.retain(|(defined, _)| *defined != name);
        }
    }

    /// Record what `script` defined. Called only after it evaluated, because a
    /// script that failed to compile defined nothing.
    fn absorb(&mut self, script: &Script) {
        for (name, definition) in definitions(script) {
            self.procs.push((name, definition));
        }
    }

    /// The names on offer for completion.
    fn proc_names(&self) -> Vec<String> {
        self.procs.iter().map(|(name, _)| name.clone()).collect()
    }
}

/// The `proc name spec body` commands at the top level of `script`, each with
/// the text that defines it again.
///
/// The text is rebuilt from the parsed words rather than sliced out of the
/// source, because the source of a definition is whatever the user typed —
/// possibly quoted several ways — while a list of the four words is quoted
/// exactly once and parses back to the same four. A nested `proc` is skipped:
/// the compiler refuses it anyway, since it defines only when its enclosing
/// code runs.
fn definitions(script: &Script) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for command in &script.commands {
        let [head, name, spec, body] = command.words.as_slice() else {
            continue;
        };
        if head.as_literal() != Some("proc") {
            continue;
        }
        let (Some(name), Some(spec), Some(body)) =
            (name.as_literal(), spec.as_literal(), body.as_literal())
        else {
            continue;
        };
        found.push((
            name.to_string(),
            tclrs::list::join(&["proc", name, spec, body]),
        ));
    }
    found
}

// ── completion ───────────────────────────────────────────────────────────

/// Offers what the compiler would accept where the cursor is.
struct TclCompleter {
    commands: Vec<String>,
    /// Shared with the loop, which refreshes both after every evaluation.
    procs: Arc<Mutex<Vec<String>>>,
    variables: Arc<Mutex<Vec<String>>>,
}

impl TclCompleter {
    fn suggestions(
        &self,
        words: impl Iterator<Item = String>,
        word: &str,
        span: Span,
    ) -> Vec<Suggestion> {
        let mut out: Vec<Suggestion> = words
            .filter(|candidate| candidate.starts_with(word))
            .map(|value| Suggestion {
                value,
                description: None,
                style: None,
                extra: None,
                span,
                append_whitespace: true,
                display_override: None,
                match_indices: None,
            })
            .collect();
        out.sort_by(|a, b| a.value.cmp(&b.value));
        out.dedup_by(|a, b| a.value == b.value);
        out
    }
}

impl Completer for TclCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let (start, word) = cursor::word_at(line, pos);
        let span = Span::new(start, pos);
        match cursor::context_in_tcl(line, start, word) {
            Context::Command => {
                let procs = self.procs.lock().map(|g| g.clone()).unwrap_or_default();
                let all = self.commands.iter().cloned().chain(procs);
                self.suggestions(all, word, span)
            }
            Context::Subcommand(head) => {
                let subs = names::subcommands(head).iter().map(|s| s.to_string());
                self.suggestions(subs, word, span)
            }
            Context::Variable => {
                let names = self.variables.lock().map(|g| g.clone()).unwrap_or_default();
                // The `$` is part of the word being replaced, so it is part of
                // every suggestion too.
                let sigiled = names.into_iter().map(|name| format!("${name}"));
                self.suggestions(sigiled, word, span)
            }
            Context::Argument => Vec::new(),
        }
    }
}

// ── continuation ─────────────────────────────────────────────────────────

/// Asks the parser whether what has been typed is a script yet. A brace, quote
/// or bracket left open keeps the editor on the same buffer; anything else —
/// including text that is malformed rather than unfinished — is handed over to
/// be evaluated and reported.
struct TclValidator;

impl Validator for TclValidator {
    fn validate(&self, line: &str) -> ValidationResult {
        if crate::repl::incomplete(line) {
            ValidationResult::Incomplete
        } else {
            ValidationResult::Complete
        }
    }
}

// ── prompt ───────────────────────────────────────────────────────────────

struct TclPrompt {
    commands: Arc<Mutex<u64>>,
}

/// The local time as `HH:MM:SS`. `localtime_r` reads the zone the rest of the
/// system reads; a clock that cannot be converted falls back to the time of
/// day in UTC rather than dropping the segment.
fn now() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as libc::time_t)
        .unwrap_or(0);
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { !libc::localtime_r(&secs, &mut tm).is_null() } {
        return format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec);
    }
    let day = (secs as u64) % 86_400;
    format!("{:02}:{:02}:{:02}", day / 3600, (day % 3600) / 60, day % 60)
}

/// The terminal's width, or 80 when it will not say.
fn columns() -> usize {
    use std::os::unix::io::AsRawFd;
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    let fd = std::io::stdout().as_raw_fd();
    let cols = if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) } == 0 && size.ws_col > 0 {
        size.ws_col as usize
    } else {
        80
    };
    cols.max(40)
}

/// The rule above the buffer: the time, how many commands have run, and which
/// tclrs is running them.
///
/// The width is counted in characters of the segments and of the frame, and
/// never of the color codes — `paint` is applied after the arithmetic for
/// exactly that reason. A terminal too narrow for all three segments loses the
/// last one instead of wrapping onto a second line.
fn status_bar(count: u64) -> String {
    let dim = Style::new().fg(Color::DarkGray);
    let time = format!(" {} ", now());
    let commands = format!(" command {count} ");
    let version = format!(" tclrs {VERSION} ");

    let frame = "─()──<>{}─".chars().count();
    let used = time.chars().count() + commands.chars().count() + version.chars().count() + frame;
    let left = format!(
        "{}{}{}{}{}{}",
        dim.paint("─("),
        Style::new().fg(Color::Cyan).paint(&time),
        dim.paint(")"),
        dim.paint("──<"),
        Style::new().fg(Color::LightYellow).bold().paint(&commands),
        dim.paint(">"),
    );
    let Some(dashes) = columns().checked_sub(used).filter(|d| *d >= 2) else {
        return left;
    };
    format!(
        "{}{}{}{}{}",
        left,
        dim.paint("─".repeat(dashes)),
        dim.paint("{"),
        Style::new().fg(Color::Magenta).paint(&version),
        dim.paint("}─"),
    )
}

impl Prompt for TclPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        let count = self.commands.lock().map(|g| *g).unwrap_or(0);
        let name = Style::new().fg(Color::Cyan).bold().paint("tclrs");
        Cow::Owned(format!("{}\n{}", status_bar(count), name))
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_indicator(&self, _mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Owned(
            Style::new()
                .fg(Color::LightCyan)
                .bold()
                .paint("❯ ")
                .to_string(),
        )
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Owned(Style::new().fg(Color::DarkGray).paint("····❯ ").to_string())
    }

    fn render_prompt_history_search_indicator(&self, search: PromptHistorySearch) -> Cow<'_, str> {
        let failing = match search.status {
            PromptHistorySearchStatus::Passing => "",
            PromptHistorySearchStatus::Failing => "failing ",
        };
        Cow::Owned(format!("({failing}reverse-search: {}) ", search.term))
    }
}

// ── configuration ────────────────────────────────────────────────────────

/// Which keymap the editor uses.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Mode {
    Emacs,
    Vi,
}

/// `~/.tclrs`, created if it is not there. Falls back to the current directory
/// when there is no home, which is where a history file is least surprising if
/// it has to go somewhere.
fn config_dir() -> PathBuf {
    let dir = std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".tclrs"))
        .unwrap_or_else(|| PathBuf::from(".tclrs"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// The keymap, from `TCLRS_REPL_MODE` first and `~/.tclrs/config.toml` second:
///
/// ```toml
/// [repl]
/// mode = "vi"
/// ```
///
/// Anything else — no file, no key, a value that is neither — is emacs, which
/// is what a line editor is expected to do when nothing has been said.
fn resolve_mode() -> Mode {
    if let Some(set) = std::env::var_os("TCLRS_REPL_MODE") {
        return named_mode(&set.to_string_lossy()).unwrap_or(Mode::Emacs);
    }
    let Ok(text) = std::fs::read_to_string(config_dir().join("config.toml")) else {
        return Mode::Emacs;
    };
    configured_mode(&text).unwrap_or(Mode::Emacs)
}

fn named_mode(value: &str) -> Option<Mode> {
    match value.trim().trim_matches('"').to_ascii_lowercase().as_str() {
        "vi" | "vim" => Some(Mode::Vi),
        "emacs" => Some(Mode::Emacs),
        _ => None,
    }
}

/// `mode` under `[repl]`, read without a TOML parser: the file has one setting,
/// and the alternative is a dependency for one key. A `mode` outside `[repl]`
/// is not this setting and is ignored.
fn configured_mode(text: &str) -> Option<Mode> {
    let mut in_repl = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_repl = line == "[repl]";
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if in_repl && key.trim() == "mode" {
            return named_mode(value);
        }
    }
    None
}

/// Tab opens the completion menu and steps through it; Shift-Tab steps back.
/// Bound on whichever keymap is doing the inserting, so completion behaves the
/// same in both modes.
fn bind_menu(keys: &mut Keybindings) {
    keys.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu("completion_menu".to_string()),
            ReedlineEvent::MenuNext,
        ]),
    );
    keys.add_binding(
        KeyModifiers::SHIFT,
        KeyCode::BackTab,
        ReedlineEvent::MenuPrevious,
    );
    keys.add_binding(
        KeyModifiers::NONE,
        KeyCode::BackTab,
        ReedlineEvent::MenuPrevious,
    );
}

fn edit_mode() -> Box<dyn EditMode> {
    match resolve_mode() {
        Mode::Emacs => {
            let mut keys = default_emacs_keybindings();
            bind_menu(&mut keys);
            Box::new(Emacs::new(keys))
        }
        Mode::Vi => {
            let mut insert = default_vi_insert_keybindings();
            bind_menu(&mut insert);
            Box::new(Vi::new(insert, default_vi_normal_keybindings()))
        }
    }
}

// ── the loop ─────────────────────────────────────────────────────────────

const LOGO: &str = "\
████████╗ ██████╗██╗     ██████╗ ███████╗
╚══██╔══╝██╔════╝██║     ██╔══██╗██╔════╝
   ██║   ██║     ██║     ██████╔╝███████╗
   ██║   ╚██████╗███████╗██║  ██║███████║
   ╚═╝    ╚═════╝╚══════╝╚═╝  ╚═╝╚══════╝";

fn greet() {
    println!("{}", Style::new().fg(Color::Cyan).paint(LOGO));
    println!(
        "{}",
        Style::new().fg(Color::DarkGray).paint(format!(
            "  tclrs {VERSION} — Tcl on fusevm. Tab completes, Ctrl-D or `exit` leaves."
        ))
    );
    println!();
}

/// Whether `line` asks to leave, and with what status. `exit` is not a command
/// this frontend compiles; in a REPL it is the way out, so it is answered here
/// rather than reported as unknown.
fn exit_request(line: &str) -> Option<ExitCode> {
    let mut words = line.split_whitespace();
    match (words.next(), words.next(), words.next()) {
        (Some("exit" | "quit"), None, _) => Some(ExitCode::SUCCESS),
        (Some("exit"), Some(code), None) => code.parse::<u8>().ok().map(ExitCode::from),
        _ => None,
    }
}

/// Read, evaluate and print until the reader asks to stop. Always succeeds:
/// end of input is how a session ends, and a command that failed was reported
/// when it failed.
pub fn run(interp: &mut Interp) -> ExitCode {
    greet();

    let commands = Arc::new(Mutex::new(0u64));
    let procs = Arc::new(Mutex::new(Vec::new()));
    let variables = Arc::new(Mutex::new(interp.global_names()));
    let vocabulary: Vec<String> = names::commands().iter().map(|s| s.to_string()).collect();

    let completer = TclCompleter {
        commands: vocabulary.clone(),
        procs: Arc::clone(&procs),
        variables: Arc::clone(&variables),
    };
    let menu = ColumnarMenu::default()
        .with_name("completion_menu")
        .with_columns(4)
        .with_column_padding(2);

    let mut editor = Reedline::create()
        .with_completer(Box::new(completer))
        .with_menu(ReedlineMenu::EngineCompleter(Box::new(menu)))
        .with_validator(Box::new(TclValidator))
        .with_highlighter(Box::new(ExampleHighlighter::new(vocabulary)))
        .with_edit_mode(edit_mode())
        .with_history(history());

    let prompt = TclPrompt {
        commands: Arc::clone(&commands),
    };
    let mut session = Session::default();

    loop {
        let line = match editor.read_line(&prompt) {
            Ok(Signal::Success(line)) => line,
            // An interrupted line is abandoned, as it is at any other prompt.
            Ok(Signal::CtrlC) => continue,
            // Every other signal ends the session the way end of input does.
            Ok(_) => return ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("tclrs: {e}");
                return ExitCode::FAILURE;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(code) = exit_request(&line) {
            return code;
        }

        if let Ok(mut count) = commands.lock() {
            *count += 1;
        }
        evaluate(interp, &mut session, &line);

        if let Ok(mut names) = procs.lock() {
            *names = session.proc_names();
        }
        if let Ok(mut names) = variables.lock() {
            *names = interp.global_names();
        }
    }
}

/// Evaluate one command, printing what it produced and what it failed with, in
/// the spellings the plain loop uses. The session's procedures are evaluated
/// with it, and are recorded from it only once it has succeeded.
fn evaluate(interp: &mut Interp, session: &mut Session, line: &str) {
    let parsed = tclrs::parse(line).ok();
    if let Some(script) = &parsed {
        session.forget_redefined(script);
    }

    let script = format!("{}{line}", session.prelude());
    match interp.eval(&script) {
        Ok(result) => {
            if !result.is_empty() {
                println!("{result}");
            }
            if let Some(parsed) = &parsed {
                session.absorb(parsed);
            }
        }
        Err(e) => eprintln!("{}", e.msg),
    }
}

/// The history file, or an unsaved history when it cannot be opened — a home
/// directory that cannot be written is a reason to lose history between
/// sessions, not a reason to refuse a prompt.
fn history() -> Box<dyn reedline::History> {
    match FileBackedHistory::with_file(HISTORY_LIMIT, config_dir().join("history")) {
        Ok(history) => Box::new(history),
        Err(e) => {
            eprintln!("tclrs: history unavailable: {e}");
            Box::new(FileBackedHistory::new(HISTORY_LIMIT).expect("in-memory history"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The point of the session: what a definition line records is a definition
    /// that parses back to the same procedure, whatever the body contained.
    #[test]
    fn a_definition_is_recorded_so_it_can_be_replayed() {
        let script = tclrs::parse("proc double {x} {expr {$x * 2}}").expect("parses");
        let found = definitions(&script);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "double");

        let mut interp = Interp::capturing();
        interp
            .eval(&format!("{}\nputs [double 21]", found[0].1))
            .expect("replayed definition answers");
        assert_eq!(interp.take_output(), "42\n");
    }

    #[test]
    fn a_redefinition_replaces_rather_than_joins() {
        let mut session = Session::default();
        let first = tclrs::parse("proc f {} {return 1}").expect("parses");
        session.absorb(&first);
        let second = tclrs::parse("proc f {} {return 2}").expect("parses");
        session.forget_redefined(&second);
        session.absorb(&second);

        assert_eq!(session.proc_names(), vec!["f".to_string()]);
        let mut interp = Interp::capturing();
        interp
            .eval(&format!("{}\nputs [f]", session.prelude()))
            .expect("one definition survives");
        assert_eq!(interp.take_output(), "2\n");
    }

    #[test]
    fn a_command_that_is_not_a_definition_is_not_recorded() {
        let script = tclrs::parse("set x 1").expect("parses");
        assert!(definitions(&script).is_empty());
        assert!(Session::default().prelude().is_empty());
    }

    #[test]
    fn exit_is_answered_by_the_loop_and_nothing_else_is() {
        assert!(exit_request("exit").is_some());
        assert!(exit_request("quit").is_some());
        assert!(exit_request("exit 3").is_some());
        assert!(exit_request("exit please now").is_none());
        assert!(exit_request("puts exit").is_none());
    }

    #[test]
    fn the_configured_mode_is_read_from_the_repl_table_only() {
        assert_eq!(configured_mode("[repl]\nmode = \"vi\"\n"), Some(Mode::Vi));
        assert_eq!(configured_mode("[repl]\n# mode = \"vi\"\n"), None);
        assert_eq!(configured_mode("[other]\nmode = \"vi\"\n"), None);
        assert_eq!(configured_mode(""), None);
        assert_eq!(named_mode("VIM"), Some(Mode::Vi));
        assert_eq!(named_mode("sometimes"), None);
    }
}
