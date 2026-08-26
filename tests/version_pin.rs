//! What `info patchlevel` / `info tclversion` answer, pinned here rather than
//! in a differential harness.
//!
//! The version is the release tclrs is WRITTEN AGAINST, not behaviour a
//! reference can be asked about: a `tclsh` from another 9.0.x answers with its
//! own patch number, and comparing the two reports a difference that says
//! nothing about the port. So the differential corpora leave it out and this
//! asserts it directly, against the constant the source documents itself with
//! (`src/cmd_info.rs`'s `TCL_PATCHLEVEL`).

#[test]
fn the_version_is_the_release_this_port_targets() {
    let out = tclrs::eval("puts [info patchlevel]\nputs [info tclversion]")
        .expect("the version commands run");
    assert_eq!(
        out.output, "9.0.4\n9.0\n",
        "tclrs reports the Tcl it ports; update this pin (and the port) together"
    );
}
