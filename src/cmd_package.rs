//! `package`, ported from `generic/tclPkg.c`.
//!
//! # Why the whole command and not just `require`
//!
//! A package is not a file this frontend loads; it is a name with a version
//! attached, and everything else in Tcl's package system is arithmetic on that
//! pair. `package require Tk` is the one call anyone writes, but it cannot be
//! answered without `provide` (what a loaded package says about itself),
//! `ifneeded` (the script that loads one), the version normaliser and the
//! version comparator — `Tk_Init`'s last acts are two `Tcl_PkgProvideEx` calls
//! (`tk9.0.4/generic/tkWindow.c:3461-3469`), and whether they agree is decided
//! by [`compare_versions`], not by `==` on two strings.
//!
//! So the whole of `Tcl_PackageObjCmd` (`generic/tclPkg.c:1060-1530`) is here,
//! with three departures, each named where it happens:
//!
//! * `package files` always answers the empty string. Tcl fills its answer from
//!   the `tclPkgFiles` association, which is written by `source` while a
//!   `pkgIndex.tcl` runs (`generic/tclPkg.c:273-297`); this frontend has no
//!   such index, so nothing has ever been recorded and the empty answer is the
//!   true one rather than a stub.
//! * `package names` lists in the order packages were first mentioned. Tcl
//!   iterates a hash table (`generic/tclPkg.c:1243-1250`), so its order is
//!   unspecified; a deterministic one cannot be wrong and is testable.
//! * After the `package unknown` script has run and the package is still
//!   missing, one more source is consulted: the toolkit loaders in
//!   [`load_native`]. That is where `package require Tk` reaches
//!   [`crate::tk::session`], which `dlopen`s libtk and calls `Tk_Init`. Tcl
//!   reaches the same place through `pkgIndex.tcl` and `load`; this frontend
//!   has neither, and the hook is the smallest thing that does not pretend to.
//!
//! # One registry, two doors
//!
//! [`Tcl_PkgProvideEx`](crate::tk::pkg::pkg_provide_ex) — the stub slot Tk
//! itself calls — and the `package provide` a script runs are the same
//! operation on the same table, and it is the table below. Tcl keeps one per
//! interpreter (`iPtr->packageTable`, `generic/tclPkg.c:113-116`); this crate
//! keeps one per process, because the host has one live interpreter at a time
//! plus the short-lived second one Tk makes for its option database
//! (`tk9.0.4/generic/tkOption.c:1496-1499`), which is never provided into.
//!
//! # Version arithmetic
//!
//! [`check_version_and_convert`] and [`compare_versions`] are TIP 268's rules,
//! ported from `generic/tclPkg.c:1654-1929`. They are here rather than in
//! `src/tk/` because the `tk` feature is off by default and `package vcompare`
//! is not: one implementation, reachable from both doors.

use std::sync::Mutex;

use fusevm::{Op, Value, VM};

use crate::compiler::{ext, CompileError, Compiler};
use crate::parser::Word;
use crate::runtime::{to_tcl_string, TclError};

/// The subcommands, in `pkgOptions`' order (`generic/tclPkg.c:1067-1071`).
///
/// The order is the message's: `bad option "x": must be files, forget, …`
/// lists them exactly as they appear here.
pub const SUBCOMMANDS: &[&str] = &[
    "files",
    "forget",
    "ifneeded",
    "names",
    "prefer",
    "present",
    "provide",
    "require",
    "unknown",
    "vcompare",
    "versions",
    "vsatisfies",
];

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

/// One entry of `pkgPtr->availPtr` (`generic/tclPkg.c:31-38`): a version this
/// package could be loaded at, and the script that loads it.
struct Avail {
    version: String,
    script: String,
}

/// One `Package` (`generic/tclPkg.c:60-65`).
struct Package {
    name: String,
    /// `pkgPtr->version`: what a `provide` recorded, or `None` for a package
    /// only ever mentioned by `ifneeded`.
    version: Option<String>,
    avail: Vec<Avail>,
    /// `pkgPtr->clientData` as `Tcl_PkgProvideEx` sets it — `&tkStubs` for
    /// `tk` (`tk9.0.4/generic/tkWindow.c:3466`). Held as an address so this
    /// module carries no raw pointer and stays `Send` in a build with no `tk`
    /// feature at all.
    client_data: usize,
    /// The version whose `ifneeded` script is running right now.
    ///
    /// The C reuses `clientData` for this (`generic/tclPkg.c:819`), which works
    /// there because the two are never both meaningful: a package with a
    /// version has nothing being selected for it, and `Tcl_PkgProvideEx`
    /// overwrites the marker with the real client data the moment the script
    /// provides. Splitting them is the same behaviour with one fewer thing to
    /// reason about, and costs a clear at the end of [`select_package`] that
    /// the C gets for free.
    providing: Option<String>,
}

/// `latest` and `stable`, in `pkgPreferOptions`' order
/// (`generic/tclPkg.c:1418-1420`). The order is load-bearing: `package
/// prefer` only ever lowers the setting (`generic/tclPkg.c:1441-1443`), and
/// "lower" means towards `latest`.
const PREFER: [&str; 2] = ["latest", "stable"];
const PREFER_STABLE: usize = 1;

