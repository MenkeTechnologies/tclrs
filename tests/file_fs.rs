//! `file link`, `file attributes`, `file tempfile`, `file tempdir`,
//! `file volumes`, `file system`, `file channels` and the setting forms of
//! `file atime` and `file mtime`. Expectations are what tclsh 9.0.4 answered on
//! Unix. `file attributes` is queried by name: the full list differs by
//! platform (tclsh on macOS adds `-readonly`, `-hidden` and others).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

fn scratch() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tclrs-file-fs-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

/// Run `script` with `$d` set to a fresh directory that holds the file `f`, and
/// the working directory moved there. The working directory belongs to the whole
/// process, so the runs take turns.
fn run(script: &str) -> String {
    static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _turn = TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = scratch();
    std::fs::write(dir.join("f"), b"hi\n").expect("seed file");
    let source = format!("set d {{{}}}\ncd $d\n{script}", dir.display());
    let out = tclrs::eval(&source)
        .unwrap_or_else(|e| panic!("script failed: {e}\n{script}"))
        .output;
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn a_symbolic_link_is_made_read_and_refused_when_it_exists() {
    let out = run("puts [file link -symbolic l f]\n\
         puts [file link l]\n\
         puts [file type l]\n\
         puts [catch {file link -symbolic l f} m]\nputs $m\n\
         puts [catch {file link -symbolic m nosuch} m]\nputs $m\n\
         puts [catch {file link -bogus n f} m]\nputs $m\n\
         puts [catch {file link nosuch} m]\nputs $m");
    assert_eq!(
        out,
        "f\nf\nlink\n\
         1\ncould not create new link \"l\": that path already exists\n\
         1\ncould not create new link \"m\": target \"nosuch\" doesn't exist\n\
         1\nbad option \"-bogus\": must be -symbolic or -hard\n\
         1\ncould not read link \"nosuch\": no such file or directory\n"
    );
}

#[test]
fn a_hard_link_shares_the_file_and_cannot_be_read_back_as_a_link() {
    let out = run("file link -hard h f\n\
         puts [file size h]\n\
         puts [catch {file link h} m]\nputs $m\n\
         puts [catch {file link -hard i nosuch} m]\nputs $m");
    assert_eq!(
        out,
        "3\n1\ncould not read link \"h\": invalid argument\n\
         1\ncould not create new link \"i\": target \"nosuch\" doesn't exist\n"
    );
}

#[test]
fn permissions_are_read_and_set_in_every_spelling() {
    let out = run("foreach m {0600 0o640 rwxr-x--- u+x go-rwx} {\n\
           file attributes f -permissions $m\n\
           puts \"$m -> [file attributes f -permissions]\"\n\
         }\n\
         puts [catch {file attributes f -permissions bogus} m]\nputs $m\n\
         puts [file attributes f -perm]");
    assert_eq!(
        out,
        "0600 -> 0o600\n\
         0o640 -> 0o640\n\
         rwxr-x--- -> 0o750\n\
         u+x -> 0o750\n\
         go-rwx -> 0o700\n\
         1\nunknown permission string format \"bogus\"\n\
         0o700\n"
    );
}

#[test]
fn attribute_errors_name_the_option_or_the_file() {
    let out = run(
        "foreach cmd {{file attributes} {file attributes f -bogus} {file attributes f -permissions 0600 -owner} \
                      {file attributes nosuch} {file attributes nosuch -permissions} {file attributes nosuch -permissions 0600}} {\n\
           catch $cmd m o\n\
           puts \"$m | [dict get $o -errorcode]\"\n\
         }",
    );
    assert_eq!(
        out,
        "wrong # args: should be \"file attributes name ?-option value ...?\" | TCL WRONGARGS\n\
         bad option \"-bogus\": must be -group, -owner, or -permissions | TCL LOOKUP INDEX option -bogus\n\
         value for \"-owner\" missing | TCL OPERATION FATTR NOVALUE\n\
               could not read \"nosuch\": no such file or directory | POSIX ENOENT {no such file or directory}\n\
               could not read \"nosuch\": no such file or directory | POSIX ENOENT {no such file or directory}\n\
               could not set permissions for file \"nosuch\": no such file or directory | POSIX ENOENT {no such file or directory}\n"
    );
}

#[test]
fn times_are_set_and_read_back() {
    let out = run("puts [file mtime f 1000000000]\n\
         puts [file atime f 1100000000]\n\
         puts [list [file mtime f] [file atime f]]\n\
         puts [catch {file mtime f x} m]\nputs $m\n\
         puts [catch {file atime nosuch 5} m]\nputs $m");
    assert_eq!(
        out,
        "1000000000\n1100000000\n1000000000 1100000000\n\
         1\nexpected integer but got \"x\"\n\
         1\ncould not read \"nosuch\": no such file or directory\n"
    );
}

#[test]
fn a_temporary_directory_is_made_under_the_template() {
    let out = run(
        "set t [file tempdir $d/work_]\n\
         puts [expr {[file dirname $t] eq $d && [string match work_* [file tail $t]] && [file isdirectory $t]}]\n\
         puts [catch {file tempdir /nonexistent-dir-for-tclrs/} m]\nputs $m\n\
         puts [catch {file tempdir a b} m]\nputs $m",
    );
    assert_eq!(
        out,
        "1\n1\ncan't create temporary directory: no such file or directory\n\
         1\nwrong # args: should be \"file tempdir ?template?\"\n"
    );
}

#[test]
fn a_temporary_file_is_a_read_write_channel_that_outlives_its_name() {
    let out = run("set ch [file tempfile name $d/note_.txt]\n\
         puts [expr {[file dirname $name] eq $d && [string match note_* [file tail $name]] \
                     && [file extension $name] eq {.txt}}]\n\
         puts $ch\n\
         puts $ch hello\nflush $ch\n\
         puts [file size $name]\n\
         seek $ch 0\nputs [gets $ch]\n\
         close $ch\n\
         set gone [file tempfile]\n\
         puts $gone\nputs once\nclose $gone\n\
         puts [catch {file tempfile a b c} m]\nputs $m");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "1");
    assert!(lines[1].starts_with("file"), "{out}");
    assert_eq!(&lines[2..4], ["6", "hello"]);
    assert!(lines[4].starts_with("file"), "{out}");
    assert_eq!(lines[5], "once");
    assert_eq!(lines[6], "1");
    assert_eq!(
        lines[7],
        "wrong # args: should be \"file tempfile ?nameVar? ?template?\""
    );
}

#[test]
fn volumes_system_and_channels_answer_for_this_platform() {
    let out = run("puts [file volumes]\n\
         puts [file system $d]\n\
         puts [file system //zipfs:/x]\n\
         puts [catch {file system {}} m]\nputs $m\n\
         puts [lsort [file channels std*]]\n\
         puts [catch {file volumes x} m]\nputs $m");
    assert_eq!(
        out,
        "//zipfs:/ /\nnative\nzipfs zip\n\
         1\nunrecognised path\n\
         stderr stdin stdout\n\
         1\nwrong # args: should be \"file volumes\"\n"
    );
}
