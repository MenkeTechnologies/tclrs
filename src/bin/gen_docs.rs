//! Offline generator for `docs/reference.html` — the command reference page.
//!
//! Run it before publishing GitHub Pages. Every entry on the page comes from
//! the crate itself: the command list from `names::commands()` (the compiler's
//! own `BUILTINS` plus `cmd_list::COMMANDS`), the ensemble tables from
//! `names::subcommands()`, the operator ladder from `expr::LEVELS` — the table
//! the parser binds with — and the `string is` classes from `cmd_string`. The
//! descriptions come from the corpora in `names`, each pinned to its source
//! table by a test, so the page cannot document a name the frontend refuses or
//! omit one it accepts.
//!
//! Whether an ensemble subcommand or a `format` conversion is *implemented* is
//! not asserted either: this binary asks the runtime, by putting one through it
//! and reading what came back. Asking the *compiler* would not do — a refusal
//! is lowered as code that raises when reached, so everything compiles.

use std::fmt::Write as _;

use tclrs::names::{self, Entry};

fn main() {
    let commands = names::commands();
    // Appended rather than sorted, so that adding one to this list adds a
    // section to the page instead of reordering the ones already there.
    let ensembles: Vec<Ensemble> = [
        "string",
        "array",
        "dict",
        "info",
        "namespace",
        "package",
        "clock",
        "file",
        "encoding",
    ]
    .iter()
    .map(|name| Ensemble::probe(name))
    .collect();
    let conversions = conversions();

    let operator_count: usize = tclrs::expr::LEVELS.iter().map(|l| l.len()).sum();
    let sub_total: usize = ensembles.iter().map(|e| e.subs.len()).sum();
    let sub_ok: usize = ensembles.iter().map(Ensemble::implemented).sum();
    let version = env!("CARGO_PKG_VERSION");
    let count = commands.len();

    let command_entries = entries("doc-cmd", names::CORPUS, |e| e.name.to_string());
    let ensemble_chapters = ensembles.iter().map(Ensemble::chapter).collect::<String>();

    // The ladder is the one place a table earns its keep: what it shows is the
    // *grouping* of operators into levels, which a list of entries cannot.
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

    // A word operator and a symbolic one can slug to the same text — `lt` and
    // `<` both become "lt" — so the two halves of the ladder take different
    // anchor prefixes rather than a disambiguating suffix nobody can guess.
    let binary_entries = names::BINARY_OPERATORS
        .iter()
        .map(|e| {
            let word = e.name.chars().all(|c| c.is_ascii_alphabetic());
            let prefix = if word { "doc-expr-kw" } else { "doc-expr-op" };
            entry(prefix, e, e.name, None)
        })
        .collect::<String>();
    let other_entries = entries("doc-expr-un", names::OTHER_OPERATORS, |e| {
        e.name.to_string()
    });
    let operand_entries = entries("doc-expr-val", names::OPERANDS, |e| e.name.to_string());
    let class_entries = entries("doc-class", names::CLASS_CORPUS, |e| e.name.to_string());
    let modifier_entries = entries("doc-format-size", names::MODIFIER_CORPUS, |e| {
        e.name.to_string()
    });

    // `format` is probed the way the ensembles are, so a conversion that stops
    // working is reported here rather than quietly claimed.
    let conversion_entries = names::CONVERSION_CORPUS
        .iter()
        .map(|e| {
            let conv = e.name.chars().nth(1).expect("a %x name");
            let state = conversions.iter().find(|(c, _)| *c == conv).map(|(_, s)| s);
            entry("doc-format-conv", e, e.name, state)
        })
        .collect::<String>();
    let conversions_ok = conversions.iter().filter(|(_, s)| s.ok()).count();

    let expr_count =
        names::BINARY_OPERATORS.len() + names::OTHER_OPERATORS.len() + names::OPERANDS.len();
    let format_count = names::CONVERSION_CORPUS.len() + names::MODIFIER_CORPUS.len();
    let entry_total = count + sub_total + expr_count + names::CLASS_CORPUS.len() + format_count;

    let index = chapter_index(&[
        ("ch-commands", "Commands", count),
        (
            "ch-string",
            "The <code>string</code> ensemble",
            ensembles[0].subs.len(),
        ),
        (
            "ch-array",
            "The <code>array</code> ensemble",
            ensembles[1].subs.len(),
        ),
        (
            "ch-dict",
            "The <code>dict</code> ensemble",
            ensembles[2].subs.len(),
        ),
        (
            "ch-info",
            "The <code>info</code> ensemble",
            ensembles[3].subs.len(),
        ),
        (
            "ch-expr",
            "<code>expr</code> operators and operands",
            expr_count,
        ),
        (
            "ch-classes",
            "<code>string is</code> classes",
            names::CLASS_CORPUS.len(),
        ),
        ("ch-format", "<code>format</code> conversions", format_count),
    ]);

    let binary_count = names::BINARY_OPERATORS.len();
    let other_count = names::OTHER_OPERATORS.len();
    let operand_count = names::OPERANDS.len();
    let class_count = names::CLASS_CORPUS.len();
    // Summed from the range table rather than typed, so it cannot outlive it.
    let uncategorised = tclrs::cmd_string::beyond_our_tables_count();
    let conversion_count = names::CONVERSION_CORPUS.len();
    let modifier_count = names::MODIFIER_CORPUS.len();

    let page = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="color-scheme" content="dark light">
  <meta name="description" content="tclrs — Command reference. Every one of the {entry_total} names the current tclrs build implements: {count} commands, {sub_total} ensemble subcommands, the expr operator ladder, the string is classes and the format conversions — each with a signature and a description, generated from the compiler's own tables. MIT licensed.">
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

    .chapter-index {{
      list-style: none; padding: 0; margin: 0;
      display: grid; grid-template-columns: repeat(auto-fill, minmax(18rem, 1fr));
      gap: 0.3rem;
    }}
    .chapter-index li {{
      border: 1px solid var(--border); padding: 0.45rem 0.65rem; border-radius: 2px;
      background: color-mix(in srgb, var(--bg-card) 92%, transparent);
      display: flex; justify-content: space-between; align-items: baseline;
    }}
    .chapter-index li a {{
      color: var(--cyan); text-decoration: none; font-size: 13px;
      font-family: 'Share Tech Mono', ui-monospace, monospace;
    }}
    .chapter-index li a:hover {{ color: var(--accent-light); }}
    .chapter-count {{
      font-size: 10px; color: var(--text-muted);
      font-family: 'Share Tech Mono', ui-monospace, monospace;
    }}
    .chapter-meta {{
      font-size: 11px; color: var(--text-muted); margin: -0.3rem 0 0.8rem;
      font-family: 'Share Tech Mono', ui-monospace, monospace;
    }}

    .doc-entry {{
      margin: 1rem 0 1.4rem;
      padding: 0.75rem 0.9rem 0.5rem;
      border-left: 2px solid var(--cyan);
      background: color-mix(in srgb, var(--bg) 94%, transparent);
      border-radius: 2px;
    }}
    .doc-entry h3 {{
      margin: 0 0 0.45rem;
      font-family: 'Orbitron', sans-serif;
      font-size: 13px; font-weight: 700; letter-spacing: 1.5px;
      text-transform: uppercase; color: var(--cyan);
    }}
    .doc-entry h3 code {{
      color: var(--accent-light); background: transparent; border: none;
      padding: 0; font-size: 1em; letter-spacing: 0.5px;
    }}
    .doc-entry .doc-anchor {{
      color: var(--text-muted); font-size: 0.85em; margin-right: 0.25rem;
      text-decoration: none;
    }}
    .doc-entry .doc-anchor:hover {{ color: var(--accent); }}
    .doc-entry p {{
      font-size: 13px; line-height: 1.6; color: var(--text-dim);
      margin: 0.35rem 0;
    }}
    .doc-entry p code {{ color: var(--accent-light); font-size: 12px; }}
    .doc-entry pre {{
      font-family: 'Share Tech Mono', ui-monospace, monospace;
      font-size: 12px;
      background: var(--bg); border: 1px solid var(--border);
      border-radius: 2px;
      padding: 0.7rem 0.9rem; overflow-x: auto;
      color: var(--text); margin: 0.5rem 0;
      box-shadow: inset 0 0 18px rgba(0, 0, 0, 0.35);
    }}
    .doc-entry pre code {{ color: var(--text); background: transparent; border: none; padding: 0; }}
    [data-theme="light"] .doc-entry pre {{ box-shadow: inset 0 0 10px rgba(0, 0, 0, 0.05); }}
    .doc-state {{ font-size: 12px; }}
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
      <h2 class="tutorial-title"><span class="step-hash">&gt;_</span>LANGUAGE REFERENCE</h2>
      <p class="tutorial-subtitle">Every name the current tclrs build knows &mdash; {count} commands,
      {sub_total} ensemble subcommands, the <code>expr</code> ladder, the
      <code>string is</code> classes and the <code>format</code> conversions &mdash; each with the
      signature the compiler reports and a description written from what the code does.
      {entry_total} entries in all. Jump via the chapter index, or <kbd>Ctrl+F</kbd> for a name.</p>
{index}
      <section class="tutorial-section" id="ch-commands">
        <h2>Commands</h2>
        <p class="chapter-meta">{count} entries</p>
        <p>Every command the current build compiles. A name absent from this
        chapter is <code>invalid command name "&hellip;"</code> at <em>compile</em>
        time, not a runtime lookup that fails &mdash; resolving the name while
        compiling is what turns a call into a <code>Op::Call</code> to a known
        entry. The list is the compiler's own: the names it matches itself, plus
        the list commands it forwards. Each signature is the wording of the
        <code>wrong # args</code> message at that command's compile site, so a
        reader who provokes the error sees this text.</p>

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

{command_entries}      </section>

{ensemble_chapters}
      <section class="tutorial-section" id="ch-expr">
        <h2>The <code>expr</code> operators and operands</h2>
        <p class="chapter-meta">{binary_count} binary operators &middot; {other_count} unary and ternary &middot; {operand_count} operand shapes</p>
        <p>Precedence levels, loosest first, printed from the table the parser
        binds with. Unary <code>+ - ~ !</code> bind tighter than every level
        below, and the ternary <code>?:</code> looser; <code>**</code> is the
        one right-associative level. The string comparisons share a level with
        their numeric counterparts, as <code>expr(n)</code> specifies &mdash; so
        <code>"a" eq "a" == 1</code> is 1. Tcl's integer arithmetic is its own
        rather than C's in most of these operators, and each entry says where.</p>
        <table class="file-table">
          <colgroup><col style="width:10%"><col style="width:60%"><col style="width:30%"></colgroup>
          <thead><tr><th>level</th><th>operators</th><th>associativity</th></tr></thead>
          <tbody>
{operator_rows}          </tbody>
        </table>

{binary_entries}{other_entries}{operand_entries}      </section>

      <section class="tutorial-section" id="ch-classes">
        <h2><code>string is</code> classes</h2>
        <p class="chapter-meta">{class_count} entries</p>
        <p>Resolved while compiling, in the interpreter's own listing order, and
        checked left to right so a decision reached early is still a decision.
        The classes that rest on Unicode general-category tables accept ASCII
        &mdash; where the two implementations were verified to agree exactly &mdash; and
        raise on the {uncategorised} code points tclsh 9.0.4 categorises and Unicode 16.0
        does not, rather than answering from a different Unicode revision than
        the reference interpreter's. Every class but <code>list</code> and
        <code>dict</code> answers 1 for the empty string unless
        <code>-strict</code> is given.</p>
{class_entries}      </section>

      <section class="tutorial-section" id="ch-format">
        <h2><code>format</code> conversions</h2>
        <p class="chapter-meta">{conversion_count} conversions &middot; {conversions_ok} implemented &middot; {modifier_count} size modifiers</p>
        <p>Each conversion below was established by running one of it through
        the runtime and reading the answer. A letter missing from this chapter
        is <code>bad field specifier</code>. Width and precision both accept
        <code>*</code>, consuming an argument; a negative width sets <code>-</code>
        and negates. Justification follows tclsh 9.0.4 rather than C99, which
        means <code>-</code> does not override <code>0</code> for an integer
        (<code>%-08d 42</code> is <code>00000042</code>) but does for everything
        else (<code>%-08.2f 42</code> is <code>42.00&nbsp;&nbsp;&nbsp;</code>).</p>
{conversion_entries}
        <h3>Size modifiers</h3>
        <p>Consumed between the field and the conversion character. Only the
        integer conversions read the width; a modifier with nothing after it is
        <code>format string ended in middle of field specifier</code>.</p>
{modifier_entries}      </section>
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
    println!(
        "wrote {out} ({entry_total} entries: {count} commands, {sub_ok} of {sub_total} \
         subcommands implemented, {expr_count} expr, {class_count} classes, {format_count} format)"
    );
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

    /// The line an entry carries when the runtime refused it. An implemented
    /// name says nothing: 56 identical "implemented" lines would be noise, and
    /// the chapter heading already carries the count.
    fn line(&self) -> String {
        match self {
            State::Implemented => String::new(),
            State::Refused(msg) => format!(
                "        <p class=\"doc-state\"><span class=\"state-no\">Refused at run time:</span> \
                 <code>{}</code></p>\n",
                escape(msg)
            ),
        }
    }
}

