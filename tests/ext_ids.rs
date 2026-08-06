//! The extension-op id map, checked.
//!
//! Two modules claiming the same id is not a build error and not a crash: the
//! first arm that matches wins, so the op quietly calls the other module's
//! handler and the script gets a wrong answer. That has happened in this tree,
//! which is why the map is asserted here rather than trusted to review.
//!
//! Every id below comes from the module that owns it, so a constant that moves
//! moves here too — the test pins the *shape* of the map (disjoint, in-block),
//! not a list of numbers copied out of the source.

use tclrs::compiler::ext;

/// Every id the frontend can emit, with the name to print when one collides.
fn all_ids() -> Vec<(&'static str, u16)> {
    let mut ids: Vec<(&'static str, u16)> = vec![
        // ── the core op space, 0–63 ──────────────────────────────────────
        ("DIV", ext::DIV),
        ("MOD", ext::MOD),
        ("POW", ext::POW),
        ("IN", ext::IN),
        ("NI", ext::NI),
        ("PUTS", ext::PUTS),
        ("EVAL", ext::EVAL),
        ("MATCH", ext::MATCH),
        ("ERROR", ext::ERROR),
        ("CATCH_END", ext::CATCH_END),
        ("CORO_CREATE", ext::CORO_CREATE),
        ("CORO_RESUME", ext::CORO_RESUME),
        ("CORO_YIELD", ext::CORO_YIELD),
        ("CORO_YIELDTO", ext::CORO_YIELDTO),
        ("CORO_INFO", ext::CORO_INFO),
        ("BOOL", ext::BOOL),
        ("CANON", ext::CANON),
        ("PROC_DEFINE", ext::PROC_DEFINE),
        ("DYN_CALL", ext::DYN_CALL),
        ("EXPAND_CALL", ext::EXPAND_CALL),
        ("STR_CMP", ext::STR_CMP),
        ("FFI_CALL", ext::FFI_CALL),
        // The list block, 16–34 and 48–57.
        ("LIST", ext::LIST),
        ("LLENGTH", ext::LLENGTH),
        ("LINDEX", ext::LINDEX),
        ("LAPPEND", ext::LAPPEND),
        ("LRANGE", ext::LRANGE),
        ("LREVERSE", ext::LREVERSE),
        ("LINSERT", ext::LINSERT),
        ("LREPLACE", ext::LREPLACE),
        ("LSEARCH", ext::LSEARCH),
        ("LSORT", ext::LSORT),
        ("JOIN", ext::JOIN),
        ("SPLIT", ext::SPLIT),
        ("CONCAT", ext::CONCAT),
        ("FOREACH_INIT", ext::FOREACH_INIT),
        ("FOREACH_MORE", ext::FOREACH_MORE),
        ("FOREACH_TAKE", ext::FOREACH_TAKE),
        ("FOREACH_ADVANCE", ext::FOREACH_ADVANCE),
        ("LAPPEND_VAR", ext::LAPPEND_VAR),
        ("LAPPEND_SLOT", ext::LAPPEND_SLOT),
        ("LASSIGN", ext::LASSIGN),
        ("LSET", ext::LSET),
        ("LPOP", ext::LPOP),
        ("LEDIT", ext::LEDIT),
        ("LREPEAT", ext::LREPEAT),
        ("LREMOVE", ext::LREMOVE),
        ("LSEQ", ext::LSEQ),
        ("LMAP_INIT", ext::LMAP_INIT),
        ("LMAP_COLLECT", ext::LMAP_COLLECT),
        ("LMAP_RESULT", ext::LMAP_RESULT),
        // The bitwise block, 40–46.
        ("BIT_AND", ext::BIT_AND),
        ("BIT_OR", ext::BIT_OR),
        ("BIT_XOR", ext::BIT_XOR),
        ("SHL", ext::SHL),
        ("SHR", ext::SHR),
        ("BIT_NOT", ext::BIT_NOT),
        ("UPLUS", ext::UPLUS),
    ];
    ids.extend(module_ids());
    ids
}

/// One module's block: the name to print, where the block starts, and the ids
/// allocated inside it.
type Block = (&'static str, u16, Vec<(&'static str, u16)>);