struct Registry {
    packages: Vec<Package>,
    /// `iPtr->packageUnknown`.
    unknown: Option<String>,
    /// `iPtr->packagePrefer`. `stable` is where an interpreter starts —
    /// measured: `tclsh9.0 -c 'puts [package prefer]'` prints `stable`.
    prefer: usize,
}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    packages: Vec::new(),
    unknown: None,
    prefer: PREFER_STABLE,
});

fn registry() -> std::sync::MutexGuard<'static, Registry> {
    REGISTRY.lock().expect("package registry poisoned")
}

impl Registry {
    /// `FindPackage` (`generic/tclPkg.c:1561-1594`): create on first mention.
    fn find_or_create(&mut self, name: &str) -> &mut Package {
        if let Some(i) = self.packages.iter().position(|p| p.name == name) {
            return &mut self.packages[i];
        }
        self.packages.push(Package {
            name: name.to_string(),
            version: None,
            avail: Vec::new(),
            client_data: 0,
            providing: None,
        });
        self.packages.last_mut().expect("just pushed")
    }

    fn find(&self, name: &str) -> Option<&Package> {
        self.packages.iter().find(|p| p.name == name)
    }
}

/// The version a package was provided at, or `None`.
pub fn provided_version(name: &str) -> Option<String> {
    registry().find(name).and_then(|p| p.version.clone())
}

/// Every provided package as `(name, version)`, in the order it was provided.
pub fn provided() -> Vec<(String, String)> {
    registry()
        .packages
        .iter()
        .filter_map(|p| p.version.clone().map(|v| (p.name.clone(), v)))
        .collect()
}

/// The client data a package was provided with, as an address; 0 when there is
/// none. `&tkStubs` for `tk` (`tk9.0.4/generic/tkWindow.c:3466`).
pub fn client_data(name: &str) -> usize {
    registry().find(name).map(|p| p.client_data).unwrap_or(0)
}

/// `Tcl_PkgProvideEx` (`generic/tclPkg.c:154-197`), with the interpreter result
/// as a `Result` rather than an out-parameter.
///
/// `client_data` is an address, and 0 is the C's NULL: a second provide at the
/// same version adopts a non-NULL one and leaves an existing one alone
/// (`:186-191`).
pub fn provide(name: &str, version: &str, client_data: usize) -> Result<(), String> {
    let previous = registry().find(name).and_then(|p| p.version.clone());
    let Some(previous) = previous else {
        // `pkgPtr->version == NULL` (`generic/tclPkg.c:167-172`).
        let mut reg = registry();
        let pkg = reg.find_or_create(name);
        pkg.version = Some(version.to_string());
        pkg.client_data = client_data;
        return Ok(());
    };

    // `generic/tclPkg.c:174-180`.
    let pvi = check_version_and_convert(&previous).ok_or_else(|| bad_version(&previous))?;
    let vi = check_version_and_convert(version).ok_or_else(|| bad_version(version))?;

    if compare_versions(&pvi, &vi).0 == 0 {
        if client_data != 0 {
            registry().find_or_create(name).client_data = client_data;
        }
        return Ok(());
    }

    // `generic/tclPkg.c:192-196`.
    Err(format!(
        "conflicting versions provided for package \"{name}\": {previous}, then {version}"
    ))
}

/// The `error:` label of `CheckVersionAndConvert` (`generic/tclPkg.c:1743-1747`).
fn bad_version(text: &str) -> String {
    format!("expected version number but got \"{text}\"")
}

// ---------------------------------------------------------------------------
// Version arithmetic
// ---------------------------------------------------------------------------

/// The version ordering, for a caller with two version strings and no
/// interpreter: `None` when either is not a version number at all.
pub fn compare(a: &str, b: &str) -> Option<i32> {
    let (a, b) = (check_version_and_convert(a)?, check_version_and_convert(b)?);
    Some(compare_versions(&a, &b).0)
}

/// `CheckVersionAndConvert` (`generic/tclPkg.c:1654-1747`): validate, and
/// return the normalised internal representation.
///
/// The rules are TIP 268's, quoted at `generic/tclPkg.c:1676-1687`: the first
/// character is a digit; every other is a digit, `.`, `a` or `b`; at most one
/// of `a`/`b`; and neither may sit next to a `.`. The conversion turns `.`
/// into ` 0 `, `a` into ` -2 ` and `b` into ` -1 `, so ordering falls out of a
/// plain comparison of the pieces. Everything from a `+` onward is ignored
/// (`generic/tclPkg.c:1692`), which is why `1.0+abc` provides as `1.0+abc` and
/// compares as `1.0`.
///
/// `None` is the C's `TCL_ERROR`.
pub fn check_version_and_convert(version: &str) -> Option<String> {
    convert(version).map(|(internal, _)| internal)
}

