//! Compile the C trampoline that reads the variadic stub slots.
//!
//! Only when the `tk` feature is on. A default build runs this script, finds
//! `CARGO_FEATURE_TK` unset, and returns without invoking a C compiler at all —
//! so a machine with no `cc` on its PATH builds tclrs exactly as it did before
//! this file existed.
//!
//! Why a C file exists in a Rust tree: stable rustc rejects the *definition* of
//! a C-variadic function (`error[E0658]`, tracking issue 44930), and on AAPCS64
//! a non-variadic declaration cannot reach the arguments either, because they
//! are on the stack rather than in the registers a fixed parameter of the same
//! position would occupy. `src/tk/trampoline.c` is the only place in the tree
//! that may write `va_arg`; see its header comment for which slots need it.

fn main() {
    println!("cargo:rerun-if-changed=src/tk/trampoline.c");
    println!("cargo:rerun-if-changed=build.rs");

    // Cargo compiles a build script with the package's own feature cfgs, so
    // this arm does not exist at all in a default build — which is what lets
    // `cc` be an optional build-dependency that is never even downloaded.
    #[cfg(feature = "tk")]
    cc::Build::new()
        .file("src/tk/trampoline.c")
        .warnings(true)
        .compile("tclrs_tk_trampoline");
}