/// The ids the command modules own, each paired with the block it must fall in.
fn blocks() -> Vec<Block> {
    vec![
        (
            "CHANNEL",
            ext::CHANNEL_BASE,
            vec![
                ("OPEN", ext::OPEN),
                ("CLOSE", ext::CLOSE),
                ("GETS", ext::GETS),
                ("READ", ext::READ),
                ("CH_PUTS", ext::CH_PUTS),
                ("FLUSH", ext::FLUSH),
                ("EOF", ext::EOF),
                ("SEEK", ext::SEEK),
                ("TELL", ext::TELL),
                ("FCONFIGURE", ext::FCONFIGURE),
            ],
        ),
        (
            "NS",
            ext::NS_BASE,
            vec![
                ("NS", tclrs::cmd_namespace::ext::NS),
                ("RENAME_GUARD", tclrs::cmd_namespace::ext::RENAME_GUARD),
                ("SOURCE", tclrs::cmd_source::ext::SOURCE),
                ("FIND_LIBRARY", tclrs::cmd_source::ext::FIND_LIBRARY),
            ],
        ),
        (
            "EVENT",
            ext::EVENT_BASE,
            vec![
                ("AFTER", ext::AFTER),
                ("UPDATE", ext::UPDATE),
                ("VWAIT", ext::VWAIT),
                ("UPLEVEL", ext::UPLEVEL),
                ("INFO_EXISTS", ext::INFO_EXISTS),
                ("INFO_NAMES", ext::INFO_NAMES),
                ("INFO_LEVEL", ext::INFO_LEVEL),
                ("INFO_COMPLETE", ext::INFO_COMPLETE),
                ("UPVAR", ext::UPVAR),
                ("LINK_GET", ext::LINK_GET),
                ("LINK_SET", ext::LINK_SET),
            ],
        ),
        ("PKG", ext::PKG_BASE, vec![("PACKAGE", ext::PACKAGE)]),
        // The math block is indexed by position in `expr_math`'s own table
        // rather than named here, so the block check below is the whole of
        // what this file can assert about it; `expr_math`'s own test pins the
        // table against `CLOCK_BASE`.
        ("MATH", ext::MATH_BASE, vec![("MATH_BASE", ext::MATH_BASE)]),
        (
            "CLOCK",
            ext::CLOCK_BASE,
            vec![("CLOCK_BASE", ext::CLOCK_BASE)],
        ),
        ("FILE", ext::FILE_BASE, vec![("FILE_BASE", ext::FILE_BASE)]),
    ]
}

fn module_ids() -> Vec<(&'static str, u16)> {
    blocks().into_iter().flat_map(|(_, _, ids)| ids).collect()
}

/// No two ops share an id. A duplicate is a wrong answer at run time, not a
/// build failure, so it has to be caught here.
#[test]
fn every_extension_id_is_claimed_once() {
    let mut seen: std::collections::HashMap<u16, &'static str> = std::collections::HashMap::new();
    let mut clashes = Vec::new();
    for (name, id) in all_ids() {
        // `LIST` and `LIST_BASE` are the same op under two names, and `SCALAR`
        // is `ASSOC_BASE`; only distinct *ops* are listed above, so any repeat
        // reaching here is two ops on one number.
        if let Some(other) = seen.insert(id, name) {
            clashes.push(format!("{id}: {other} and {name}"));
        }
    }
    assert!(
        clashes.is_empty(),
        "extension ids claimed twice: {clashes:?}"
    );
}

/// Every module's ids sit inside the block that module was given, and the
/// blocks do not overlap. This is what keeps the range dispatch in
/// `runtime::extension` — which tests `id >= REGEXP_BASE` first — from routing
/// one module's op into another's handler.
#[test]
fn every_module_stays_inside_its_block() {
    for (block, base, ids) in blocks() {
        let end = base + ext::BLOCK;
        for (name, id) in ids {
            assert!(
                (base..end).contains(&id),
                "{block}::{name} is {id}, outside its block {base}..{end}"
            );
        }
    }
}

/// The core op space and the three ranges above it stay clear of the module
/// blocks, and the module blocks start where the dispatcher expects.
#[test]
fn the_block_map_is_ordered() {
    // A `const` block, because these are all compile-time constants and a
    // plain `assert!` over them is a lint rather than a test. The build fails
    // if the order is ever broken, which is stronger than a failing run.
    const _: () = {
        assert!(ext::LIST_BASE < ext::ASSOC_BASE);
        assert!(ext::ASSOC_BASE < ext::STRING_BASE);
        assert!(ext::STRING_BASE < ext::REGEXP_BASE);
        assert!(ext::REGEXP_BASE < ext::SUBSYSTEM_BASE);
        assert!(ext::CHANNEL_END == ext::CHANNEL_BASE + ext::BLOCK);
        assert!(ext::CHANNEL_END == ext::NS_BASE);
    };
    // Every core op is below the first module block, or the guard chain in
    // `runtime::extension` would reach it before its own arm.
    for (name, id) in all_ids() {
        if id < ext::SUBSYSTEM_BASE {
            assert!(
                id < ext::REGEXP_BASE,
                "{name} is {id}, between REGEXP_BASE and the module blocks"
            );
        }
    }
}

/// The blocks themselves do not overlap, whatever is allocated inside them.
///
/// Every one of these was picked independently on a branch of its own, and
/// four of the seven collided when the branches met: `namespace` and `after`
/// both took 35, `package` and `info commands` both took 58, `proc` and `info
/// complete` both took 60, and `clock`/`file`/`expr`'s math functions landed
/// on top of channels, namespaces and the event loop. This is the assertion
/// that would have caught all of them.
#[test]
fn the_blocks_do_not_overlap() {
    let mut spans: Vec<(&str, u16, u16)> = blocks()
        .iter()
        .map(|(name, base, _)| (*name, *base, *base + ext::BLOCK))
        .collect();
    spans.sort_by_key(|(_, base, _)| *base);
    for pair in spans.windows(2) {
        let [(a, _, a_end), (b, b_base, _)] = pair else {
            unreachable!("windows(2)")
        };
        assert!(
            a_end <= b_base,
            "block {a} ends at {a_end} and block {b} starts at {b_base}"
        );
    }
}