/// The same, keeping the `stable` out-parameter: false once an `a` or `b` has
/// been seen (`generic/tclPkg.c:1733-1735`). `package require` needs it to
/// prefer a stable version over a newer alpha.
fn convert(version: &str) -> Option<(String, bool)> {
    let b = version.as_bytes();
    // Rule 1 (`generic/tclPkg.c:1689-1691`).
    if b.first().is_none_or(|c| !c.is_ascii_digit()) {
        return None;
    }
    let mut out = String::new();
    out.push(b[0] as char);
    let mut has_unstable = false;
    let mut prev = b[0];
    for &c in &b[1..] {
        if c == b'+' {
            break;
        }
        // Rules 2, 4 and 5 (`generic/tclPkg.c:1696-1703`), in the C's own
        // order so the reading is checkable against it.
        let structural = c == b'.' || c == b'a' || c == b'b';
        if !c.is_ascii_digit()
            && (!structural
                || (has_unstable && (c == b'a' || c == b'b'))
                || ((prev == b'a' || prev == b'b' || prev == b'.') && c == b'.')
                || (structural && prev == b'.'))
        {
            return None;
        }
        if c == b'a' || c == b'b' {
            has_unstable = true;
        }
        match c {
            b'.' => out.push_str(" 0 "),
            b'a' => out.push_str(" -2 "),
            b'b' => out.push_str(" -1 "),
            _ => out.push(c as char),
        }
        prev = c;
    }
    // A version may not end on a separator (`generic/tclPkg.c:1729`).
    if prev == b'.' || prev == b'a' || prev == b'b' {
        return None;
    }
    Some((out, !has_unstable))
}

/// `CompareVersions` (`generic/tclPkg.c:1777-1929`).
///
/// Returns `(ordering, is_major)`: -1/0/1 as the C does, and whether the
/// difference was in the first segment, which is the C's `isMajorPtr`.
///
/// The comparison is deliberately not numeric. Leading zeros are skipped and
/// then a *shorter* run of digits is the smaller number, with `strcmp` only
/// between runs of equal length (`generic/tclPkg.c:1871-1887`) — which is what
/// makes a version component wider than 64 bits compare correctly.
pub fn compare_versions(v1: &str, v2: &str) -> (i32, bool) {
    let a = v1.as_bytes();
    let b = v2.as_bytes();
    let (mut s1, mut s2) = (0usize, 0usize);
    let mut this_is_major = true;
    let res;
    loop {
        while s1 < a.len() && a[s1] == b'0' {
            s1 += 1;
        }
        while s2 < b.len() && b[s2] == b'0' {
            s2 += 1;
        }
        let neg1 = a.get(s1) == Some(&b'-');
        let neg2 = b.get(s2) == Some(&b'-');
        // Signs first, as a shortcut (`generic/tclPkg.c:1830-1839`).
        if neg1 && !neg2 {
            res = -1;
            break;
        }
        if !neg1 && neg2 {
            res = 1;
            break;
        }
        let flip = neg1 && neg2;
        if flip {
            s1 += 1;
            s2 += 1;
        }

        let e1 = s1
            + a[s1..]
                .iter()
                .position(|c| *c == b' ')
                .unwrap_or(a.len() - s1);
        let e2 = s2
            + b[s2..]
                .iter()
                .position(|c| *c == b' ')
                .unwrap_or(b.len() - s2);

        let mut r: i32 = if e1 - s1 < e2 - s2 {
            -1
        } else if e2 - s2 < e1 - s1 {
            1
        } else {
            match a[s1..e1].cmp(&b[s2..e2]) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            }
        };
        if r != 0 {
            if flip {
                r = -r;
            }
            res = r;
            break;
        }

        // `generic/tclPkg.c:1906-1921`: advance past the separator, and stop
        // when both sides are exhausted at the same time.
        s1 = e1;
        s2 = e2;
        if s1 < a.len() {
            s1 += 1;
        } else if s2 >= b.len() {
            res = 0;
            break;
        }
        if s2 < b.len() {
            s2 += 1;
        }
        this_is_major = false;
    }
    (res, this_is_major)
}

/// `CheckRequirement` (`generic/tclPkg.c:1985-2039`): `version`,
/// `version-version` or `version-`.
fn check_requirement(req: &str) -> Result<(), String> {
    let dash = match req.find('+') {
        Some(_) => None,
        None => req.find('-'),
    };
    let Some(dash) = dash else {
        return match check_version_and_convert(req) {
            Some(_) => Ok(()),
            None => Err(bad_version(req)),
        };
    };
    let (min, max) = (&req[..dash], &req[dash + 1..]);
    if max.contains('-') {
        return Err(format!("expected versionMin-versionMax but got \"{req}\""));
    }
    if check_version_and_convert(min).is_none() {
        return Err(bad_version(min));
    }
    if !max.is_empty() && check_version_and_convert(max).is_none() {
        return Err(bad_version(max));
    }
    Ok(())
}

/// `CheckAllRequirements` (`generic/tclPkg.c:1951-1965`).
fn check_all_requirements(reqs: &[String]) -> Result<(), String> {
    reqs.iter().try_for_each(|r| check_requirement(r))
}

