//! `tclrs --lsp` — a language server (stdio) for Tcl.
//!
//! Everything it answers with comes from the crate that runs the code, not
//! from a description of it kept alongside:
//!
//! * **Diagnostics** are the parser's failure and then the compiler's, in the
//!   order a run meets them. A construct this frontend refuses is a diagnostic
//!   for the same reason it is an error — the editor learns what the
//!   interpreter would say, not what full Tcl would allow.
//! * **Completion**, **hover** and **signature help** read [`crate::names`],
//!   whose command list is the compiler's own tables and whose synopses are the
//!   wording of its `wrong # args` messages.
//! * **Document symbols** are the `proc` commands the parser found, so a
//!   definition the parser cannot see is not offered.
//!
//! What is under the cursor is decided by [`crate::cursor`], the module the
//! REPL's completer uses, so an editor and the prompt answer alike.
//!
//! Nothing here writes to the terminal: the server speaks JSON-RPC on stdio and
//! that is all stdout carries.

use std::collections::HashMap;

use lsp_server::{Connection, Message, Request, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
    PublishDiagnostics,
};
use lsp_types::request::{
    Completion, DocumentSymbolRequest, HoverRequest, Request as _, SignatureHelpRequest,
};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, DidOpenTextDocumentParams,
    Documentation, Hover, HoverContents, HoverParams, Location, MarkupContent, MarkupKind, OneOf,
    Position, PublishDiagnosticsParams, Range, ServerCapabilities, SignatureHelp,
    SignatureHelpOptions, SignatureHelpParams, SignatureInformation, SymbolInformation, SymbolKind,
    TextDocumentSyncCapability, TextDocumentSyncKind, Uri, WorkDoneProgressOptions,
};

use crate::cursor::{self, Context};
use crate::names::{self, Entry};

/// Run the server on stdin/stdout until the client says to stop. The return is
/// the process's exit status: false when the connection failed rather than
/// closed.
pub fn run_stdio() -> bool {
    match serve() {
        Ok(()) => true,
        Err(e) => {
            // stderr, never stdout — stdout is the protocol.
            eprintln!("tclrs --lsp: {e}");
            false
        }
    }
}

type Failure = Box<dyn std::error::Error + Sync + Send>;

fn serve() -> Result<(), Failure> {
    let (connection, io_threads) = Connection::stdio();
    let capabilities = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec!["[".to_string(), "$".to_string()]),
            ..Default::default()
        }),
        hover_provider: Some(lsp_types::HoverProviderCapability::Simple(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec![" ".to_string()]),
            retrigger_characters: None,
            work_done_progress_options: WorkDoneProgressOptions::default(),
        }),
        ..Default::default()
    };
    connection.initialize(serde_json::to_value(capabilities)?)?;
    main_loop(&connection)?;
    // Before the join, and not after: `Connection::stdio` writes from a thread
    // that ends when the sending half of its channel closes, which is when this
    // connection is dropped. Joining while it is still alive waits for a thread
    // that is waiting for this scope to end.
    drop(connection);
    io_threads.join()?;
    Ok(())
}

/// `Uri` has interior mutability, so documents are keyed by its string form and
/// the `Uri` itself is kept only where the protocol needs one.
fn uri_key(uri: &Uri) -> String {
    uri.as_str().to_string()
}

