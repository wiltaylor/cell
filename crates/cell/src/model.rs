//! Read a `cell.wcl` build file into the typed build plan.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use cell_core::format::{
    Audio, Bind, BindMode, Dbus, Desktop, Display, Entry, Gpu, HomeMode, Input, Integration,
    Isolation, NamedCommand, NvidiaMode, PidMode, SandboxMode, Steam,
};
use wcl_lang::{disk_loader, Block, Document, Environment, Registry, Value};

pub const SCHEMA: &str = include_str!("../../../schema/cell.wcl");

/// Where a layer goes when the cell is assembled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlacementKind {
    #[default]
    Embedded,
    Sidecar,
    Referenced,
}

impl PlacementKind {
    fn parse(s: Option<&str>) -> Result<Self> {
        Ok(match s {
            None | Some("embedded") => PlacementKind::Embedded,
            Some("sidecar") => PlacementKind::Sidecar,
            Some("referenced") => PlacementKind::Referenced,
            Some(o) => bail!("placement: unknown {o}"),
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeSpec {
    pub base: String,
    pub multilib: bool,
    pub placement: PlacementKind,
    pub packages: Vec<String>,
    pub setup: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum BuildStep {
    Copy { from: PathBuf, to: String },
    Run(String),
}

#[derive(Debug, Clone)]
pub struct DataLayer {
    pub name: String,
    pub source: PathBuf,
    pub mount: String,
    pub compress: bool,
    pub placement: PlacementKind,
}

#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub id: String,
    pub name: String,
    pub version: String,
    pub arch: String,
    /// `linux` or `windows`.
    pub os: String,
    pub runtime: RuntimeSpec,
    pub steps: Vec<BuildStep>,
    pub data_layers: Vec<DataLayer>,
    pub entry: Entry,
    pub isolation: Isolation,
    pub integration: Integration,
    pub desktop: Option<Desktop>,
    pub steam: Option<Steam>,
    pub root: PathBuf,
}

pub fn open(path: &Path) -> Result<Document> {
    let source =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if !source.contains("import <cell.wcl>") {
        bail!(
            "missing schema import: add `import <cell.wcl>` at the top of {}",
            path.display()
        );
    }
    let mut registry = Registry::new();
    registry.register("cell.wcl", SCHEMA);
    let loader = registry.loader(disk_loader());
    let doc = Document::open_at_with_loader(
        &source,
        &path.display().to_string(),
        path.parent().map(Path::to_path_buf),
        &Environment::new(),
        loader,
    )
    .map_err(|e| anyhow!("parse error: {e}"))?;
    let errors = doc.schema_errors();
    if !errors.is_empty() {
        let mut msg = String::from("schema errors:\n");
        for e in errors {
            msg.push_str(&format!("  - {e}\n"));
        }
        bail!("{msg}");
    }
    Ok(doc)
}

// ---- small readers ---------------------------------------------------------

fn field_value(b: &Block, name: &str) -> Result<Option<Value>> {
    let Some(f) = b.field(name) else {
        return Ok(None);
    };
    match f.value() {
        Ok(Value::None) => Ok(None),
        Ok(v) => Ok(Some(v.clone())),
        Err(e) => bail!("cannot evaluate `{name}`: {e}"),
    }
}

fn string(b: &Block, name: &str) -> Result<Option<String>> {
    match field_value(b, name)? {
        None => Ok(None),
        Some(Value::Utf8(s)) | Some(Value::Ascii(s)) | Some(Value::Identifier(s)) => Ok(Some(s)),
        Some(Value::Symbol(s)) => Ok(Some(s)),
        Some(other) => bail!("`{name}` must be a string, got {other:?}"),
    }
}

fn boolean(b: &Block, name: &str) -> Result<Option<bool>> {
    match field_value(b, name)? {
        None => Ok(None),
        Some(Value::Bool(v)) => Ok(Some(v)),
        Some(other) => bail!("`{name}` must be a bool, got {other:?}"),
    }
}

fn symbol(b: &Block, name: &str) -> Result<Option<String>> {
    match field_value(b, name)? {
        None => Ok(None),
        Some(Value::Symbol(s)) | Some(Value::Utf8(s)) | Some(Value::Identifier(s)) => Ok(Some(s)),
        Some(other) => bail!("`{name}` must be a symbol, got {other:?}"),
    }
}

fn string_list(b: &Block, name: &str) -> Result<Vec<String>> {
    match field_value(b, name)? {
        None => Ok(Vec::new()),
        Some(Value::List(items)) => items
            .iter()
            .map(|v| match v {
                Value::Utf8(s) | Value::Ascii(s) | Value::Identifier(s) | Value::Symbol(s) => {
                    Ok(s.clone())
                }
                other => bail!("`{name}` items must be strings or symbols, got {other:?}"),
            })
            .collect(),
        Some(other) => bail!("`{name}` must be a list, got {other:?}"),
    }
}

fn label(b: &Block) -> Result<String> {
    let labels = b.labels().map_err(|e| anyhow!("label: {e}"))?;
    match labels.first() {
        Some(Value::Utf8(s)) | Some(Value::Ascii(s)) | Some(Value::Identifier(s)) => Ok(s.clone()),
        Some(Value::Symbol(s)) => Ok(s.clone()),
        _ => bail!("`{}` needs a label", b.kind()),
    }
}

fn child<'a>(b: &'a Block<'a>, kind: &str) -> Option<Block<'a>> {
    b.blocks().find(|c| c.kind() == kind)
}

fn children<'a>(b: &'a Block<'a>, kind: &str) -> Vec<Block<'a>> {
    b.blocks().filter(|c| c.kind() == kind).collect()
}

// ---- extraction ------------------------------------------------------------

pub fn plan(doc: &Document, root: &Path) -> Result<Plan> {
    let mut apps = Vec::new();
    let mut templates = Vec::new();
    for b in doc.blocks() {
        match b.kind() {
            "app" => apps.push(b),
            "template" => templates.push(b),
            _ => {}
        }
    }
    if apps.len() != 1 {
        bail!(
            "a build file declares exactly one `app` block, found {}",
            apps.len()
        );
    }
    let app = &apps[0];
    let mut plan = Plan {
        id: label(app)?,
        root: root.to_path_buf(),
        arch: "x86_64".into(),
        os: "linux".into(),
        ..Default::default()
    };
    plan.name = string(app, "name")?.unwrap_or_else(|| plan.id.clone());
    plan.version = string(app, "version")?.unwrap_or_else(|| "0.0.0".into());
    if let Some(a) = string(app, "arch")? {
        plan.arch = a;
    }
    if let Some(o) = string(app, "os")? {
        plan.os = o;
    }
    // Template first, app on top.
    let mut sources: Vec<&Block> = Vec::new();
    if let Some(tname) = string(app, "template")? {
        let t = templates
            .iter()
            .find(|t| label(t).ok().as_deref() == Some(tname.as_str()))
            .with_context(|| format!("template `{tname}` not found"))?;
        sources.push(t);
    }
    sources.push(app);
    for src in sources {
        if let Some(r) = child(src, "runtime") {
            read_runtime(&r, &mut plan.runtime)?;
        }
        if let Some(bld) = child(src, "build") {
            for c in bld.blocks() {
                match c.kind() {
                    "copy" => plan.steps.push(BuildStep::Copy {
                        from: root.join(string(&c, "from")?.context("copy.from")?),
                        to: string(&c, "to")?.context("copy.to")?,
                    }),
                    "run" => plan.steps.push(BuildStep::Run(label(&c)?)),
                    _ => {}
                }
            }
        }
        for l in children(src, "layer") {
            plan.data_layers.push(DataLayer {
                name: label(&l)?,
                source: root.join(string(&l, "source")?.context("layer.source")?),
                mount: string(&l, "mount")?.context("layer.mount")?,
                compress: symbol(&l, "compression")?.as_deref() != Some("raw"),
                placement: PlacementKind::parse(symbol(&l, "placement")?.as_deref())?,
            });
        }
        if let Some(e) = child(src, "entry") {
            read_entry(&e, &mut plan.entry)?;
        }
        if let Some(i) = child(src, "isolation") {
            read_isolation(&i, &mut plan.isolation)?;
        }
        if let Some(i) = child(src, "integration") {
            read_integration(
                &i,
                &mut plan.integration,
                &mut plan.desktop,
                &mut plan.steam,
                root,
            )?;
        }
    }
    if plan.runtime.base.is_empty() && !plan.is_windows() {
        bail!("`runtime {{ base = ... }}` is required");
    }
    if plan.entry.exec.is_empty() {
        bail!("`entry {{ exec = [...] }}` is required");
    }
    Ok(plan)
}

fn read_runtime(b: &Block, r: &mut RuntimeSpec) -> Result<()> {
    if let Some(base) = string(b, "base")? {
        r.base = base;
    }
    if let Some(m) = boolean(b, "multilib")? {
        r.multilib = m;
    }
    if let Some(p) = symbol(b, "placement")? {
        r.placement = PlacementKind::parse(Some(&p))?;
    }
    let pk = string_list(b, "packages")?;
    if !pk.is_empty() {
        r.packages.extend(pk);
    }
    let setup = string_list(b, "setup")?;
    if !setup.is_empty() {
        r.setup.extend(setup);
    }
    Ok(())
}

fn read_entry(b: &Block, e: &mut Entry) -> Result<()> {
    let exec = string_list(b, "exec")?;
    if !exec.is_empty() {
        e.exec = exec;
    }
    if let Some(c) = string(b, "cwd")? {
        e.cwd = Some(c);
    }
    if let Some(g) = boolean(b, "gamescope")? {
        e.gamescope = g;
    }
    for ev in children(b, "env") {
        let name = label(&ev)?;
        let value = string(&ev, "value")?.unwrap_or_default();
        e.env.retain(|(k, _)| k != &name);
        e.env.push((name, value));
    }
    for c in children(b, "command") {
        e.commands.push(NamedCommand {
            name: label(&c)?,
            exec: string_list(&c, "exec")?,
        });
    }
    Ok(())
}

fn read_isolation(b: &Block, i: &mut Isolation) -> Result<()> {
    if let Some(n) = boolean(b, "network")? {
        i.network = n;
    }
    if let Some(h) = symbol(b, "home")? {
        i.home = match h.as_str() {
            "private" => HomeMode::Private,
            "portable" => HomeMode::Portable,
            "host" => HomeMode::Host,
            o => bail!("home: unknown mode {o}"),
        };
    }
    if let Some(p) = symbol(b, "pid")? {
        i.pid = if p == "host" {
            PidMode::Host
        } else {
            PidMode::Private
        };
    }
    if let Some(s) = boolean(b, "shm")? {
        i.shm = s;
    }
    if let Some(s) = symbol(b, "sandbox")? {
        i.sandbox = match s.as_str() {
            "appcontainer" => SandboxMode::AppContainer,
            "off" => SandboxMode::Off,
            o => bail!("sandbox: unknown mode {o}"),
        };
    }
    for bind in children(b, "bind") {
        let mode = match label(&bind)?.as_str() {
            "ro" => BindMode::Ro,
            "rw" => BindMode::Rw,
            "dev" => BindMode::Dev,
            o => bail!("bind: unknown kind {o}"),
        };
        i.binds.push(Bind {
            mode,
            host: string(&bind, "host")?.context("bind.host")?,
            guest: string(&bind, "guest")?.context("bind.guest")?,
            optional: boolean(&bind, "optional")?.unwrap_or(true),
        });
    }
    Ok(())
}

/// Read a PNG named in the build file. The bytes go into the manifest.
fn png(root: &Path, b: &Block, field: &str) -> Result<Option<Vec<u8>>> {
    let Some(p) = string(b, field)? else {
        return Ok(None);
    };
    let bytes = std::fs::read(root.join(&p)).with_context(|| format!("read {field} {p}"))?;
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        bail!("{field}: {p} is not a PNG file");
    }
    Ok(Some(bytes))
}

fn read_integration(
    b: &Block,
    i: &mut Integration,
    desktop: &mut Option<Desktop>,
    steam: &mut Option<Steam>,
    root: &Path,
) -> Result<()> {
    if b.field("display").is_some() {
        i.display = string_list(b, "display")?
            .iter()
            .map(|s| match s.as_str() {
                "wayland" => Ok(Display::Wayland),
                "x11" => Ok(Display::X11),
                "x11_fallback" => Ok(Display::X11Fallback),
                o => bail!("display: unknown {o}"),
            })
            .collect::<Result<_>>()?;
    }
    if b.field("audio").is_some() {
        i.audio = string_list(b, "audio")?
            .iter()
            .map(|s| match s.as_str() {
                "pipewire" => Ok(Audio::Pipewire),
                "pulse" => Ok(Audio::Pulse),
                "alsa" => Ok(Audio::Alsa),
                o => bail!("audio: unknown {o}"),
            })
            .collect::<Result<_>>()?;
    }
    if b.field("input").is_some() {
        i.input = string_list(b, "input")?
            .iter()
            .map(|s| match s.as_str() {
                "gamepad" => Ok(Input::Gamepad),
                "uinput" => Ok(Input::Uinput),
                o => bail!("input: unknown {o}"),
            })
            .collect::<Result<_>>()?;
    }
    if let Some(d) = symbol(b, "dbus")? {
        i.dbus = if d == "session" {
            Dbus::Session
        } else {
            Dbus::None
        };
    }
    if let Some(f) = boolean(b, "fonts")? {
        i.fonts = f;
    }
    if let Some(t) = boolean(b, "themes")? {
        i.themes = t;
    }
    if let Some(g) = child(b, "gpu") {
        let mut gpu = Gpu::default();
        if let Some(e) = boolean(&g, "enable")? {
            gpu.enable = e;
        }
        if let Some(n) = symbol(&g, "nvidia")? {
            gpu.nvidia = match n.as_str() {
                "fetch" => NvidiaMode::Fetch,
                "host" => NvidiaMode::Host,
                "off" | "none" => NvidiaMode::None,
                _ => NvidiaMode::Auto,
            };
        }
        if let Some(v) = boolean(&g, "vaapi")? {
            gpu.vaapi = v;
        }
        i.gpu = gpu;
    }
    if let Some(d) = child(b, "desktop") {
        *desktop = Some(Desktop {
            icon_png: png(root, &d, "icon")?,
            categories: string_list(&d, "categories")?,
            mime: string_list(&d, "mime")?,
            comment: string(&d, "comment")?,
            protocols: string_list(&d, "protocols")?,
            keywords: string_list(&d, "keywords")?,
        });
    }
    if let Some(st) = child(b, "steam") {
        *steam = Some(Steam {
            portrait: png(root, &st, "portrait")?,
            wide: png(root, &st, "wide")?,
            hero: png(root, &st, "hero")?,
            logo: png(root, &st, "logo")?,
            tags: string_list(&st, "tags")?,
            launch_options: string(&st, "launch_options")?,
        });
    }
    Ok(())
}

/// Rules the schema cannot express.
impl Plan {
    pub fn is_windows(&self) -> bool {
        self.os == "windows"
    }
}

/// Walk a host directory and report names Windows cannot hold, plus names
/// that differ only in case (the union volume is case-insensitive).
fn windows_name_issues(root: &Path, label: &str, issues: &mut Vec<String>) {
    fn walk(dir: &Path, rel: &str, label: &str, issues: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let here = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            if let Some(why) = cell_core::extract::windows_name_problem(&name) {
                issues.push(format!("{label}: `{here}` {why}"));
            }
            if let Some(other) = seen.insert(name.to_lowercase(), name.clone()) {
                issues.push(format!(
                    "{label}: `{here}` and `{other}` differ only in case; Windows sees one file"
                ));
            }
            if e.path().is_dir() {
                walk(&e.path(), &here, label, issues);
            }
        }
    }
    if root.is_dir() {
        walk(root, "", label, issues);
    } else if let Some(name) = root.file_name() {
        if let Some(why) = cell_core::extract::windows_name_problem(&name.to_string_lossy()) {
            issues.push(format!("{label}: `{}` {why}", name.to_string_lossy()));
        }
    }
}

pub fn validate(plan: &Plan) -> Vec<String> {
    let mut issues = Vec::new();
    if plan.os != "linux" && plan.os != "windows" {
        issues.push(format!("os: `{}` must be linux or windows", plan.os));
    }
    if plan.is_windows() {
        if plan.arch != "x86_64" {
            issues.push(format!(
                "os = windows: arch `{}` has no launcher yet (x86_64 only)",
                plan.arch
            ));
        }
        if !plan.runtime.packages.is_empty() || !plan.runtime.setup.is_empty() {
            issues.push("os = windows: runtime.packages and runtime.setup need a Linux package manager; copy the files instead".into());
        }
        if !plan.runtime.base.is_empty()
            && !["dir://", "tar://"]
                .iter()
                .any(|s| plan.runtime.base.starts_with(s))
        {
            issues.push(format!(
                "os = windows: runtime.base `{}` must be dir:// or tar:// (no package manager runs for a Windows cell)",
                plan.runtime.base
            ));
        }
        if plan.runtime.multilib {
            issues.push("os = windows: runtime.multilib does not apply".into());
        }
        for s in &plan.steps {
            match s {
                BuildStep::Run(cmd) => issues.push(format!(
                    "os = windows: `run \"{cmd}\"` cannot execute on the Linux build host; use copy steps"
                )),
                BuildStep::Copy { from, to } => {
                    windows_name_issues(from, &format!("copy {to}"), &mut issues);
                    if let Some(last) = to.trim_end_matches('/').rsplit('/').next() {
                        if let Some(why) = cell_core::extract::windows_name_problem(last) {
                            issues.push(format!("copy: `{to}` {why}"));
                        }
                    }
                }
            }
        }
        for l in &plan.data_layers {
            windows_name_issues(&l.source, &format!("layer {}", l.name), &mut issues);
        }
        if !plan.integration.display.is_empty()
            || !plan.integration.audio.is_empty()
            || !plan.integration.input.is_empty()
        {
            issues.push("os = windows: display, audio and input grants do not apply; the app uses the host Win32 APIs (leave them out)".into());
        }
        if plan.entry.gamescope {
            issues.push("os = windows: gamescope does not apply".into());
        }
        if !plan.isolation.binds.is_empty() {
            issues.push(
                "os = windows: bind blocks are not implemented (no process sandbox yet)".into(),
            );
        }
    }
    let has = |d: Display| plan.integration.display.contains(&d);
    if has(Display::X11Fallback) && !has(Display::Wayland) {
        issues.push("display: :x11_fallback needs :wayland in the same list".into());
    }
    for s in &plan.steps {
        if let BuildStep::Copy { from, .. } = s {
            if !from.exists() {
                issues.push(format!("copy: {} does not exist", from.display()));
            }
        }
    }
    for l in &plan.data_layers {
        if !l.source.is_dir() {
            issues.push(format!(
                "layer {}: {} is not a directory",
                l.name,
                l.source.display()
            ));
        }
        if !l.mount.starts_with('/') {
            issues.push(format!("layer {}: mount must be an absolute path", l.name));
        }
    }
    if let Some(d) = &plan.desktop {
        for p in &d.protocols {
            let ok = !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "+-.".contains(c));
            if !ok {
                issues.push(format!(
                    "desktop.protocols: `{p}` is not a URL scheme (lowercase letters, digits, + - .)"
                ));
            }
        }
    }
    let scheme_ok = ["oci://", "docker://", "tar://", "dir://"]
        .iter()
        .any(|s| plan.runtime.base.starts_with(s));
    if !scheme_ok && !(plan.is_windows() && plan.runtime.base.is_empty()) {
        issues.push(format!(
            "runtime.base `{}` must start with oci://, docker://, tar:// or dir://",
            plan.runtime.base
        ));
    }
    issues
}