/// `RequirementSatisfied` (`generic/tclPkg.c:2173-2254`), given the candidate's
/// already-converted internal representation.
///
/// The ` -2` the C appends with `strcat` is the internal form of `a0`: padding
/// the requirement with it is what makes `1.2a1` satisfy a requirement of
/// `1.2`, because an alpha sorts below the release it precedes.
fn requirement_satisfied(havei: &str, req: &str) -> bool {
    let Some(dash) = req.find('-') else {
        let Some(mut reqi) = check_version_and_convert(req) else {
            return false;
        };
        reqi.push_str(" -2");
        let (res, this_is_major) = compare_versions(havei, &reqi);
        return res == 0 || (res == 1 && !this_is_major);
    };
    let (min, max) = (&req[..dash], &req[dash + 1..]);
    let Some(mut mini) = check_version_and_convert(min) else {
        return false;
    };
    if max.is_empty() {
        // A min with no max (`generic/tclPkg.c:2218-2230`).
        mini.push_str(" -2");
        return compare_versions(havei, &mini).0 >= 0;
    }
    let Some(mut maxi) = check_version_and_convert(max) else {
        return false;
    };
    // Identical bounds are compared as they stand; otherwise both are padded,
    // and the range is half-open (`generic/tclPkg.c:2241-2248`).
    if compare_versions(&mini, &maxi).0 == 0 {
        return compare_versions(&mini, havei).0 == 0;
    }
    mini.push_str(" -2");
    maxi.push_str(" -2");
    compare_versions(&mini, havei).0 <= 0 && compare_versions(havei, &maxi).0 < 0
}

/// `SomeRequirementSatisfied` (`generic/tclPkg.c:2136-2153`). No requirements
/// at all is the caller's business: every caller tests `reqs.is_empty()` first,
/// as the C tests `reqc != 0`.
fn some_requirement_satisfied(havei: &str, reqs: &[String]) -> bool {
    reqs.iter().any(|r| requirement_satisfied(havei, r))
}

/// `AddRequirementsToResult` (`generic/tclPkg.c:2057-2079`).
///
/// A requirement built by `-exact` is `V-V`, and is reported as `exactly V`.
/// The C recognizes it arithmetically — odd length, a dash in the middle, the
/// two halves equal — rather than by remembering how it was spelled.
fn add_requirements_to_result(out: &mut String, reqs: &[String]) {
    for r in reqs {
        let n = r.len();
        // `length/2` and `(length+1)/2` are the C's own two indices: the dash
        // and the character after it, which coincide only when the length is
        // odd — which the first test already requires.
        let (dash, after) = (n / 2, n.div_ceil(2));
        let exact = n % 2 == 1 && r.as_bytes()[dash] == b'-' && r[..dash] == r[after..];
        match exact {
            true => out.push_str(&format!(" exactly {}", &r[after..])),
            false => out.push_str(&format!(" {r}")),
        }
    }
}

/// `AddRequirementsToDString` (`generic/tclPkg.c:2097-2115`): the requirements
/// as they were written, or ` 0-` when there are none.
fn add_requirements_to_dstring(out: &mut String, reqs: &[String]) {
    if reqs.is_empty() {
        out.push_str(" 0-");
        return;
    }
    for r in reqs {
        out.push(' ');
        out.push_str(r);
    }
}

// ---------------------------------------------------------------------------
// `package require`
// ---------------------------------------------------------------------------

/// How this module runs a script: the `ifneeded` script that loads a package,
/// and the `package unknown` script that finds one.
///
/// A trait rather than a closure because the implementation
/// ([`crate::runtime`]'s) has to reach both the interpreter and the running
/// VM's globals, and hands them back afterwards — a nested evaluation writes
/// variables the outer chunk is holding in slots.
pub trait ScriptHost {
    /// Evaluate `src` in the interpreter this command is running in, at the
    /// global level, and give back its value.
    fn eval(&mut self, src: &str) -> Result<String, TclError>;
}

/// `PkgRequireCore` and the four steps after it
/// (`generic/tclPkg.c:458-639`), flattened: the C threads them through
/// `Tcl_NRAddCallback` so a `package require` inside a coroutine can yield, and
/// this frontend's `eval` is an ordinary nested run.
fn pkg_require(name: &str, reqs: &[String], host: &mut dyn ScriptHost) -> Result<String, TclError> {
    check_all_requirements(reqs).map_err(TclError::plain)?;
    registry().find_or_create(name);

    if provided_version(name).is_none() {
        // `PkgRequireCore` (`:478-481`): try the `ifneeded` scripts already
        // registered.
        select_package(name, reqs, host)?;
    }
    if provided_version(name).is_none() {
        // `PkgRequireCoreStep1` (`:513-541`): ask `package unknown`.
        let script = registry().unknown.clone();
        if let Some(script) = script {
            let mut command = script;
            command.push(' ');
            command.push_str(&crate::list::quote(name, false));
            add_requirements_to_dstring(&mut command, reqs);
            // `PkgRequireCoreStep2` (`:561-566`): an error from the script is
            // the answer; success discards whatever it left as a result.
            host.eval(&command)?;
            if provided_version(name).is_none() {
                select_package(name, reqs, host)?;
            }
        }
    }
    if provided_version(name).is_none() {
        // The departure named in this module's documentation: a toolkit this
        // binary can load itself, where Tcl would have found a `pkgIndex.tcl`.
        load_native(name).map_err(TclError::plain)?;
    }

    // `PkgRequireCoreFinal` (`:579-629`).
    let Some(have) = provided_version(name) else {
        let mut msg = format!("can't find package {name}");
        add_requirements_to_result(&mut msg, reqs);
        return Err(TclError::plain(msg));
    };
    if !reqs.is_empty() {
        // The provided version passed `CheckVersionAndConvert` when it was
        // provided, so it converts here too.
        let havei =
            check_version_and_convert(&have).ok_or_else(|| TclError::plain(bad_version(&have)))?;
        if !some_requirement_satisfied(&havei, reqs) {
            let mut msg = format!("version conflict for package \"{name}\": have {have}, need");
            add_requirements_to_result(&mut msg, reqs);
            return Err(TclError::plain(msg));
        }
    }
    Ok(have)
}