fn main_loop(c: &Connection) -> Result<(), Failure> {
    let mut docs: HashMap<String, String> = HashMap::new();
    for msg in &c.receiver {
        match msg {
            Message::Request(request) => {
                if c.handle_shutdown(&request)? {
                    return Ok(());
                }
                c.sender.send(Message::Response(answer(&request, &docs)))?;
            }
            Message::Notification(note) => match note.method.as_str() {
                DidOpenTextDocument::METHOD => {
                    let p: DidOpenTextDocumentParams = serde_json::from_value(note.params)?;
                    docs.insert(uri_key(&p.text_document.uri), p.text_document.text.clone());
                    publish(c, &p.text_document.uri, &p.text_document.text)?;
                }
                DidChangeTextDocument::METHOD => {
                    let p: DidChangeTextDocumentParams = serde_json::from_value(note.params)?;
                    // Full sync: the last change carries the whole document.
                    if let Some(change) = p.content_changes.into_iter().next_back() {
                        docs.insert(uri_key(&p.text_document.uri), change.text.clone());
                        publish(c, &p.text_document.uri, &change.text)?;
                    }
                }
                DidCloseTextDocument::METHOD => {
                    let p: lsp_types::DidCloseTextDocumentParams =
                        serde_json::from_value(note.params)?;
                    docs.remove(&uri_key(&p.text_document.uri));
                }
                _ => {}
            },
            Message::Response(_) => {}
        }
    }
    Ok(())
}

fn answer(request: &Request, docs: &HashMap<String, String>) -> Response {
    let result = match request.method.as_str() {
        Completion::METHOD => from_params::<CompletionParams>(request).and_then(|p| {
            let position = p.text_document_position;
            let text = docs.get(&uri_key(&position.text_document.uri))?;
            serde_json::to_value(completion(text, position.position)).ok()
        }),
        HoverRequest::METHOD => from_params::<HoverParams>(request).and_then(|p| {
            let position = p.text_document_position_params;
            let text = docs.get(&uri_key(&position.text_document.uri))?;
            serde_json::to_value(hover(text, position.position)?).ok()
        }),
        SignatureHelpRequest::METHOD => from_params::<SignatureHelpParams>(request).and_then(|p| {
            let position = p.text_document_position_params;
            let text = docs.get(&uri_key(&position.text_document.uri))?;
            serde_json::to_value(signature_help(text, position.position)?).ok()
        }),
        DocumentSymbolRequest::METHOD => from_params::<lsp_types::DocumentSymbolParams>(request)
            .and_then(|p| {
                let text = docs.get(&uri_key(&p.text_document.uri))?;
                serde_json::to_value(document_symbols(&p.text_document.uri, text)).ok()
            }),
        _ => None,
    };
    Response {
        id: request.id.clone(),
        result: Some(result.unwrap_or(serde_json::Value::Null)),
        error: None,
    }
}

fn from_params<T: serde::de::DeserializeOwned>(request: &Request) -> Option<T> {
    serde_json::from_value(request.params.clone()).ok()
}

fn publish(c: &Connection, uri: &Uri, text: &str) -> Result<(), Failure> {
    let params = PublishDiagnosticsParams {
        uri: uri.clone(),
        diagnostics: diagnostics(text),
        version: None,
    };
    c.sender
        .send(Message::Notification(lsp_server::Notification {
            method: PublishDiagnostics::METHOD.to_string(),
            params: serde_json::to_value(params)?,
        }))?;
    Ok(())
}

// ── positions ────────────────────────────────────────────────────────────

/// Byte offsets to LSP positions. The protocol counts UTF-16 code units in a
/// line; the parser counts bytes. Both are converted here, and nowhere else.
struct LineIndex {
    /// Byte offset where each line starts.
    starts: Vec<usize>,
    text: String,
}

