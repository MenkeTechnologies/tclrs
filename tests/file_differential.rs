//! Differential execution of `file`, `glob`, `pwd` and `cd` against tclsh.
//!
//! The path arithmetic needs nothing on disk and is compared directly. The
//! commands that read or change the filesystem run inside a tree this test
//! builds first, so both interpreters see exactly the same directory, and the
//! tree is rebuilt before each half so a program that deletes something cannot
//! change what the next one sees.
//!
//! Every program that names the scratch tree `cd`s into it, and the two runs
//! are separate processes, so neither can be affected by the other's working
//! directory.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Path arithmetic: no file has to exist for any of this.
const PATH_PROGRAMS: &[&str] = &[
    "foreach p {/a/b/c a/b/c /a a / {} . .. ./a a/ //a//b// ~ ~/x ~root/x a.b.c .hidden /a/.hidden a.tar.gz foo. ./foo.txt ~foo /a//b c:/x ///a //} {puts \"[file dirname $p]|[file tail $p]\"}",
    "foreach p {/a/b/c a/b/c /a a / {} . .. ./a a/ //a//b// a.b.c .hidden /a/.hidden a.tar.gz foo. ./foo.txt} {puts \"[file rootname $p]|[file extension $p]\"}",
    "foreach p {/a/b/c a/b/c /a a / {} . .. ./a a/ //a//b// ///a // /a//b c:/x} {puts [file split $p]}",
    "foreach p {/a/b/c a/b/c /a a / {} //a//b// ///a // /a//b} {puts \"[file nativename $p]|[file pathtype $p]\"}",
    "puts [file join a b c]",
    "puts [file join /a b]",
    "puts [file join a /b]",
    "puts [file join a b /c d]",
    "puts [file join {}]",
    "puts [file join a {} b]",
    "puts [file join ~ a]",
    "puts [file join a b/ c]",
    "puts [file join / a]",
    "puts [file join // a]",
    "puts [file join //a b]",
    "puts [file join /// a]",
    "puts [file separator]",
    "puts [catch {file join} m]\nputs $m",
    "puts [catch {file bogus x} m]\nputs $m",
    "puts [catch {file} m]\nputs $m",
    "puts [catch {file dirname} m]\nputs $m",
    "puts [catch {file dirname a b} m]\nputs $m",
    // `~` is expanded by exactly one command in Tcl 9.
    "puts [file tildeexpand a]",
    // Printed rather than compared against `$env(HOME)`: the two runs are on
    // one machine, so the home directory is the same for both, and the
    // environment array is a separate feature this does not need.
    "puts [file tildeexpand ~]",
    "puts [file tildeexpand ~/x]",
    "puts [catch {file tildeexpand ~nosuchuser99} m]\nputs $m",
    "puts [file home]",
    "puts [file dirname [file tildeexpand ~/x/y]]",
];

