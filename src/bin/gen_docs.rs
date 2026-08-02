//! Offline generator for `docs/reference.html` — the command reference page.
//!
//! Run it before publishing GitHub Pages. Every table on the page comes from
//! the crate itself: the command list from `names::commands()` (the compiler's
//! own `BUILTINS` plus `cmd_list::COMMANDS`), the ensemble tables from
//! `names::subcommands()`, the operator ladder from `expr::LEVELS` — the table
//! the parser binds with — and the `string is` classes from `cmd_string`.
//! Whether an ensemble subcommand or a `format` conversion is *implemented* is
//! not asserted either: this binary asks the compiler and the runtime, by
//! putting one through each and reading what came back. So the page cannot
//! claim a command the frontend refuses, and every count on it is computed.

use std::fmt::Write as _;

use tclrs::names::{self, Entry};

fn main() {
    let commands = names::commands();
    let ensembles: Vec<Ensemble> = ["string", "array", "dict", "clock", "file", "info"]
        .iter()
        .map(|name| Ensemble::probe(name))
        .collect();
    let conversions = conversions();

    let operator_count: usize = tclrs::expr::LEVELS.iter().map(|l| l.len()).sum();
    let sub_total: usize = ensembles.iter().map(|e| e.subs.len()).sum();
    let sub_ok: usize = ensembles.iter().map(Ensemble::implemented).sum();
    let version = env!("CARGO_PKG_VERSION");
    let count = commands.len();

    let command_rows = rows(names::CORPUS.iter().map(|e: &Entry| {
        format!(
            "<tr><td><code>{}</code></td><td><code>{}</code></td><td>{}</td></tr>",
            escape(e.name),
            escape(e.synopsis),
            escape(e.summary)
        )
    }));

    let ensemble_tables = ensembles.iter().map(Ensemble::table).collect::<String>();

    let operator_rows = rows(tclrs::expr::LEVELS.iter().enumerate().map(|(i, level)| {
        let ops = level
            .iter()
            .map(|(text, _)| format!("<code>{}</code>", escape(text)))
            .collect::<Vec<_>>()
            .join(" ");
        // `**` is the one right-associative level; `ExprParser::parse_binary`
        // recurses at the same level for it and one level up for the rest.
        let assoc = if level.iter().any(|(text, _)| *text == "**") {
            "right"
        } else {
            "left"
        };
        format!(
            "<tr><td>{}</td><td>{ops}</td><td>{assoc}</td></tr>",
            tclrs::expr::LEVELS.len() - i
        )
    }));

    let class_cells = tclrs::cmd_string::CLASSES
        .iter()
        .map(|c| format!("<code>{}</code>", escape(c)))
        .collect::<Vec<_>>()
        .join(" · ");

    let conversion_rows = rows(conversions.iter().map(|(conv, state)| {
        format!(
            "<tr><td><code>%{}</code></td><td>{}</td></tr>",
            escape(&conv.to_string()),
            state.cell()
        )
    }));
    let conversions_ok = conversions.iter().filter(|(_, s)| s.ok()).count();

    let page = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="color-scheme" content="dark light">
  <meta name="description" content="tclrs — Command reference. The {count} commands the current tclrs build compiles, their ensembles, and the expr operator ladder, generated from the compiler's own tables. MIT licensed.">
  <title>tclrs &mdash; Command Reference</title>
  <link rel="preconnect" href="https://fonts.googleapis.com">
  <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
  <link href="https://fonts.googleapis.com/css2?family=Orbitron:wght@400;600;700;900&amp;family=Share+Tech+Mono&amp;display=swap" rel="stylesheet">
  <link rel="stylesheet" href="hud-static.css">
  <link rel="stylesheet" href="tutorial.css">
  <style>
    .tutorial-main {{ max-width: 76rem; }}
    .file-table {{ width:100%;border-collapse:collapse;margin:0.6rem 0;font-size:12px; }}
    .file-table th {{ background:var(--bg-secondary);color:var(--cyan);font-family:'Orbitron',sans-serif;font-size:10px;font-weight:700;letter-spacing:1.2px;text-transform:uppercase;text-align:left;padding:7px 10px;border:1px solid var(--border); }}
    .file-table td {{ padding:6px 10px;border:1px solid var(--border);color:var(--text-dim);vertical-align:middle; }}
    .file-table tr:hover td {{ background:var(--bg-hover); }}
    .file-table td:first-child {{ font-family:'Share Tech Mono',monospace;color:var(--accent-light);font-weight:600;white-space:nowrap; }}
    .file-table code {{ font-size:11px;color:var(--accent-light);background:var(--bg-primary);padding:1px 4px;border-radius:2px; }}
    .stat-grid {{ display:grid;grid-template-columns:repeat(auto-fill,minmax(14rem,1fr));gap:0.75rem;margin:1.2rem 0; }}
    .stat-card {{ border:1px solid var(--border);border-top:3px solid var(--cyan);background:var(--bg-card);padding:1rem 1.2rem;border-radius:2px;text-align:center; }}
    .stat-card .stat-val {{ font-family:'Orbitron',sans-serif;font-size:28px;font-weight:900;color:var(--cyan);line-height:1.1;text-shadow:0 0 20px var(--cyan-glow); }}
    .stat-card .stat-val.accent {{ color:var(--accent);text-shadow:0 0 20px var(--accent-glow); }}
    .stat-card .stat-label {{ font-family:'Orbitron',sans-serif;font-size:9px;font-weight:700;letter-spacing:2px;text-transform:uppercase;color:var(--text-muted);margin-top:0.5rem; }}
    .docs-build-line {{ margin:0.35rem 0 0;font-family:'Share Tech Mono',ui-monospace,monospace;font-size:11px;color:var(--text-dim);letter-spacing:0.03em;max-width:42rem;opacity:0.75; }}
    .state-yes {{ color:var(--green,#39ff14);font-weight:600; }}
    .state-no {{ color:var(--accent,#ff2a6d);font-weight:600; }}
  </style>
</head>
<body>
  <div class="app tutorial-app" id="docsApp">
    <div class="crt-scanline" id="crtH" aria-hidden="true"></div>
    <div class="crt-scanline-v" id="crtV" aria-hidden="true"></div>

    <header class="tutorial-header">
      <div class="tutorial-header-inner">
        <div>
          <h1 class="tutorial-brand">// TCLRS &mdash; COMMAND REFERENCE</h1>
          <nav class="tutorial-crumbs" aria-label="Breadcrumb">
            <a href="index.html">Docs</a>
            <span class="sep">/</span>
            <a href="report.html">Engineering Report</a>
            <span class="sep">/</span>
            <span class="current">Command Reference</span>
            <span class="sep">/</span>
            <a href="https://github.com/MenkeTechnologies/tclrs" target="_blank" rel="noopener noreferrer">GitHub</a>
          </nav>
          <p class="docs-build-line">tclrs v{version} &middot; generated from the compiler's own tables &middot; MIT</p>
        </div>
        <div class="tutorial-toolbar">
          <button type="button" class="btn btn-secondary" id="btnTheme" title="Toggle light/dark">Theme</button>
          <button type="button" class="btn btn-secondary active" id="btnCrt" title="CRT scanline overlay">CRT</button>
          <button type="button" class="btn btn-secondary active" id="btnNeon" title="Neon border pulse">Neon</button>
          <a class="btn btn-secondary" href="index.html">Docs</a>
          <a class="btn btn-secondary" href="report.html">Report</a>
          <a class="btn btn-secondary" href="https://github.com/MenkeTechnologies/tclrs" target="_blank" rel="noopener noreferrer">GitHub</a>
        </div>
      </div>
    </header>

    <main class="tutorial-main">
      <section id="commands">
        <h2>Commands</h2>
        <p>Every command the current build compiles. A name absent from this
        table is <code>invalid command name "&hellip;"</code> at <em>compile</em>
        time, not a runtime lookup that fails — resolving the name while
        compiling is what turns a call into a <code>Op::Call</code> to a known
        entry. The list is the compiler's own: the names it matches itself, plus
        the list commands it forwards.</p>

        <div class="stat-grid">
          <div class="stat-card">
            <div class="stat-val">{count}</div>
            <div class="stat-label">Commands</div>
          </div>
          <div class="stat-card">
            <div class="stat-val">{sub_ok}/{sub_total}</div>
            <div class="stat-label">Ensemble subcommands</div>
          </div>
          <div class="stat-card">
            <div class="stat-val">{operator_count}</div>
            <div class="stat-label">expr binary operators</div>
          </div>
          <div class="stat-card">
            <div class="stat-val accent">v{version}</div>
            <div class="stat-label">Build</div>
          </div>
        </div>

        <table class="file-table">
          <colgroup><col style="width:14%"><col style="width:29%"><col style="width:57%"></colgroup>
          <thead><tr><th>command</th><th>synopsis</th><th>what it does</th></tr></thead>
          <tbody>
{command_rows}          </tbody>
        </table>
      </section>

      <section id="ensembles">
        <h2>Ensemble subcommands</h2>
        <p>An ensemble resolves its subcommand the way
        <code>Tcl_GetIndexFromObj</code> does — an exact match, else a prefix
        that fits exactly one entry — so the tables below carry the names the
        frontend <em>recognises</em>, which is what decides whether an
        abbreviation is ambiguous. Recognising a name is not implementing it:
        the second column is the answer the compiler actually gives when the
        subcommand is put through it.</p>
{ensemble_tables}      </section>

      <section id="expr">
        <h2>The <code>expr</code> operator ladder</h2>
        <p>Precedence levels, tightest last, printed from the table the parser
        binds with. Unary <code>+ - ~ !</code> bind tighter than every level
        below, and the ternary <code>?:</code> looser; <code>**</code> is the
        one right-associative level. The string comparisons share a level with
        their numeric counterparts, as <code>expr(n)</code> specifies — so
        <code>"a" eq "a" == 1</code> is 1.</p>
        <table class="file-table">
          <colgroup><col style="width:10%"><col style="width:60%"><col style="width:30%"></colgroup>
          <thead><tr><th>level</th><th>operators</th><th>associativity</th></tr></thead>
          <tbody>
{operator_rows}          </tbody>
        </table>
      </section>

      <section id="classes">
        <h2><code>string is</code> classes</h2>
        <p>Resolved while compiling, in the interpreter's own listing order. The
        classes that need Tcl's Unicode general-category tables accept ASCII and
        report an error otherwise, rather than answering from a different
        Unicode revision than the reference interpreter's.</p>
        <p>{class_cells}</p>
      </section>

      <section id="format">
        <h2><code>format</code> conversions</h2>
        <p>{conversions_ok} conversions, established by running one of each
        through the runtime and reading the answer. A conversion missing from
        this table is <code>bad field specifier</code>.</p>
        <table class="file-table">
          <colgroup><col style="width:20%"><col style="width:80%"></colgroup>
          <thead><tr><th>conversion</th><th>state</th></tr></thead>
          <tbody>
{conversion_rows}          </tbody>
        </table>
      </section>
    </main>
  </div>
  <script src="hud-theme.js"></script>
</body>
</html>
"#
    );

    let out = "docs/reference.html";
    if let Err(e) = std::fs::write(out, page) {
        eprintln!("gen-docs: cannot write {out}: {e}");
        std::process::exit(1);
    }
    // Explicit user-requested output: this binary exists to report what it wrote.
    println!("wrote {out} ({count} commands, {sub_total} subcommands, {operator_count} operators)");
}

/// Whether a name the frontend recognises is one it also implements — and if
/// not, the refusal in its own words.
enum State {
    Implemented,
    Refused(String),
}

impl State {
    fn ok(&self) -> bool {
        matches!(self, State::Implemented)
    }

    fn cell(&self) -> String {
        match self {
            State::Implemented => "<span class=\"state-yes\">implemented</span>".into(),
            State::Refused(msg) => format!("<span class=\"state-no\">{}</span>", escape(msg)),
        }
    }
}

/// One ensemble command and the state of each subcommand it recognises.
struct Ensemble {
    name: &'static str,
    subs: Vec<(&'static str, State)>,
}

impl Ensemble {
    /// Ask the compiler about every subcommand rather than keeping a second
    /// list beside the one that refuses them. A subcommand is put through the
    /// compiler with each argument count it might want; a refusal it reports
    /// for every one of them is the subcommand's answer, and anything else —
    /// compiling, or complaining about the arguments — means the subcommand
    /// itself was accepted.
    fn probe(name: &'static str) -> Ensemble {
        let subs = names::subcommands(name)
            .iter()
            .map(|sub| {
                let mut refusal = None;
                for argc in 0..=4 {
                    let args = ["a", "b", "c", "d"][..argc].join(" ");
                    match tclrs::runtime::compile(&format!("{name} {sub} {args}")) {
                        Ok(_) => return (*sub, State::Implemented),
                        Err(e) if is_refusal(&e) => refusal = Some(strip_line(&e)),
                        Err(_) => return (*sub, State::Implemented),
                    }
                }
                match refusal {
                    Some(msg) => (*sub, State::Refused(msg)),
                    None => (*sub, State::Implemented),
                }
            })
            .collect();
        Ensemble { name, subs }
    }

    fn implemented(&self) -> usize {
        self.subs.iter().filter(|(_, s)| s.ok()).count()
    }

    fn table(&self) -> String {
        let body = rows(self.subs.iter().map(|(sub, state)| {
            format!(
                "<tr><td><code>{} {}</code></td><td>{}</td></tr>",
                escape(self.name),
                escape(sub),
                state.cell()
            )
        }));
        format!(
            "        <h3><code>{name}</code> &mdash; {ok} of {all} implemented</h3>\n        \
             <table class=\"file-table\">\n          \
             <colgroup><col style=\"width:32%\"><col style=\"width:68%\"></colgroup>\n          \
             <thead><tr><th>subcommand</th><th>state</th></tr></thead>\n          <tbody>\n\
             {body}          </tbody>\n        </table>\n",
            name = escape(self.name),
            ok = self.implemented(),
            all = self.subs.len(),
        )
    }
}

/// Every `format` conversion the runtime answers to, found by running one.
/// Probing the whole alphabet rather than listing the ones expected to work is
/// what keeps this from becoming a second, drifting copy of the conversion
/// table in `cmd_string`.
fn conversions() -> Vec<(char, State)> {
    let mut found = Vec::new();
    for conv in ('a'..='z').chain('A'..='Z') {
        let script = format!("format %{conv} 1");
        match tclrs::eval(&script) {
            Ok(_) => found.push((conv, State::Implemented)),
            Err(e) if is_refusal(&e.to_string()) => {
                found.push((conv, State::Refused(strip_line(&e.to_string()))))
            }
            // "bad field specifier" — not a conversion at all. Anything else is
            // this probe's own argument being wrong for a conversion that does
            // exist, so the conversion counts as implemented.
            Err(e) if e.to_string().contains("bad field specifier") => {}
            Err(_) => found.push((conv, State::Implemented)),
        }
    }
    found
}

/// The frontend's one wording for "recognised, not built".
fn is_refusal(msg: &str) -> bool {
    msg.contains("is not supported yet")
}

/// Compile errors carry ` (line N)`, which is noise on a reference page.
fn strip_line(msg: &str) -> String {
    match msg.find(" (line ") {
        Some(at) => msg[..at].to_string(),
        None => msg.to_string(),
    }
}

fn rows(cells: impl Iterator<Item = String>) -> String {
    let mut out = String::new();
    for cell in cells {
        let _ = writeln!(out, "        {cell}");
    }
    out
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