/// `SelectPackage` and `SelectPackageFinal` (`generic/tclPkg.c:641-945`):
/// choose the best available version that meets the requirements, run its
/// script, and check that the script provided what it promised.
///
/// Leaves the package unprovided and returns `Ok` when there is nothing to
/// choose from — that is not a failure, it is what sends `pkg_require` on to
/// `package unknown`.
fn select_package(name: &str, reqs: &[String], host: &mut dyn ScriptHost) -> Result<(), TclError> {
    // Circular dependency (`:663-671`).
    if let Some(providing) = registry().find(name).and_then(|p| p.providing.clone()) {
        let mut msg = format!(
            "circular package dependency: attempt to provide {name} {providing} requires {name}"
        );
        add_requirements_to_result(&mut msg, reqs);
        return Err(TclError::plain(msg));
    }

    let best = {
        let reg = registry();
        let Some(pkg) = reg.find(name) else {
            return Ok(());
        };
        best_available(pkg, reqs, reg.prefer)
    };
    let Some((version, script)) = best else {
        return Ok(());
    };

    registry().find_or_create(name).providing = Some(version.clone());
    let outcome = host.eval(&script);
    // `SelectPackageFinal` (`:844-945`). The marker goes whatever happened: the
    // C lets `Tcl_PkgProvideEx` overwrite it on the way through and clears it
    // by hand on failure, which comes to the same thing.
    registry().find_or_create(name).providing = None;

    if let Err(e) = outcome {
        // `:923-939`: a script that failed did not load the package, so any
        // version it managed to provide is forgotten rather than remembered.
        registry().find_or_create(name).version = None;
        return Err(e);
    }
    match provided_version(name) {
        // `:868-875`.
        None => Err(TclError::plain(format!(
            "attempt to provide package {name} {version} failed: \
             no version of package {name} provided"
        ))),
        // `:876-901`: it provided *something*; it has to be the same version.
        Some(got) => match compare(&got, &version) {
            Some(0) => Ok(()),
            _ => {
                registry().find_or_create(name).version = None;
                Err(TclError::plain(format!(
                    "attempt to provide package {name} {version} failed: \
                     package {name} {got} provided instead"
                )))
            }
        },
    }
}

/// The best and best-stable candidates, resolved by the preference
/// (`generic/tclPkg.c:680-801`).
fn best_available(pkg: &Package, reqs: &[String], prefer: usize) -> Option<(String, String)> {
    let mut best: Option<(&Avail, String)> = None;
    let mut best_stable: Option<(&Avail, String)> = None;
    for avail in &pkg.avail {
        // A version that does not convert cannot happen — `package ifneeded`
        // refuses one — and the C skips it rather than failing (`:687-696`).
        let Some((availi, stable)) = convert(&avail.version) else {
            continue;
        };
        if !reqs.is_empty() && !some_requirement_satisfied(&availi, reqs) {
            continue;
        }
        if best
            .as_ref()
            .is_none_or(|(_, b)| compare_versions(&availi, b).0 > 0)
        {
            best = Some((avail, availi.clone()));
        }
        if stable
            && best_stable
                .as_ref()
                .is_none_or(|(_, b)| compare_versions(&availi, b).0 > 0)
        {
            best_stable = Some((avail, availi));
        }
    }
    // `:798-801`: `stable` takes the best stable when there is one.
    let chosen = match (prefer == PREFER_STABLE, &best_stable) {
        (true, Some(_)) => best_stable,
        _ => best,
    };
    chosen.map(|(a, _)| (a.version.clone(), a.script.clone()))
}

/// Load a package this binary can produce without a script.
///
/// `Tk` and `tk` are the whole list, and only in a build with the `tk` feature
/// and a session started by `tclrs --tk`: see [`crate::tk::session::load_tk`]
/// for what "load" means and why the main thread is a precondition. Every other
/// name is left unprovided, which is what turns into `can't find package X` —
/// the same answer `tclsh` gives for a package with no `pkgIndex.tcl`.
fn load_native(name: &str) -> Result<(), String> {
    #[cfg(feature = "tk")]
    if name == "Tk" || name == "tk" {
        return crate::tk::session::load_tk();
    }
    let _ = name;
    Ok(())
}

// ---------------------------------------------------------------------------
// The command
// ---------------------------------------------------------------------------

/// Resolve a possibly abbreviated subcommand, as `Tcl_GetIndexFromObj` does
/// (`generic/tclIndexObj.c:242-296`), with the wording `package` asks for:
/// the message names the argument `option` (`generic/tclPkg.c:1095`).
fn resolve(word: &str) -> Result<&'static str, String> {
    if let Some(exact) = SUBCOMMANDS.iter().copied().find(|s| *s == word) {
        return Ok(exact);
    }
    let mut hits = SUBCOMMANDS.iter().copied().filter(|s| s.starts_with(word));
    match (hits.next(), hits.next()) {
        (Some(only), None) if !word.is_empty() => Ok(only),
        _ => Err(format!(
            "bad option \"{word}\": must be {}, or {}",
            SUBCOMMANDS[..SUBCOMMANDS.len() - 1].join(", "),
            SUBCOMMANDS[SUBCOMMANDS.len() - 1]
        )),
    }
}