impl LineIndex {
    fn new(text: &str) -> Self {
        let mut starts = vec![0];
        for (offset, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                starts.push(offset + 1);
            }
        }
        LineIndex {
            starts,
            text: text.to_string(),
        }
    }

    fn position(&self, offset: usize) -> Position {
        let offset = offset.min(self.text.len());
        let line = match self.starts.binary_search(&offset) {
            Ok(line) => line,
            Err(next) => next - 1,
        };
        let start = self.starts[line];
        let character = self.text[start..offset].encode_utf16().count();
        Position {
            line: line as u32,
            character: character as u32,
        }
    }

    /// The byte offset a position names, clamped to the document.
    fn offset(&self, position: Position) -> usize {
        let line = position.line as usize;
        let Some(&start) = self.starts.get(line) else {
            return self.text.len();
        };
        let end = self.line_end(line);
        let mut units = 0;
        for (offset, c) in self.text[start..end].char_indices() {
            if units >= position.character as usize {
                return start + offset;
            }
            units += c.len_utf16();
        }
        end
    }

    fn line_end(&self, line: usize) -> usize {
        self.starts
            .get(line + 1)
            .map(|next| next - 1)
            .unwrap_or(self.text.len())
    }

    /// The whole of a 1-based line, which is the only span a compile failure
    /// can be pinned to: the compiler reports the command's line, not its
    /// column.
    fn whole_line(&self, line: usize) -> Range {
        let line = line.saturating_sub(1).min(self.starts.len() - 1);
        Range {
            start: self.position(self.starts[line]),
            end: self.position(self.line_end(line)),
        }
    }

    /// The text of the line a position is on, and the offset of the position
    /// within it — what [`crate::cursor`] wants.
    fn line_at(&self, position: Position) -> (&str, usize) {
        let line = (position.line as usize).min(self.starts.len() - 1);
        let start = self.starts[line];
        let end = self.line_end(line);
        (&self.text[start..end], self.offset(position) - start)
    }
}

// ── diagnostics ──────────────────────────────────────────────────────────

/// What running the document would report, and where.
///
/// One at a time, because that is how compilation fails: the parser refuses the
/// whole script, and the compiler stops at the first command it cannot lower.
/// Reporting a second failure would mean inventing a recovery the interpreter
/// does not perform.
pub fn diagnostics(text: &str) -> Vec<Diagnostic> {
    let index = LineIndex::new(text);
    let script = match crate::parse(text) {
        Ok(script) => script,
        Err(e) => {
            let start = index.position(e.offset);
            let range = Range {
                start,
                end: index.position(e.offset.saturating_add(1)),
            };
            return vec![error(range, e.msg)];
        }
    };
    match crate::compiler::compile(&script) {
        Ok(_) => Vec::new(),
        Err(e) => vec![error(index.whole_line(e.line), e.msg)],
    }
}

fn error(range: Range, message: String) -> Diagnostic {
    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("tclrs".to_string()),
        message,
        ..Default::default()
    }
}

// ── completion, hover, signature help ────────────────────────────────────

/// What the compiler would accept where the cursor is: command names at the
/// head of a command, an ensemble's subcommands after its name, and the
/// document's own procedures alongside the commands.
pub fn completion(text: &str, position: Position) -> CompletionResponse {
    let index = LineIndex::new(text);
    let (line, offset) = index.line_at(position);
    let (start, word) = cursor::word_at(line, offset);
    let items = match cursor::context_in_tcl(line, start, word) {
        Context::Command => {
            let mut items: Vec<CompletionItem> = names::CORPUS.iter().map(item).collect();
            items.extend(procs(text).into_iter().map(|(name, line)| CompletionItem {
                label: name,
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some(format!("procedure, defined on line {line}")),
                ..Default::default()
            }));
            items
        }
        Context::Subcommand(head) => names::subcommand_corpus(head).iter().map(item).collect(),
        // A variable's name is not something this frontend can enumerate
        // without running the script, and an argument could be anything.
        Context::Variable | Context::Argument => Vec::new(),
    };
    CompletionResponse::Array(items)
}

fn item(entry: &Entry) -> CompletionItem {
    CompletionItem {
        label: entry.name.to_string(),
        kind: Some(CompletionItemKind::FUNCTION),
        detail: Some(entry.synopsis.to_string()),
        documentation: Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: entry.summary.to_string(),
        })),
        ..Default::default()
    }
}