/// One ensemble command and the state of each subcommand it recognises.
struct Ensemble {
    name: &'static str,
    subs: Vec<(&'static str, State)>,
}

impl Ensemble {
    /// Ask the runtime about every subcommand rather than keeping a second list
    /// beside the one that refuses them. A subcommand is *run* with each
    /// argument count it might want; a refusal it reports for every one of them
    /// is the subcommand's answer, and anything else — succeeding, or
    /// complaining about the arguments — means the subcommand itself was built.
    ///
    /// Running is what makes the answer true. A refusal here is lowered as code
    /// that raises when reached (`compiler::defers_to_run_time`), so *compiling*
    /// `array startsearch a` succeeds and says nothing about whether the
    /// subcommand exists; only reaching the op does. The earlier compile-time
    /// probe reported all 87 subcommands as implemented for exactly that reason.
    ///
    /// `Interp::capturing` gives each attempt its own interpreter and swallows
    /// what the script writes, so probing cannot leak a variable into the next
    /// attempt or a line of output onto this program's stdout.
    fn probe(name: &'static str) -> Ensemble {
        let subs = names::subcommands(name)
            .iter()
            .map(|sub| {
                let mut refusal = None;
                for argc in 0..=4 {
                    let args = ["a", "b", "c", "d"][..argc].join(" ");
                    match tclrs::eval(&format!("{name} {sub} {args}")) {
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

    /// The chapter for this ensemble: one entry per subcommand, each carrying
    /// the description from the corpus and, when the runtime refused it, the
    /// refusal in the frontend's own words.
    fn chapter(&self) -> String {
        let corpus = names::subcommand_corpus(self.name);
        let body = corpus
            .iter()
            .zip(self.subs.iter())
            .map(|(e, (_, state))| {
                let heading = format!("{} {}", self.name, e.name);
                entry(&format!("doc-{}", self.name), e, &heading, Some(state))
            })
            .collect::<String>();
        format!(
            "      <section class=\"tutorial-section\" id=\"ch-{name}\">\n        \
             <h2>The <code>{name}</code> ensemble</h2>\n        \
             <p class=\"chapter-meta\">{all} subcommands recognised &middot; {ok} implemented \
             &middot; {no} refused</p>\n        \
             <p>{intro}</p>\n{body}      </section>\n\n",
            name = escape(self.name),
            all = self.subs.len(),
            ok = self.implemented(),
            no = self.subs.len() - self.implemented(),
            intro = self.intro(),
        )
    }

    /// What a reader has to know before the entries make sense: why a
    /// subcommand can be listed and refused at the same time.
    fn intro(&self) -> &'static str {
        "An ensemble resolves its subcommand the way <code>Tcl_GetIndexFromObj</code> \
         does &mdash; an exact match, else a prefix that fits exactly one entry &mdash; so this \
         chapter carries the names the frontend <em>recognises</em>, which is what decides \
         whether an abbreviation is ambiguous. Recognising a name is not implementing it. \
         Each entry says what the subcommand does in the language; an entry the runtime \
         refused says so, in the frontend's own words, after the description. The refusal \
         is deferred rather than raised while compiling, so an unexecuted one costs nothing \
         and a <code>catch</code> can trap it."
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
            Err(e) if is_refusal(&e) => found.push((conv, State::Refused(strip_line(&e)))),
            // "bad field specifier" — not a conversion at all. "ended in middle
            // of field specifier" — a size modifier, which has a chapter of its
            // own. Anything else is this probe's own argument being wrong for a
            // conversion that does exist, so the conversion counts as built.
            Err(e)
                if e.contains("bad field specifier")
                    || e.contains("ended in middle of field specifier") => {}
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

/// One reference entry: an anchored heading, the signature in a fenced block
/// the print pipeline highlights as Tcl, the description, and — when the name
/// was probed and refused — what the runtime said.
fn entry(prefix: &str, e: &Entry, heading: &str, state: Option<&State>) -> String {
    let id = format!("{prefix}-{}", slug(e.name));
    format!(
        "        <article class=\"doc-entry\" id=\"{id}\">\n          \
         <h3><a class=\"doc-anchor\" href=\"#{id}\">#</a> <code>{heading}</code></h3>\n          \
         <pre><code class=\"lang-tcl\">{synopsis}\n</code></pre>\n          \
         <p>{summary}</p>\n{state}        </article>\n",
        heading = escape(heading),
        synopsis = escape(e.synopsis),
        summary = markup(e.summary),
        state = state.map(State::line).unwrap_or_default(),
    )
}

/// A whole corpus rendered as entries, with the heading taken from each entry.
fn entries(prefix: &str, corpus: &[Entry], heading: impl Fn(&Entry) -> String) -> String {
    corpus
        .iter()
        .map(|e| entry(prefix, e, &heading(e), None))
        .collect()
}

/// The index of chapters, which the print pipeline drops in favour of the
/// LaTeX table of contents — it is the one section deliberately left without an
/// `id` so that the drop can find it.
fn chapter_index(chapters: &[(&str, &str, usize)]) -> String {
    let mut out = String::from(
        "      <section class=\"tutorial-section\">\n        <h2>Chapters</h2>\n        \
         <ul class=\"chapter-index\">\n",
    );
    for (id, title, count) in chapters {
        let _ = writeln!(
            out,
            "          <li><a href=\"#{id}\">{title}</a> \
             <span class=\"chapter-count\">{count}</span></li>"
        );
    }
    out.push_str("        </ul>\n      </section>\n");
    out
}

/// An anchor fragment for a name that may be punctuation. Symbols become words
/// rather than disappearing, so `**` and `*` do not collide, and the case is
/// kept so `%x` and `%X` do not either.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            continue;
        }
        out.push_str(match c {
            '|' => "pipe",
            '&' => "amp",
            '^' => "caret",
            '=' => "eq",
            '!' => "bang",
            '<' => "lt",
            '>' => "gt",
            '+' => "plus",
            '-' => "minus",
            '*' => "star",
            '/' => "slash",
            '%' => "pct",
            '~' => "tilde",
            '?' => "q",
            ':' => "colon",
            _ => "-",
        });
    }
    // Runs of the separator collapse, so `inf / nan` is `inf-nan` and not
    // `inf---nan`.
    let mut collapsed = String::with_capacity(out.len());
    for c in out.chars() {
        if c == '-' && collapsed.ends_with('-') {
            continue;
        }
        collapsed.push(c);
    }
    collapsed.trim_matches('-').to_string()
}

/// Escape a description and render the three markers its prose uses:
/// `backticks` as `<code>`, `**bold**` as `<strong>`, `*italic*` as `<em>`.
///
/// The corpora are written as Rust-doc prose, which is what the LSP hover
/// already serves; the page renders that same source rather than keeping a
/// second, HTML-shaped copy of every description. Emphasis is not looked for
/// inside a code span — `2 ** 3` is exponentiation twice over, not bold.
fn markup(text: &str) -> String {
    let escaped: Vec<char> = escape(text).chars().collect();
    let mut out = String::with_capacity(escaped.len());
    let (mut code, mut strong, mut em) = (false, false, false);
    let mut i = 0;
    while i < escaped.len() {
        let c = escaped[i];
        if c == '`' {
            out.push_str(if code { "</code>" } else { "<code>" });
            code = !code;
            i += 1;
        } else if !code && c == '*' && escaped.get(i + 1) == Some(&'*') {
            out.push_str(if strong { "</strong>" } else { "<strong>" });
            strong = !strong;
            i += 2;
        } else if !code && c == '*' {
            out.push_str(if em { "</em>" } else { "<em>" });
            em = !em;
            i += 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    // An unbalanced marker would leave the page with an unclosed tag.
    for (open, close) in [(code, "</code>"), (strong, "</strong>"), (em, "</em>")] {
        if open {
            out.push_str(close);
        }
    }
    out
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