/// `Tcl_WrongNumArgs(interp, 2, objv, usage)` (`generic/tclIndexObj.c`), which
/// prints the command and the subcommand and then the usage.
fn wrong_args(sub: &str, usage: &str) -> String {
    match usage.is_empty() {
        true => format!("wrong # args: should be \"package {sub}\""),
        false => format!("wrong # args: should be \"package {sub} {usage}\""),
    }
}

/// The whole of `TclNRPackageObjCmd` (`generic/tclPkg.c:1060-1530`).
///
/// `argv[0]` is the command name, as an `objv` is.
pub fn run(argv: &[String], host: &mut dyn ScriptHost) -> Result<String, TclError> {
    if argv.len() < 2 {
        return Err(TclError::plain(
            "wrong # args: should be \"package option ?arg ...?\"".to_string(),
        ));
    }
    let sub = resolve(&argv[1]).map_err(TclError::plain)?;
    let args = &argv[2..];
    match sub {
        // `PKG_FILES` (`:1100-1116`). Nothing records a package's files here;
        // see this module's documentation.
        "files" => match args.len() {
            1 => Ok(String::new()),
            _ => Err(TclError::plain(wrong_args("files", "package"))),
        },
        // `PKG_FORGET` (`:1117-1155`): a name that is not known is not an
        // error, it is nothing to do.
        "forget" => {
            let mut reg = registry();
            reg.packages.retain(|p| !args.contains(&p.name));
            Ok(String::new())
        }
        "ifneeded" => ifneeded(args),
        // `PKG_NAMES` (`:1234-1253`).
        "names" => match args.is_empty() {
            true => Ok(crate::list::join(
                &registry()
                    .packages
                    .iter()
                    .filter(|p| p.version.is_some() || !p.avail.is_empty())
                    .map(|p| p.name.clone())
                    .collect::<Vec<_>>(),
            )),
            false => Err(TclError::plain(wrong_args("names", ""))),
        },
        "prefer" => prefer(args),
        "present" => present(args, host),
        "provide" => provide_cmd(args),
        "require" => require_cmd("require", args, host),
        "unknown" => unknown(args),
        // `PKG_VCOMPARE` (`:1454-1482`).
        "vcompare" => match args.len() {
            2 => {
                let a = check_version_and_convert(&args[0])
                    .ok_or_else(|| TclError::plain(bad_version(&args[0])))?;
                let b = check_version_and_convert(&args[1])
                    .ok_or_else(|| TclError::plain(bad_version(&args[1])))?;
                Ok(compare_versions(&a, &b).0.to_string())
            }
            _ => Err(TclError::plain(wrong_args("vcompare", "version1 version2"))),
        },
        // `PKG_VERSIONS` (`:1483-1503`).
        "versions" => match args.len() {
            1 => {
                let versions: Vec<String> = registry()
                    .find(&args[0])
                    .map(|p| p.avail.iter().map(|a| a.version.clone()).collect())
                    .unwrap_or_default();
                Ok(crate::list::join(&versions))
            }
            _ => Err(TclError::plain(wrong_args("versions", "package"))),
        },
        // `PKG_VSATISFIES` (`:1504-1525`).
        "vsatisfies" => match args.len() {
            0 | 1 => Err(TclError::plain(wrong_args(
                "vsatisfies",
                "version ?requirement ...?",
            ))),
            _ => {
                let have = check_version_and_convert(&args[0])
                    .ok_or_else(|| TclError::plain(bad_version(&args[0])))?;
                check_all_requirements(&args[1..]).map_err(TclError::plain)?;
                Ok(u8::from(some_requirement_satisfied(&have, &args[1..])).to_string())
            }
        },
        _ => unreachable!("resolve answers only with a name from SUBCOMMANDS"),
    }
}

/// `PKG_IFNEEDED` (`generic/tclPkg.c:1156-1233`).
fn ifneeded(args: &[String]) -> Result<String, TclError> {
    if args.len() != 2 && args.len() != 3 {
        return Err(TclError::plain(wrong_args(
            "ifneeded",
            "package version ?script?",
        )));
    }
    let (name, version) = (&args[0], &args[1]);
    let wanted =
        check_version_and_convert(version).ok_or_else(|| TclError::plain(bad_version(version)))?;

    let mut reg = registry();
    if args.len() == 2 {
        // A query: the script registered for exactly this version, or nothing.
        let Some(pkg) = reg.find(name) else {
            return Ok(String::new());
        };
        let found = pkg.avail.iter().find(|a| {
            check_version_and_convert(&a.version)
                .is_some_and(|avi| compare_versions(&avi, &wanted).0 == 0)
        });
        return Ok(found.map(|a| a.script.clone()).unwrap_or_default());
    }

    let pkg = reg.find_or_create(name);
    let existing = pkg.avail.iter().position(|a| {
        check_version_and_convert(&a.version)
            .is_some_and(|avi| compare_versions(&avi, &wanted).0 == 0)
    });
    match existing {
        Some(i) => pkg.avail[i].script = args[2].clone(),
        None => pkg.avail.push(Avail {
            version: version.clone(),
            script: args[2].clone(),
        }),
    }
    Ok(String::new())
}

