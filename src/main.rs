//! `tclrs` — run a Tcl script, or compile one to a native executable.
//!
//! The interpreter path lowers the script to `fusevm` bytecode and runs it on
//! the VM with all three JIT tiers armed; `--aot` lowers the same bytecode
//! through fusevm's closed-world compiler to a native object and links it into
//! a standalone binary. `--tiers` and `--disasm` report what the VM sees, which
//! is how the README's claims about either path are checked.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
usage: tclrs [options] script.tcl
       tclrs [options] -c script

options:
  -c SCRIPT       run SCRIPT instead of a file
  --aot OUT       compile the script to a standalone native executable at OUT
  --aot-object O  emit the relocatable AOT object only (link it yourself)
  --tiers         run the script, then report which fusevm tiers took its chunk
  --disasm        print the compiled bytecode instead of running it
  -h, --help      this message
  -V, --version   version";

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("tclrs: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// What the command line asked for.
enum Action {
    Run,
    Aot(PathBuf),
    AotObject(PathBuf),
    Tiers,
    Disasm,
}

fn run() -> Result<ExitCode, String> {
    let mut args = std::env::args().skip(1);
    let mut action = Action::Run;
    let mut source: Option<String> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(ExitCode::SUCCESS);
            }
            "-V" | "--version" => {
                println!("tclrs {}", env!("CARGO_PKG_VERSION"));
                return Ok(ExitCode::SUCCESS);
            }
            "-c" => {
                let script = args.next().ok_or("-c wants a script")?;
                source = Some(script);
            }
            "--aot" => action = Action::Aot(PathBuf::from(next_path(&mut args, "--aot")?)),
            "--aot-object" => {
                action = Action::AotObject(PathBuf::from(next_path(&mut args, "--aot-object")?))
            }
            "--tiers" => action = Action::Tiers,
            "--disasm" => action = Action::Disasm,
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(format!("unknown option \"{other}\"\n{USAGE}"))
            }
            path => {
                source = Some(read_script(Path::new(path))?);
            }
        }
    }

    let src = source.ok_or_else(|| format!("no script given\n{USAGE}"))?;

    match action {
        Action::Run => {
            tclrs::runtime::run_to_stdout(&src)?;
            Ok(ExitCode::SUCCESS)
        }
        Action::Aot(out) => {
            tclrs::aot::compile_executable(&src, &out)?;
            Ok(ExitCode::SUCCESS)
        }
        Action::AotObject(out) => {
            tclrs::aot::compile_object(&src, &out)?;
            Ok(ExitCode::SUCCESS)
        }
        Action::Tiers => {
            println!("{}", tclrs::tiers::report(&src)?);
            Ok(ExitCode::SUCCESS)
        }
        Action::Disasm => {
            print!("{}", tclrs::runtime::compile(&src)?.disassemble());
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn next_path(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{flag} wants a path"))
}

fn read_script(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}