/// Programs run inside the scratch tree. `@` is replaced by its path.
const TREE_PROGRAMS: &[&str] = &[
    "cd @\nputs [lsort [glob *]]",
    "cd @\nputs [lsort [glob *.txt]]",
    "cd @\nputs [lsort [glob .*]]",
    "cd @\nputs [lsort [glob -nocomplain *.zzz]]",
    "cd @\nputs [lsort [glob *.zzz]]",
    "cd @\nputs [lsort [glob -types f *]]",
    "cd @\nputs [lsort [glob -types d *]]",
    "cd @\nputs [lsort [glob -types {f d} *]]",
    "cd @\nputs [lsort [glob -types l *]]",
    "cd @\nputs [lsort [glob -types x *]]",
    "cd @\nputs [lsort [glob -types {f r} *]]",
    "cd @\nputs [lsort [glob sub/*]]",
    "cd @\nputs [lsort [glob sub/deep/*]]",
    "cd @\nputs [lsort [glob */*]]",
    "cd @\nputs [lsort [glob {*.{txt,dat}}]]",
    "cd @\nputs [lsort [glob {a*,b*}]]",
    "cd @\nputs [lsort [glob {[ab].txt}]]",
    "cd @\nputs [lsort [glob {?.txt}]]",
    "cd @\nputs [lsort [glob a.txt b.txt]]",
    "cd @\nputs [lsort [glob nonexistent]]",
    "cd @\nputs [lsort [glob zzz*]]",
    "puts [lsort [glob -directory @ *.txt]]",
    "puts [lsort [glob -dir @/sub *]]",
    "puts [lsort [glob -tails -directory @ *.txt]]",
    "puts [lsort [glob -path @/a *]]",
    "puts [lsort [glob -join @ sub *]]",
    "puts [lsort [glob @/../[file tail @]/*.txt]]",
    // Queries.
    "cd @\nforeach p {a.txt sub link.txt nonexistent} {puts \"[file exists $p]|[file isdirectory $p]|[file isfile $p]\"}",
    "cd @\nforeach p {a.txt sub link.txt} {puts [file type $p]}",
    "cd @\nputs [file size a.txt]",
    "cd @\nputs [file size eight.txt]",
    "cd @\nputs \"[file readable a.txt]|[file writable a.txt]|[file executable a.txt]\"",
    "cd @\nputs \"[file readable sub]|[file executable sub]\"",
    "cd @\nputs [file owned a.txt]",
    "cd @\nputs [file readlink link.txt]",
    "cd @\nputs [catch {file readlink a.txt} m]\nputs $m",
    "cd @\nputs [expr {[file mtime a.txt] == [file mtime a.txt]}]",
    "cd @\nputs [expr {[file atime a.txt] > 0}]",
    "puts [catch {file size /nonexistent-xyz} m]\nputs $m",
    "puts [catch {file mtime /nonexistent-xyz} m]\nputs $m",
    "puts [catch {file atime /nonexistent-xyz} m]\nputs $m",
    "puts [catch {file type /nonexistent-xyz} m]\nputs $m",
    // `normalize` leaves a link in the last component alone and resolves the
    // rest, and `pwd` reports what `cd` was told rather than what `getcwd`
    // answers.
    "cd @\nputs [file normalize a.txt]",
    "cd @\nputs [file normalize .]",
    "cd @\nputs [file normalize ./sub/../a.txt]",
    "cd @\nputs [file normalize link.txt]",
    "cd @\nputs [file normalize link.txt/x]",
    "cd @\nputs [file normalize sub/deep/..]",
    "puts [file normalize {}]",
    "puts [file normalize /]",
    "puts [file normalize //]",
    "puts [file normalize ///a]",
    "puts [file normalize //a//b//]",
    "puts [file normalize /nonexistent-xyz/a/b]",
    "cd @\nputs [pwd]",
    "cd @/sub\nputs [pwd]",
    "cd @/sub\ncd ..\nputs [pwd]",
    "puts [catch {pwd x} m]\nputs $m",
    "puts [catch {cd /nonexistent-xyz} m]\nputs $m",
    // Creation and removal.
    "cd @\nfile mkdir n1 n2\nputs \"[file exists n1][file exists n2]\"\nfile delete n1 n2\nputs [file exists n1]",
    "cd @\nfile mkdir deep/er/still\nputs [file isdirectory deep/er/still]\nfile delete -force deep\nputs [file exists deep]",
    "cd @\nfile delete nonexistent-xyz\nputs done",
    "cd @\nfile copy a.txt cp.txt\nputs [file exists cp.txt]\nputs [catch {file copy a.txt cp.txt} m]\nputs $m\nfile rename cp.txt cp2.txt\nputs \"[file exists cp2.txt][file exists cp.txt]\"\nfile delete cp2.txt",
    "cd @\nfile mkdir dd/ee\nputs [catch {file delete dd} m]\nfile delete -force dd\nputs [file exists dd]",
    "cd @\nfile mkdir into\nfile copy a.txt b.txt into\nputs [lsort [glob -tails -directory into *]]\nfile delete -force into",
    // `file copy` builds no missing parent for its target, and `-force`
    // overwrites a file and never a directory. Both are measured: creating the
    // parents silently made one conformance case copy a whole home directory
    // into the runner's working directory before anything reported a problem.
    "cd @\nputs [catch {file copy sub no/such/place} m]\nputs $m\nputs [file exists no]",
    "cd @\nputs [catch {file copy a.txt no2/b.txt} m]\nputs $m\nputs [file exists no2]",
    "cd @\nputs [catch {file rename a.txt no3/b.txt} m]\nputs $m\nputs [file exists no3]",
    "cd @\nfile mkdir d/sub\nputs [catch {file copy sub d} m]\nputs $m\nputs [catch {file copy -force sub d} m]\nputs $m\nfile delete -force d",
    "cd @\nfile mkdir e\nfile copy sub e\nputs [lsort [glob e/sub/*]]\nputs [catch {file copy sub e} m]\nputs $m\nfile delete -force e",
    "cd @\nfile copy sub dest\nputs [lsort [glob dest/*]]\nfile copy sub dest\nputs [lsort [glob dest/*]]\nputs [catch {file copy a.txt dest} m]\nputs $m\nfile copy -force a.txt dest\nputs ok\nfile delete -force dest",
];