/// `PKG_PREFER` (`generic/tclPkg.c:1417-1453`). The setting only ever moves
/// towards `latest`, and the current value is always the answer.
fn prefer(args: &[String]) -> Result<String, TclError> {
    if args.len() > 1 {
        return Err(TclError::plain(wrong_args("prefer", "?latest|stable?")));
    }
    let mut reg = registry();
    if let Some(word) = args.first() {
        let new = PREFER.iter().position(|p| *p == word).ok_or_else(|| {
            TclError::plain(format!(
                "bad preference \"{word}\": must be latest or stable"
            ))
        })?;
        if new < reg.prefer {
            reg.prefer = new;
        }
    }
    Ok(PREFER[reg.prefer].to_string())
}

/// `PKG_PROVIDE` (`generic/tclPkg.c:1300-1320`).
fn provide_cmd(args: &[String]) -> Result<String, TclError> {
    match args.len() {
        1 => Ok(provided_version(&args[0]).unwrap_or_default()),
        2 => {
            let (name, version) = (&args[0], &args[1]);
            check_version_and_convert(version)
                .ok_or_else(|| TclError::plain(bad_version(version)))?;
            provide(name, version, 0)
                .map(|()| String::new())
                .map_err(TclError::plain)
        }
        _ => Err(TclError::plain(wrong_args("provide", "package ?version?"))),
    }
}

/// `PKG_PRESENT` (`generic/tclPkg.c:1254-1298`), which falls through to
/// `require` for a package that is already provided and otherwise reports that
/// it is not.
fn present(args: &[String], host: &mut dyn ScriptHost) -> Result<String, TclError> {
    if args.is_empty() {
        return require_cmd("present", args, host);
    }
    let exact = args[0] == "-exact";
    if exact && args.len() != 3 {
        return Err(TclError::plain(wrong_args(
            "present",
            "?-exact? package ?requirement ...?",
        )));
    }
    let name = match exact {
        true => &args[1],
        false => &args[0],
    };
    if provided_version(name).is_some() {
        return require_cmd("present", args, host);
    }

    // Not provided: `Tcl_PkgPresentEx` (`:1023-1031`). The version named in the
    // message is the `-exact` one, or the first requirement when it is a plain
    // version number — the name is `objv[2]`, so the requirements start at
    // `objv[3]`, which is `args[1..]` here (`:1288-1294`).
    let version = match exact {
        true => {
            check_version_and_convert(&args[2])
                .ok_or_else(|| TclError::plain(bad_version(&args[2])))?;
            Some(args[2].clone())
        }
        false => {
            check_all_requirements(&args[1..]).map_err(TclError::plain)?;
            args.get(1)
                .filter(|v| check_version_and_convert(v).is_some())
                .cloned()
        }
    };
    Err(TclError::plain(match version {
        Some(v) => format!("package {name} {v} is not present"),
        None => format!("package {name} is not present"),
    }))
}

/// `PKG_REQUIRE` and the `require:` label `present` jumps to
/// (`generic/tclPkg.c:1321-1393`).
///
/// `sub` is which of the two spelled it, because the `wrong # args` message
/// names the subcommand the user typed.
fn require_cmd(sub: &str, args: &[String], host: &mut dyn ScriptHost) -> Result<String, TclError> {
    let syntax = || TclError::plain(wrong_args(sub, "?-exact? package ?requirement ...?"));
    if args.is_empty() {
        return Err(syntax());
    }
    if args[0] == "-exact" {
        if args.len() != 3 {
            return Err(syntax());
        }
        // `-exact V` becomes the requirement `V-V` (`:1346-1351`), which is
        // also how it comes back out as `exactly V` in a failure message.
        let version = &args[2];
        check_version_and_convert(version).ok_or_else(|| TclError::plain(bad_version(version)))?;
        let reqs = vec![format!("{version}-{version}")];
        return pkg_require(&args[1], &reqs, host);
    }
    pkg_require(&args[0], &args[1..], host)
}

/// `PKG_UNKNOWN` (`generic/tclPkg.c:1395-1416`). An empty command clears it.
fn unknown(args: &[String]) -> Result<String, TclError> {
    let mut reg = registry();
    match args.len() {
        0 => Ok(reg.unknown.clone().unwrap_or_default()),
        1 => {
            reg.unknown = match args[0].is_empty() {
                true => None,
                false => Some(args[0].clone()),
            };
            Ok(String::new())
        }
        _ => Err(TclError::plain(wrong_args("unknown", "?command?"))),
    }
}

// ---------------------------------------------------------------------------
// Lowering
// ---------------------------------------------------------------------------

/// `package` as bytecode: the line, then every argument, then the op that pops
/// all of them.
///
/// Nothing is decided while compiling — not even the argument count. Every
/// answer this command gives depends on a table that only exists at run time,
/// and `tclsh` reports all of them from the command's own invocation, so
/// deciding any of them early would move a diagnostic that is pinned against
/// it. The line rides on the stack for the same reason
/// [`crate::compiler::ext::TK_DISPATCH`] carries one: the failures are located,
/// and `(file "x.tcl" line N)` is part of them.
pub(crate) fn compile(c: &mut Compiler, args: &[Word]) -> Result<(), CompileError> {
    let count = u8::try_from(args.len() + 2)
        .map_err(|_| c.err("more than 253 arguments to the command \"package\"".to_string()))?;
    c.push_value(Value::Int(c.command_line as i64));
    c.push_str("package");
    for arg in args {
        c.word(arg)?;
    }
    c.emit(Op::Extended(ext::PACKAGE, count), 1 - count as i32);
    Ok(())
}

