//! `cell`: build a `.cell` portable application from a `cell.wcl` file.
//!
//! ```text
//! cell validate <file.wcl>
//! cell build    <file.wcl> [-o out.cell] [--launcher path] [--no-cache]
//! cell inspect  <file.cell>
//! ```

mod build;
mod model;
mod ns;
mod oci;
mod unpack;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

fn usage() -> ! {
    eprintln!("usage:\n  cell validate <file.wcl>\n  cell build <file.wcl> [-o out.cell] [--launcher path] [--no-cache]\n  cell inspect <file.cell>");
    std::process::exit(2)
}

fn find_launcher(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    if let Ok(p) = std::env::var("CELL_LAUNCHER") {
        return Ok(PathBuf::from(p));
    }
    let exe = std::env::current_exe()?;
    let dir = exe.parent().context("exe dir")?;
    let candidates = [
        dir.join("cell-launcher"),
        dir.join("../x86_64-unknown-linux-musl/release/cell-launcher"),
        dir.join("../../x86_64-unknown-linux-musl/release/cell-launcher"),
    ];
    for c in candidates {
        if c.exists() {
            return Ok(c.canonicalize()?);
        }
    }
    bail!("cannot find cell-launcher; pass --launcher or set CELL_LAUNCHER (build it with `cargo build --release -p cell-launcher --target x86_64-unknown-linux-musl`)")
}

fn main() {
    if let Err(e) = real_main() {
        eprintln!("cell: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else { usage() };
    match cmd.as_str() {
        "validate" => {
            let file = PathBuf::from(args.get(1).unwrap_or_else(|| usage()));
            let plan = load(&file)?;
            let issues = model::validate(&plan);
            if issues.is_empty() {
                println!("ok: {} {} ({})", plan.name, plan.version, plan.id);
                Ok(())
            } else {
                for i in &issues {
                    eprintln!("error: {i}");
                }
                bail!("{} issue(s)", issues.len())
            }
        }
        "build" => {
            let file = PathBuf::from(args.get(1).unwrap_or_else(|| usage()));
            let mut out = None;
            let mut launcher = None;
            let mut no_cache = false;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "-o" => {
                        out = Some(PathBuf::from(args.get(i + 1).unwrap_or_else(|| usage())));
                        i += 1;
                    }
                    "--launcher" => {
                        launcher = Some(PathBuf::from(args.get(i + 1).unwrap_or_else(|| usage())));
                        i += 1;
                    }
                    "--no-cache" => no_cache = true,
                    _ => usage(),
                }
                i += 1;
            }
            let plan = load(&file)?;
            let issues = model::validate(&plan);
            if !issues.is_empty() {
                for i in &issues {
                    eprintln!("error: {i}");
                }
                bail!("{} issue(s); fix them and build again", issues.len());
            }
            let out = out.unwrap_or_else(|| {
                let short = plan.id.rsplit('.').next().unwrap_or(&plan.id).to_string();
                plan.root.join(format!("{short}.cell"))
            });
            let out = if out.is_absolute() { out } else { std::env::current_dir()?.join(out) };
            let opts = build::Options {
                out,
                launcher: find_launcher(launcher)?,
                no_cache,
            };
            let code = ns::with_mapped_root(|| build::run_build(&plan, &opts))?;
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }
        "inspect" => {
            let file = PathBuf::from(args.get(1).unwrap_or_else(|| usage()));
            build::inspect(&file)
        }
        _ => usage(),
    }
}

fn load(file: &Path) -> Result<model::Plan> {
    let file = file.canonicalize().with_context(|| format!("find {}", file.display()))?;
    let doc = model::open(&file)?;
    let root = file.parent().context("build file directory")?;
    model::plan(&doc, root)
}