fn tclsh() -> Option<PathBuf> {
    for name in ["tclsh9.0", "tclsh", "tclsh8.6"] {
        let Ok(out) = Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {name}"))
            .output()
        else {
            continue;
        };
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if path.is_empty() {
            continue;
        }
        // Only the exact release this port is written against is an oracle.
        // tclrs targets 9.0.4 (`src/cmd_info.rs`'s `TCL_PATCHLEVEL`), and a
        // reference from any other release reports ITS version's differences
        // as tclrs failures: 8.6 words errors differently ("couldn't compile
        // regular expression" for "cannot compile") and has a different
        // ensemble membership, while 9.0.3 predates the lseq fixes (a zero
        // step yields the empty list where the manual says it yields `count`
        // elements, and a bareword argument is still an expr). The ubuntu CI
        // image ships 8.6, so CI skips these and they run against a matching
        // tclsh locally.
        let Ok(v) = Command::new("sh")
            .arg("-c")
            .arg(format!("printf 'puts [info patchlevel]\\n' | {path}"))
            .output()
        else {
            continue;
        };
        if String::from_utf8_lossy(&v.stdout).trim() == "9.0.4" {
            return Some(PathBuf::from(path));
        }
    }
    None
}

/// Distinct per call: the two tests in this file run at the same time.
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn reference_output(tclsh: &PathBuf, program: &str) -> String {
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("tclrs-file-{}-{serial}.tcl", std::process::id()));
    std::fs::write(&path, program).expect("write program");
    let out = Command::new(tclsh).arg(&path).output().expect("run tclsh");
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "tclsh rejected program:\n{program}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Build the tree the filesystem programs run against, from scratch.
fn build_tree(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root.join("sub/deep")).expect("create tree");
    for name in ["a.txt", "b.txt", "c.dat", ".hidden"] {
        std::fs::write(root.join(name), "").expect("create file");
    }
    std::fs::write(root.join("eight.txt"), "12345678").expect("create file");
    std::fs::write(root.join("sub/x.txt"), "").expect("create file");
    std::fs::write(root.join("sub/deep/y.txt"), "").expect("create file");
    let link = root.join("link.txt");
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink("a.txt", &link).expect("create link");
}

/// tclrs changes the process working directory, so each program is run from a
/// known one and the directory is put back afterwards.
fn run_here(program: &str, from: &Path) -> Result<String, String> {
    std::env::set_current_dir(from).expect("enter the starting directory");
    let outcome = tclrs::eval(program);
    std::env::set_current_dir(from).expect("return to the starting directory");
    outcome.map(|o| o.output).map_err(|e| e.to_string())
}

fn compare(tclsh: &PathBuf, programs: &[&str], root: Option<&Path>) {
    let start = std::env::temp_dir();
    let mut failures = Vec::new();
    for program in programs {
        let program = match root {
            Some(root) => program.replace('@', &root.to_string_lossy()),
            None => program.to_string(),
        };
        if let Some(root) = root {
            build_tree(root);
        }
        let expected = reference_output(tclsh, &program);
        if let Some(root) = root {
            build_tree(root);
        }
        match run_here(&program, &start) {
            Ok(output) if output == expected => {}
            Ok(output) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs: {output:?}"
            )),
            Err(e) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs failed: {e}"
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} programs diverge:\n\n{}",
        failures.len(),
        programs.len(),
        failures.join("\n\n")
    );
}

/// One test rather than two: `cd` changes the *process* working directory, so
/// two of these running at once would each be walking the other's ground.
#[test]
fn file_commands_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    compare(&tclsh, PATH_PROGRAMS, None);
    let root = std::env::temp_dir().join(format!("tclrs-file-tree-{}", std::process::id()));
    compare(&tclsh, TREE_PROGRAMS, Some(&root));
    let _ = std::fs::remove_dir_all(&root);
}