/// Take [`crate::compiler::ext::PACKAGE`]'s operands off the stack: the script
/// line, then the command name and its arguments, in the order the compiler
/// pushed them.
///
/// Separate from [`run`] because the caller has to be holding no borrow of the
/// VM by the time it builds the [`ScriptHost`], which borrows it whole.
pub fn take_args(vm: &mut VM, argc: u8) -> (usize, Vec<String>) {
    let mut values = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        values.push(vm.pop());
    }
    values.reverse();
    let line = match values.first() {
        Some(Value::Int(n)) => *n as usize,
        _ => 0,
    };
    (line, values[1..].iter().map(to_tcl_string).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host for the tests below, which never reach a script.
    struct NoScripts;
    impl ScriptHost for NoScripts {
        fn eval(&mut self, _src: &str) -> Result<String, TclError> {
            panic!("these tests do not run a script")
        }
    }

    fn ok(argv: &[&str]) -> String {
        let argv: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
        run(&argv, &mut NoScripts).expect("should succeed")
    }

    fn err(argv: &[&str]) -> String {
        let argv: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
        run(&argv, &mut NoScripts).expect_err("should fail").msg
    }

    /// The comparisons `Tk_Init` depends on: it provides itself twice at
    /// `TK_PATCH_LEVEL` (`tk9.0.4/generic/tkWindow.c:3461-3469`) and a host
    /// that compared strings would refuse the second.
    #[test]
    fn versions_compare_by_value_and_not_by_spelling() {
        // Measured against tclsh9.0: `package vcompare 9.0 9.0.0` is 0.
        assert_eq!(compare("9.0", "9.0.0"), Some(0));
        assert_eq!(compare("010", "10"), Some(0));
        assert_eq!(compare("1.2", "1.2.0.0"), Some(0));
        assert_eq!(compare("1.2", "1.3"), Some(-1));
        assert_eq!(compare("1.3", "1.2"), Some(1));
        // An alpha sorts below the release it precedes.
        assert_eq!(compare("1.2a3", "1.2"), Some(-1));
        assert_eq!(compare("1.2b1", "1.2"), Some(-1));
        // Everything from a `+` is ignored (`generic/tclPkg.c:1692`).
        assert_eq!(compare("1.0+abc", "1.0"), Some(0));
        // A component wider than 64 bits still compares, because the C never
        // converts to a number (`generic/tclPkg.c:1871-1887`).
        assert_eq!(
            compare("1.99999999999999999999", "1.99999999999999999998"),
            Some(1)
        );
        assert_eq!(compare("bogus", "1"), None);
    }

    /// `vsatisfies`, against the answers tclsh9.0 gives.
    #[test]
    fn a_requirement_admits_a_later_minor_and_refuses_a_later_major() {
        assert_eq!(ok(&["package", "vsatisfies", "1.2", "1.0"]), "1");
        assert_eq!(ok(&["package", "vsatisfies", "2.0", "1.0"]), "0");
        assert_eq!(ok(&["package", "vsatisfies", "1.2", "1.0-2.0"]), "1");
        assert_eq!(ok(&["package", "vsatisfies", "1.0", "1.0-1.0"]), "1");
        assert_eq!(
            err(&["package", "vsatisfies", "1.2", "bogus"]),
            "expected version number but got \"bogus\""
        );
    }

    /// The refusals, which are what a script sees when it gets this wrong.
    #[test]
    fn the_refusals_are_worded_as_tclsh_words_them() {
        assert_eq!(
            err(&["package"]),
            "wrong # args: should be \"package option ?arg ...?\""
        );
        assert_eq!(
            err(&["package", "bogus"]),
            "bad option \"bogus\": must be files, forget, ifneeded, names, prefer, \
             present, provide, require, unknown, vcompare, versions, or vsatisfies"
        );
        assert_eq!(
            err(&["package", "require"]),
            "wrong # args: should be \"package require ?-exact? package ?requirement ...?\""
        );
        assert_eq!(
            err(&["package", "present"]),
            "wrong # args: should be \"package present ?-exact? package ?requirement ...?\""
        );
        assert_eq!(
            err(&["package", "names", "extra"]),
            "wrong # args: should be \"package names\""
        );
        assert_eq!(
            err(&["package", "vcompare", "1"]),
            "wrong # args: should be \"package vcompare version1 version2\""
        );
        // A unique abbreviation is accepted, as `Tcl_GetIndexFromObj` accepts
        // one: measured, `package prov foo` is `package provide foo`.
        assert_eq!(ok(&["package", "prov", "nothing-provided-under-this"]), "");
    }

    /// The `-exact` requirement is `V-V`, and comes back out as `exactly V`.
    #[test]
    fn an_exact_requirement_is_reported_as_exactly() {
        let mut msg = String::new();
        add_requirements_to_result(&mut msg, &["1.3-1.3".to_string()]);
        assert_eq!(msg, " exactly 1.3");
        let mut msg = String::new();
        add_requirements_to_result(&mut msg, &["1.0-2.0".to_string()]);
        assert_eq!(msg, " 1.0-2.0");
    }
}