/// The documentation for whatever the cursor is on, or nothing when it is on a
/// word this frontend has nothing to say about.
pub fn hover(text: &str, position: Position) -> Option<Hover> {
    let index = LineIndex::new(text);
    let (line, offset) = index.line_at(position);
    let (start, word) = word_under(line, offset);
    let entry = match cursor::context_in_tcl(line, start, word) {
        Context::Command => lookup(names::CORPUS, word),
        Context::Subcommand(head) => lookup(names::subcommand_corpus(head), word),
        Context::Variable | Context::Argument => None,
    }?;
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: format!("```tcl\n{}\n```\n\n{}", entry.synopsis, entry.summary),
        }),
        range: None,
    })
}

/// The synopsis of the command the cursor is inside, whichever of its arguments
/// is being typed.
pub fn signature_help(text: &str, position: Position) -> Option<SignatureHelp> {
    let index = LineIndex::new(text);
    let (line, offset) = index.line_at(position);
    let (start, _) = cursor::word_at(line, offset);
    let before = line.get(..start).unwrap_or("");
    let command = match before.rfind(['\n', ';', '[']) {
        Some(at) => &before[at + 1..],
        None => before,
    };
    let mut words = command.split_whitespace();
    let head = words.next()?;
    // An ensemble's real synopsis is its subcommand's.
    let entry = match words.next() {
        Some(sub) if !names::subcommands(head).is_empty() => {
            lookup(names::subcommand_corpus(head), sub)
        }
        _ => lookup(names::CORPUS, head),
    }?;
    Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label: entry.synopsis.to_string(),
            documentation: Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: entry.summary.to_string(),
            })),
            parameters: None,
            active_parameter: None,
        }],
        active_signature: Some(0),
        active_parameter: None,
    })
}

