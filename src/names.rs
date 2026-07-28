//! The command vocabulary, for whoever needs to offer it rather than compile
//! it.
//!
//! The compiler learns a command name by matching it — `compiler.rs` for the
//! ones it lowers itself, `cmd_list.rs` for the rest — and an ensemble learns a
//! subcommand by resolving it against its own table. None of those are lists
//! anyone can read: the REPL's completer needs one, so it is assembled here
//! from the same constants the matches are written beside, never copied.
//!
//! Nothing in the compile path calls this module. It exists so that a name
//! offered at the prompt is a name the compiler knows.

use crate::assoc::{ARRAY_SUBCOMMANDS, DICT_SUBCOMMANDS};
use crate::cmd_list;
use crate::cmd_string;
use crate::compiler::Compiler;

/// Every command name the compiler accepts: the ones it lowers itself, then
/// the list commands it forwards. Sorted and free of duplicates, because a
/// completion menu is read by eye.
pub fn commands() -> Vec<&'static str> {
    let mut all: Vec<&'static str> = Compiler::BUILTINS
        .iter()
        .copied()
        .chain(cmd_list::COMMANDS.iter().copied())
        .collect();
    all.sort_unstable();
    all.dedup();
    all
}

/// The subcommands of an ensemble command, or an empty slice for a command
/// that has none. `info` is not an ensemble here in the way the others are —
/// the frontend supports exactly one of its subcommands (`coro.rs`) — so that
/// one word is what it offers.
pub fn subcommands(command: &str) -> &'static [&'static str] {
    match command {
        "string" => cmd_string::SUBCOMMANDS,
        "array" => ARRAY_SUBCOMMANDS,
        "dict" => DICT_SUBCOMMANDS,
        "info" => &["coroutine"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_sorted_and_cover_both_halves() {
        let all = commands();
        assert!(all.windows(2).all(|w| w[0] < w[1]), "not sorted or deduped");
        // One name from the compiler's own match, one it forwards to cmd_list.
        assert!(all.contains(&"proc"));
        assert!(all.contains(&"lsort"));
    }

    /// Every name offered has to be one the compiler knows — the point of the
    /// module. `invalid command name` is the compiler's answer for a name it
    /// does not; any other complaint (wrong argument count, say) means the name
    /// itself was recognized.
    #[test]
    fn every_offered_command_is_known_to_the_compiler() {
        for name in commands() {
            let err = crate::runtime::compile(name).err().unwrap_or_default();
            assert!(
                !err.contains("invalid command name"),
                "{name} is offered for completion but not compiled: {err}"
            );
        }
    }

    #[test]
    fn ensembles_offer_their_subcommands_and_others_offer_none() {
        assert!(subcommands("string").contains(&"toupper"));
        assert!(subcommands("array").contains(&"names"));
        assert!(subcommands("dict").contains(&"keys"));
        assert!(subcommands("info").contains(&"coroutine"));
        assert!(subcommands("puts").is_empty());
    }
}