fn lookup(corpus: &'static [Entry], name: &str) -> Option<&'static Entry> {
    corpus.iter().find(|entry| entry.name == name)
}

/// The word the cursor is *on*, rather than the word being typed up to it: a
/// cursor resting anywhere in `puts` should describe `puts`.
fn word_under(line: &str, offset: usize) -> (usize, &str) {
    let end = line[offset..]
        .find(|c: char| c.is_whitespace() || matches!(c, ';' | '[' | ']' | '{' | '}' | '"'))
        .map(|at| offset + at)
        .unwrap_or(line.len());
    let (start, _) = cursor::word_at(line, end);
    (start, &line[start..end])
}

// ── symbols ──────────────────────────────────────────────────────────────

/// The procedures the document defines, as the parser sees them.
pub fn document_symbols(uri: &Uri, text: &str) -> Vec<SymbolInformation> {
    let index = LineIndex::new(text);
    procs(text)
        .into_iter()
        .map(|(name, line)| {
            #[allow(deprecated)]
            SymbolInformation {
                name,
                kind: SymbolKind::FUNCTION,
                tags: None,
                deprecated: None,
                location: Location {
                    uri: uri.clone(),
                    range: index.whole_line(line),
                },
                container_name: None,
            }
        })
        .collect()
}

/// Every `proc name args body` the document defines, with the line it is on.
/// A document that does not parse defines nothing yet — the same answer the
/// compiler gives.
fn procs(text: &str) -> Vec<(String, usize)> {
    let Ok(script) = crate::parse(text) else {
        return Vec::new();
    };
    script
        .commands
        .iter()
        .filter(|command| command.words.len() == 4)
        .filter(|command| command.words[0].as_literal() == Some("proc"))
        .filter_map(|command| {
            let name = command.words[1].as_literal()?;
            Some((name.to_string(), command.line))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    #[test]
    fn a_script_that_runs_has_nothing_to_report() {
        assert!(diagnostics("set x 1\nputs $x\n").is_empty());
    }

    #[test]
    fn a_parse_failure_is_reported_where_the_parser_stopped() {
        let found = diagnostics("puts hello\nputs {unclosed\n");
        assert_eq!(found.len(), 1);
        assert!(found[0].message.contains("missing close-brace"));
        assert_eq!(found[0].range.start.line, 1, "{:?}", found[0].range);
    }

    /// A construct the frontend refuses is a diagnostic, because it is what
    /// running the file would report — the editor learns this interpreter's
    /// answer, not full Tcl's.
    ///
    /// The construct here only has to be *something still refused*: it was
    /// `string wordend` until that was implemented. Whoever lands `{*}` should
    /// swap in whatever is refused then rather than delete the test.
    #[test]
    fn a_refused_construct_is_reported_on_its_line() {
        let found = diagnostics("set x 1\nputs {*}{a b}\n");
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].message.contains("not supported"), "{found:?}");
        assert_eq!(found[0].range.start.line, 1);
    }

    #[test]
    fn completion_offers_commands_at_the_head_and_subcommands_after_an_ensemble() {
        let CompletionResponse::Array(items) = completion("ll", at(0, 2)) else {
            panic!("expected an array");
        };
        assert!(items.iter().any(|i| i.label == "llength"));
        assert!(items.iter().any(|i| i.label == "proc"));

        let CompletionResponse::Array(items) = completion("string tou", at(0, 10)) else {
            panic!("expected an array");
        };
        assert!(items.iter().any(|i| i.label == "toupper"));
        assert!(!items.iter().any(|i| i.label == "llength"));
    }

    #[test]
    fn completion_offers_the_documents_own_procedures() {
        let text = "proc double {x} {expr {$x * 2}}\ndo";
        let CompletionResponse::Array(items) = completion(text, at(1, 2)) else {
            panic!("expected an array");
        };
        let double = items
            .iter()
            .find(|i| i.label == "double")
            .expect("the proc");
        assert!(double.detail.as_deref().unwrap().contains("line 1"));
    }

    #[test]
    fn hover_describes_the_word_the_cursor_rests_on() {
        // Anywhere inside the word, not only at its end.
        let on_command = hover("puts hello", at(0, 2)).expect("a command");
        let HoverContents::Markup(markup) = on_command.contents else {
            panic!("expected markup");
        };
        assert!(
            markup.value.contains("puts ?-nonewline? string"),
            "{markup:?}"
        );

        let on_subcommand = hover("string toupper x", at(0, 9)).expect("a subcommand");
        let HoverContents::Markup(markup) = on_subcommand.contents else {
            panic!("expected markup");
        };
        assert!(markup.value.contains("string toupper"), "{markup:?}");
    }

    #[test]
    fn hover_says_nothing_about_a_word_that_is_not_a_command() {
        assert!(hover("puts hello", at(0, 7)).is_none());
        assert!(hover("", at(0, 0)).is_none());
    }

    #[test]
    fn signature_help_follows_the_command_being_written() {
        let help = signature_help("lsort -unique ", at(0, 14)).expect("a signature");
        assert_eq!(help.signatures[0].label, "lsort ?-option value ...? list");
        // An ensemble answers with its subcommand's synopsis, not its own.
        let help = signature_help("string repeat ab ", at(0, 17)).expect("a signature");
        assert_eq!(help.signatures[0].label, "string repeat string count");
    }

    #[test]
    fn symbols_are_the_procedures_the_parser_found() {
        let uri: Uri = "file:///t.tcl".parse().expect("uri");
        let text = "set x 1\nproc double {x} {expr {$x * 2}}\nproc quad {x} {double [double $x]}\n";
        let symbols = document_symbols(&uri, text);
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["double", "quad"]);
        assert_eq!(symbols[0].location.range.start.line, 1);
    }

    /// A position is UTF-16 code units, and the document is bytes. A line with
    /// a character outside the basic plane is where the two disagree.
    #[test]
    fn positions_are_counted_in_utf16_code_units() {
        let text = "puts 𝄞x\n";
        let index = LineIndex::new(text);
        // The surrogate pair counts as two units, so `x` is at 5 + 2.
        let position = index.position(text.find('x').expect("the x"));
        assert_eq!(position, at(0, 7));
        assert_eq!(index.offset(position), text.find('x').expect("the x"));
    }
}
