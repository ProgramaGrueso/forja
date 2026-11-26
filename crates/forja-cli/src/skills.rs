//! `forja skills`: skills genéricas embebidas en el binario (ver `skills/` en el repo)
//! que se copian a la carpeta de skills de cada agente.
//!
//! Garantías:
//! - Por defecto todo es DRY-RUN; solo `--yes` escribe.
//! - Nunca sobrescribe una skill existente sin `--force` y nunca sigue enlaces
//!   simbólicos (ni siquiera con `--force`).
//! - Nunca borra: `uninstall` solo quita los ficheros que Forja instaló en carpetas
//!   marcadas con `.forja-skill` (y las carpetas que queden vacías).
//! - Los perfiles de Hermes ocultos (`hidden_profiles` / `FORJA_HIDDEN_PROFILES`) se
//!   rechazan igual que en el resto de Forja.

use std::io::Write;
use std::path::{Path, PathBuf};

use forja_agent::hermes::{discover_profiles_in, hermes_base, validate_profile_with, ProfileRules};
use forja_core::{ForjaError, Result};
use forja_server::orchestrator::agent_installed;
use forja_server::settings::load_settings;

include!(concat!(env!("OUT_DIR"), "/skills_embedded.rs"));

/// Marcador que Forja deja dentro de cada skill que instala.
pub const MARKER: &str = ".forja-skill";
/// Agentes con carpeta de skills conocida.
pub const SKILL_AGENTS: [&str; 4] = ["claude", "opencode", "agy", "hermes"];

fn io_err(err: std::io::Error) -> ForjaError {
    ForjaError::Io(err)
}

fn invalid(msg: impl Into<String>) -> ForjaError {
    ForjaError::InvalidInput(msg.into())
}

/// Skill embebida.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// (ruta relativa dentro de la skill, contenido)
    pub files: Vec<(&'static str, &'static str)>,
}

/// Extrae `description:` del frontmatter (acepta comillas).
pub fn parse_description(skill_md: &str) -> String {
    let mut lines = skill_md.lines();
    if lines.next().map(str::trim) != Some("---") {
        return String::new();
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some(rest) = line.strip_prefix("description:") {
            return rest
                .trim()
                .trim_matches(|c| c == '"' || c == '\'')
                .to_string();
        }
    }
    String::new()
}

/// Catálogo ordenado por nombre a partir de una tabla (ruta, contenido).
pub fn catalog_from(table: &'static [(&'static str, &'static str)]) -> Vec<Skill> {
    let mut skills: Vec<Skill> = Vec::new();
    for (path, content) in table {
        let Some((name, rel)) = path.split_once('/') else {
            continue;
        };
        if !skills.iter().any(|s| s.name == name) {
            skills.push(Skill {
                name: name.to_string(),
                description: String::new(),
                files: Vec::new(),
            });
        }
        let skill = skills.iter_mut().find(|s| s.name == name).expect("skill");
        if rel == "SKILL.md" {
            skill.description = parse_description(content);
        }
        skill.files.push((rel, content));
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// Skills embebidas en este binario.
pub fn catalog() -> Vec<Skill> {
    catalog_from(EMBEDDED)
}

/// Entorno de las operaciones (inyectable en tests).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillsEnv {
    pub home: Option<PathBuf>,
    /// `XDG_CONFIG_HOME` o `<home>/.config`.
    pub config_home: Option<PathBuf>,
    pub hermes_base: Option<PathBuf>,
    /// Agentes instalados (subconjunto de [`SKILL_AGENTS`]).
    pub installed: Vec<String>,
    /// Perfiles de Hermes existentes (incluye `default`).
    pub hermes_profiles: Vec<String>,
    /// Perfiles ocultos efectivos (ajustes ∪ `FORJA_HIDDEN_PROFILES`).
    pub hidden: Vec<String>,
}

impl SkillsEnv {
    /// Detecta el entorno real. No escribe nada.
    pub fn detect(store_dir: &Path) -> Result<Self> {
        let home = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from);
        let config_home = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".config")));
        let installed = SKILL_AGENTS
            .iter()
            .filter(|a| agent_installed(a))
            .map(|a| a.to_string())
            .collect();
        let base = hermes_base();
        let hermes_profiles = base
            .as_deref()
            .map(discover_profiles_in)
            .unwrap_or_else(|| vec!["default".to_string()]);
        let rules = ProfileRules::new();
        let settings = load_settings(&store_dir.join("settings.json")).map_err(invalid)?;
        if let Some(s) = settings {
            rules.set_hidden(&s.hidden_profiles);
        }
        Ok(Self {
            home,
            config_home,
            hermes_base: base,
            installed,
            hermes_profiles,
            hidden: rules.hidden(),
        })
    }
}

/// Qué agentes, skills y perfil se tocan. Vacío = todos los instalados / todas.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    pub agents: Vec<String>,
    pub skills: Vec<String>,
    pub profile: Option<String>,
}

/// Destino concreto: una carpeta de skills de un agente.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub agent: String,
    pub profile: Option<String>,
    pub root: PathBuf,
}

/// Carpeta de skills de un agente (no toca el disco).
pub fn target_root(env: &SkillsEnv, agent: &str, profile: Option<&str>) -> Result<PathBuf> {
    let need_home = || {
        env.home
            .clone()
            .ok_or_else(|| invalid("no se pudo determinar HOME"))
    };
    match agent {
        "claude" => Ok(need_home()?.join(".claude").join("skills")),
        "opencode" => {
            let cfg = env
                .config_home
                .clone()
                .map(Ok)
                .unwrap_or_else(|| need_home().map(|h| h.join(".config")))?;
            Ok(cfg.join("opencode").join("skills"))
        }
        // agy lee `~/.gemini/skills` (prioridad) y `~/.agents/skills` (compartida).
        "agy" => Ok(need_home()?.join(".gemini").join("skills")),
        "hermes" => {
            let base = env
                .hermes_base
                .clone()
                .ok_or_else(|| invalid("no se pudo determinar la base de Hermes"))?;
            match profile {
                None | Some("default") => Ok(base.join("skills")),
                Some(p) => Ok(base.join("profiles").join(p).join("skills")),
            }
        }
        other => Err(invalid(format!(
            "agente sin carpeta de skills: {other} (válidos: {})",
            SKILL_AGENTS.join(", ")
        ))),
    }
}

/// Resuelve los destinos de la selección, validando agente y perfil.
pub fn resolve_targets(env: &SkillsEnv, sel: &Selection) -> Result<Vec<Target>> {
    let profile = match sel.profile.as_deref() {
        Some(p) => Some(validate_profile_with(p, &env.hermes_profiles, &env.hidden)?),
        None => None,
    };
    let agents: Vec<String> = if sel.agents.is_empty() {
        env.installed.clone()
    } else {
        for a in &sel.agents {
            if !SKILL_AGENTS.contains(&a.as_str()) {
                return Err(invalid(format!(
                    "agente sin carpeta de skills: {a} (válidos: {})",
                    SKILL_AGENTS.join(", ")
                )));
            }
        }
        sel.agents.clone()
    };
    let mut out = Vec::new();
    for agent in agents {
        let prof = if agent == "hermes" {
            Some(profile.clone().unwrap_or_else(|| "default".to_string()))
        } else {
            None
        };
        let root = target_root(env, &agent, prof.as_deref())?;
        out.push(Target {
            agent,
            profile: prof,
            root,
        });
    }
    Ok(out)
}

/// Skills elegidas (todas si `names` está vacío).
pub fn select_skills(all: &[Skill], names: &[String]) -> Result<Vec<Skill>> {
    if names.is_empty() {
        return Ok(all.to_vec());
    }
    let mut out = Vec::new();
    for n in names {
        match all.iter().find(|s| &s.name == n) {
            Some(s) if !out.iter().any(|o: &Skill| o.name == s.name) => out.push(s.clone()),
            Some(_) => {}
            None => {
                return Err(invalid(format!(
                    "skill desconocida: {n} (mira `forja skills list`)"
                )))
            }
        }
    }
    Ok(out)
}

/// Estado de una skill en un destino.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillState {
    Missing,
    /// Carpeta con marcador `.forja-skill`.
    Ours,
    /// Existe pero sin marcador.
    Foreign,
    /// Es un enlace simbólico: no se toca.
    Symlink,
}

pub fn state_of(root: &Path, name: &str) -> SkillState {
    let dir = root.join(name);
    match std::fs::symlink_metadata(&dir) {
        Err(_) => SkillState::Missing,
        Ok(m) if m.file_type().is_symlink() => SkillState::Symlink,
        Ok(m) if !m.is_dir() => SkillState::Foreign,
        Ok(_) => match std::fs::symlink_metadata(dir.join(MARKER)) {
            Ok(m) if m.is_file() => SkillState::Ours,
            _ => SkillState::Foreign,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Copy,
    Overwrite,
    Remove,
    SkipInstalled,
    SkipForeign,
    SkipSymlink,
    SkipMissing,
}

impl Action {
    pub fn writes(self) -> bool {
        matches!(self, Self::Copy | Self::Overwrite | Self::Remove)
    }

    fn label(self) -> &'static str {
        match self {
            Self::Copy => "copiar",
            Self::Overwrite => "sobrescribir",
            Self::Remove => "quitar",
            Self::SkipInstalled => "omitir (ya instalada por Forja; --force para actualizar)",
            Self::SkipForeign => "omitir (existe y no es de Forja; --force para sobrescribir)",
            Self::SkipSymlink => "omitir (es un enlace simbólico, no se toca)",
            Self::SkipMissing => "omitir (no instalada)",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Item {
    pub target: Target,
    pub skill: Skill,
    pub dest: PathBuf,
    pub action: Action,
}

pub fn plan_install(targets: &[Target], skills: &[Skill], force: bool) -> Vec<Item> {
    let mut items = Vec::new();
    for t in targets {
        for s in skills {
            let action = match state_of(&t.root, &s.name) {
                SkillState::Missing => Action::Copy,
                SkillState::Ours | SkillState::Foreign if force => Action::Overwrite,
                SkillState::Ours => Action::SkipInstalled,
                SkillState::Foreign => Action::SkipForeign,
                SkillState::Symlink => Action::SkipSymlink,
            };
            items.push(Item {
                target: t.clone(),
                skill: s.clone(),
                dest: t.root.join(&s.name),
                action,
            });
        }
    }
    items
}

pub fn plan_uninstall(targets: &[Target], skills: &[Skill]) -> Vec<Item> {
    let mut items = Vec::new();
    for t in targets {
        for s in skills {
            let action = match state_of(&t.root, &s.name) {
                SkillState::Ours => Action::Remove,
                SkillState::Missing => Action::SkipMissing,
                SkillState::Foreign => Action::SkipForeign,
                SkillState::Symlink => Action::SkipSymlink,
            };
            items.push(Item {
                target: t.clone(),
                skill: s.clone(),
                dest: t.root.join(&s.name),
                action,
            });
        }
    }
    items
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

fn install_one(item: &Item) -> Result<()> {
    std::fs::create_dir_all(&item.dest).map_err(io_err)?;
    for (rel, content) in &item.skill.files {
        let path = item.dest.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io_err)?;
        }
        if is_symlink(&path) {
            // Nunca escribir a través de un enlace.
            continue;
        }
        std::fs::write(&path, content).map_err(io_err)?;
    }
    let marker = item.dest.join(MARKER);
    if !is_symlink(&marker) {
        std::fs::write(
            marker,
            format!(
                "skill={} instalada por forja {}\n",
                item.skill.name,
                env!("CARGO_PKG_VERSION")
            ),
        )
        .map_err(io_err)?;
    }
    Ok(())
}

fn remove_one(item: &Item) -> Result<()> {
    for (rel, _) in &item.skill.files {
        let path = item.dest.join(rel);
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
            std::fs::remove_file(&path).map_err(io_err)?;
        }
    }
    let marker = item.dest.join(MARKER);
    if std::fs::symlink_metadata(&marker).is_ok_and(|m| m.is_file()) {
        std::fs::remove_file(&marker).map_err(io_err)?;
    }
    // Carpetas que quedaron vacías (de lo más profundo a lo más superficial);
    // `remove_dir` falla si queda algo, y eso es lo que queremos.
    let mut dirs: Vec<PathBuf> = item
        .skill
        .files
        .iter()
        .filter_map(|(rel, _)| Path::new(rel).parent().map(|p| item.dest.join(p)))
        .collect();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    dirs.dedup();
    for d in dirs {
        let _ = std::fs::remove_dir(&d);
    }
    let _ = std::fs::remove_dir(&item.dest);
    Ok(())
}

/// Ejecuta los elementos que escriben. Devuelve cuántos se aplicaron.
pub fn apply(items: &[Item]) -> Result<usize> {
    let mut n = 0;
    for item in items {
        match item.action {
            Action::Copy | Action::Overwrite => install_one(item)?,
            Action::Remove => remove_one(item)?,
            _ => continue,
        }
        n += 1;
    }
    Ok(n)
}

/// Imprime el plan agrupado por destino.
pub fn print_plan<W: Write>(out: &mut W, items: &[Item], applied: bool) -> Result<()> {
    let mut last: Option<(&str, &Path)> = None;
    for item in items {
        let key = (item.target.agent.as_str(), item.target.root.as_path());
        if last != Some(key) {
            let prof = item
                .target
                .profile
                .as_deref()
                .map(|p| format!(" [perfil {p}]"))
                .unwrap_or_default();
            writeln!(
                out,
                "{}{} -> {}",
                item.target.agent,
                prof,
                item.target.root.display()
            )
            .map_err(io_err)?;
            last = Some(key);
        }
        let verb = if applied && item.action.writes() {
            match item.action {
                Action::Copy => "copiada",
                Action::Overwrite => "sobrescrita",
                _ => "quitada",
            }
            .to_string()
        } else {
            item.action.label().to_string()
        };
        writeln!(out, "  {:<24} {verb}", item.skill.name).map_err(io_err)?;
    }
    Ok(())
}

fn no_targets_note<W: Write>(out: &mut W) -> Result<()> {
    writeln!(
        out,
        "No hay agentes con carpeta de skills detectados (claude, opencode, agy, hermes). \
         Usa --agent para elegir uno explícitamente."
    )
    .map_err(io_err)
}

/// `forja skills list`.
pub fn cmd_list<W: Write>(out: &mut W) -> Result<()> {
    let skills = catalog();
    writeln!(out, "{} skills incluidas en Forja:", skills.len()).map_err(io_err)?;
    for s in &skills {
        writeln!(out, "  {:<24} {}", s.name, s.description).map_err(io_err)?;
    }
    Ok(())
}

/// `forja skills status`.
pub fn cmd_status<W: Write>(out: &mut W, env: &SkillsEnv, sel: &Selection) -> Result<()> {
    let skills = select_skills(&catalog(), &sel.skills)?;
    let targets = resolve_targets(env, sel)?;
    if targets.is_empty() {
        return no_targets_note(out);
    }
    for t in &targets {
        let installed = env.installed.contains(&t.agent);
        let prof = t
            .profile
            .as_deref()
            .map(|p| format!(" [perfil {p}]"))
            .unwrap_or_default();
        writeln!(
            out,
            "{}{}{} -> {}",
            t.agent,
            prof,
            if installed { "" } else { " (no detectado)" },
            t.root.display()
        )
        .map_err(io_err)?;
        let mut ours = 0;
        for s in &skills {
            let st = state_of(&t.root, &s.name);
            let (mark, txt) = match st {
                SkillState::Ours => {
                    ours += 1;
                    ("ok", "instalada por Forja")
                }
                SkillState::Foreign => ("!!", "existe (no es de Forja)"),
                SkillState::Symlink => ("!!", "enlace simbólico (no se toca)"),
                SkillState::Missing => ("--", "falta"),
            };
            writeln!(out, "  [{mark}] {:<24} {txt}", s.name).map_err(io_err)?;
        }
        writeln!(out, "  {ours}/{} instaladas por Forja", skills.len()).map_err(io_err)?;
    }
    Ok(())
}

/// `forja skills install`: dry-run salvo `yes`.
pub fn cmd_install<W: Write>(
    out: &mut W,
    env: &SkillsEnv,
    sel: &Selection,
    force: bool,
    yes: bool,
) -> Result<()> {
    let skills = select_skills(&catalog(), &sel.skills)?;
    let targets = resolve_targets(env, sel)?;
    if targets.is_empty() {
        return no_targets_note(out);
    }
    let items = plan_install(&targets, &skills, force);
    if !yes {
        writeln!(out, "DRY-RUN: no se escribe nada.").map_err(io_err)?;
        print_plan(out, &items, false)?;
        let n = items.iter().filter(|i| i.action.writes()).count();
        writeln!(out, "{n} a copiar. Añade --yes para aplicar.").map_err(io_err)?;
        return Ok(());
    }
    let n = apply(&items)?;
    print_plan(out, &items, true)?;
    writeln!(out, "{n} skills instaladas.").map_err(io_err)
}

/// `forja skills uninstall`: dry-run salvo `yes`; solo quita lo marcado por Forja.
pub fn cmd_uninstall<W: Write>(
    out: &mut W,
    env: &SkillsEnv,
    sel: &Selection,
    yes: bool,
) -> Result<()> {
    let skills = select_skills(&catalog(), &sel.skills)?;
    let targets = resolve_targets(env, sel)?;
    if targets.is_empty() {
        return no_targets_note(out);
    }
    let items = plan_uninstall(&targets, &skills);
    if !yes {
        writeln!(out, "DRY-RUN: no se borra nada.").map_err(io_err)?;
        print_plan(out, &items, false)?;
        let n = items.iter().filter(|i| i.action.writes()).count();
        writeln!(out, "{n} a quitar. Añade --yes para aplicar.").map_err(io_err)?;
        return Ok(());
    }
    let n = apply(&items)?;
    print_plan(out, &items, true)?;
    writeln!(out, "{n} skills quitadas.").map_err(io_err)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn tmp() -> TempDir {
        TempDir::new_in("/tmp").expect("tempdir")
    }

    fn env_in(root: &Path, installed: &[&str], profiles: &[&str], hidden: &[&str]) -> SkillsEnv {
        SkillsEnv {
            home: Some(root.join("home")),
            config_home: Some(root.join("home/.config")),
            hermes_base: Some(root.join("hermes")),
            installed: installed.iter().map(|s| s.to_string()).collect(),
            hermes_profiles: profiles.iter().map(|s| s.to_string()).collect(),
            hidden: hidden.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn sel(agents: &[&str], skills: &[&str], profile: Option<&str>) -> Selection {
        Selection {
            agents: agents.iter().map(|s| s.to_string()).collect(),
            skills: skills.iter().map(|s| s.to_string()).collect(),
            profile: profile.map(str::to_string),
        }
    }

    fn run<F: FnOnce(&mut Vec<u8>) -> Result<()>>(f: F) -> (Result<()>, String) {
        let mut out = Vec::new();
        let r = f(&mut out);
        (r, String::from_utf8(out).expect("utf8"))
    }

    #[test]
    fn catalog_has_between_15_and_25_skills_with_descriptions() {
        let c = catalog();
        assert!((15..=25).contains(&c.len()), "{} skills", c.len());
        for s in &c {
            assert!(!s.description.is_empty(), "{} sin description", s.name);
            assert!(s.files.iter().any(|(r, _)| *r == "SKILL.md"));
            let md = s.files.iter().find(|(r, _)| *r == "SKILL.md").unwrap().1;
            assert!(md.contains(&format!("name: {}", s.name)), "{}", s.name);
        }
        assert!(c.iter().any(|s| s.name == "forja-orquestador"));
        assert!(c.iter().any(|s| s.name == "forja-revision"));
    }

    #[test]
    fn embedded_skills_have_no_personal_data() {
        let banned = [
            "/home/", "juang", "claudia", "yuuki", "intimo", "susi", "sigecq", "utp",
        ];
        for (path, content) in EMBEDDED {
            let lower = content.to_lowercase();
            for b in banned {
                if b == "utp" {
                    assert!(!lower.split(|c: char| !c.is_alphanumeric()).any(|w| w == b));
                } else {
                    assert!(!lower.contains(b), "{path} contiene {b}");
                }
            }
        }
    }

    #[test]
    fn parse_description_handles_quotes_and_missing() {
        assert_eq!(
            parse_description("---\nname: a\ndescription: \"hola\"\n---\n"),
            "hola"
        );
        assert_eq!(
            parse_description("---\ndescription: sin comillas\n---\n"),
            "sin comillas"
        );
        assert_eq!(parse_description("sin frontmatter"), "");
    }

    #[test]
    fn target_roots_per_agent() {
        let r = tmp();
        let env = env_in(r.path(), &[], &["default", "uno"], &[]);
        let home = r.path().join("home");
        assert_eq!(
            target_root(&env, "claude", None).unwrap(),
            home.join(".claude/skills")
        );
        assert_eq!(
            target_root(&env, "opencode", None).unwrap(),
            home.join(".config/opencode/skills")
        );
        assert_eq!(
            target_root(&env, "agy", None).unwrap(),
            home.join(".gemini/skills")
        );
        assert_eq!(
            target_root(&env, "hermes", Some("default")).unwrap(),
            r.path().join("hermes/skills")
        );
        assert_eq!(
            target_root(&env, "hermes", Some("uno")).unwrap(),
            r.path().join("hermes/profiles/uno/skills")
        );
        assert!(target_root(&env, "mock", None).is_err());
    }

    #[test]
    fn hidden_and_unknown_profiles_are_rejected() {
        let r = tmp();
        let env = env_in(
            r.path(),
            &["hermes"],
            &["default", "uno", "priv"],
            &["priv"],
        );
        let e = resolve_targets(&env, &sel(&["hermes"], &[], Some("priv"))).unwrap_err();
        assert!(e.to_string().contains("oculto"), "{e}");
        let e = resolve_targets(&env, &sel(&["hermes"], &[], Some("nope"))).unwrap_err();
        assert!(e.to_string().contains("no existe"), "{e}");
        let e = resolve_targets(&env, &sel(&["hermes"], &[], Some("../x"))).unwrap_err();
        assert!(e.to_string().contains("inválido"), "{e}");
        let t = resolve_targets(&env, &sel(&["hermes"], &[], Some("uno"))).unwrap();
        assert_eq!(t[0].profile.as_deref(), Some("uno"));
    }

    #[test]
    fn dry_run_writes_nothing_and_yes_installs() {
        let r = tmp();
        let env = env_in(r.path(), &["claude"], &["default"], &[]);
        let s = sel(&[], &["forja-revision", "spike"], None);

        let (res, text) = run(|o| cmd_install(o, &env, &s, false, false));
        res.unwrap();
        assert!(text.contains("DRY-RUN"), "{text}");
        assert!(text.contains("copiar"), "{text}");
        assert!(
            !r.path().join("home").exists(),
            "el dry-run no debe crear nada"
        );

        let (res, text) = run(|o| cmd_install(o, &env, &s, false, true));
        res.unwrap();
        assert!(text.contains("2 skills instaladas"), "{text}");
        let d = r.path().join("home/.claude/skills/forja-revision");
        assert!(d.join("SKILL.md").is_file());
        assert!(d.join(MARKER).is_file());
        assert_eq!(
            state_of(&r.path().join("home/.claude/skills"), "spike"),
            SkillState::Ours
        );
    }

    #[test]
    fn install_never_overwrites_without_force_and_never_follows_symlinks() {
        let r = tmp();
        let env = env_in(r.path(), &["claude"], &["default"], &[]);
        let root = r.path().join("home/.claude/skills");
        fs::create_dir_all(root.join("spike")).unwrap();
        fs::write(root.join("spike/SKILL.md"), "mía").unwrap();
        // Enlace simbólico a una carpeta ajena.
        let outside = r.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("SKILL.md"), "ajena").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("forja-revision")).unwrap();

        let s = sel(&[], &["spike", "forja-revision"], None);
        let (res, text) = run(|o| cmd_install(o, &env, &s, false, true));
        res.unwrap();
        assert!(text.contains("0 skills instaladas"), "{text}");
        assert_eq!(
            fs::read_to_string(root.join("spike/SKILL.md")).unwrap(),
            "mía"
        );

        // Con --force se sobrescribe la carpeta propia pero nunca el enlace.
        let (res, _) = run(|o| cmd_install(o, &env, &s, true, true));
        res.unwrap();
        assert_ne!(
            fs::read_to_string(root.join("spike/SKILL.md")).unwrap(),
            "mía"
        );
        assert!(root.join("spike").join(MARKER).is_file());
        assert_eq!(
            fs::read_to_string(outside.join("SKILL.md")).unwrap(),
            "ajena"
        );
        assert!(!outside.join(MARKER).exists());
    }

    #[test]
    fn second_install_skips_and_force_updates() {
        let r = tmp();
        let env = env_in(r.path(), &["opencode"], &["default"], &[]);
        let s = sel(&[], &["writing-plans"], None);
        run(|o| cmd_install(o, &env, &s, false, true)).0.unwrap();
        let f = r
            .path()
            .join("home/.config/opencode/skills/writing-plans/SKILL.md");
        fs::write(&f, "editada").unwrap();
        let (_, text) = run(|o| cmd_install(o, &env, &s, false, true));
        assert!(text.contains("ya instalada por Forja"), "{text}");
        assert_eq!(fs::read_to_string(&f).unwrap(), "editada");
        run(|o| cmd_install(o, &env, &s, true, true)).0.unwrap();
        assert_ne!(fs::read_to_string(&f).unwrap(), "editada");
    }

    #[test]
    fn uninstall_only_removes_marked_and_keeps_foreign_and_extra_files() {
        let r = tmp();
        let env = env_in(r.path(), &["claude"], &["default"], &[]);
        let root = r.path().join("home/.claude/skills");
        let s = sel(&[], &["github", "spike"], None);
        run(|o| cmd_install(o, &env, &s, false, true)).0.unwrap();
        // Skill ajena sin marcador con el mismo nombre que una nuestra.
        fs::create_dir_all(root.join("writing-plans")).unwrap();
        fs::write(root.join("writing-plans/SKILL.md"), "ajena").unwrap();
        // Fichero extra del usuario dentro de una skill nuestra.
        fs::write(root.join("spike/notas.txt"), "mis notas").unwrap();

        let all = sel(&[], &["github", "spike", "writing-plans"], None);
        let (_, dry) = run(|o| cmd_uninstall(o, &env, &all, false));
        assert!(dry.contains("DRY-RUN"), "{dry}");
        assert!(root.join("github/SKILL.md").exists(), "dry-run no borra");

        let (res, text) = run(|o| cmd_uninstall(o, &env, &all, true));
        res.unwrap();
        assert!(text.contains("2 skills quitadas"), "{text}");
        assert!(!root.join("github").exists(), "carpeta vacía se retira");
        assert!(!root.join("spike/SKILL.md").exists());
        assert!(!root.join("spike").join(MARKER).exists());
        assert_eq!(
            fs::read_to_string(root.join("spike/notas.txt")).unwrap(),
            "mis notas"
        );
        assert_eq!(
            fs::read_to_string(root.join("writing-plans/SKILL.md")).unwrap(),
            "ajena"
        );
    }

    #[test]
    fn status_shows_per_agent_state_and_hermes_profile_dir() {
        let r = tmp();
        let env = env_in(r.path(), &["claude", "hermes"], &["default", "uno"], &[]);
        let s1 = sel(&["hermes"], &["spike"], Some("uno"));
        run(|o| cmd_install(o, &env, &s1, false, true)).0.unwrap();
        assert!(r
            .path()
            .join("hermes/profiles/uno/skills/spike/SKILL.md")
            .is_file());

        let (res, text) = run(|o| cmd_status(o, &env, &sel(&[], &["spike"], Some("uno"))));
        res.unwrap();
        assert!(text.contains("claude"), "{text}");
        assert!(text.contains("hermes [perfil uno]"), "{text}");
        assert!(text.contains("[ok] spike"), "{text}");
        assert!(text.contains("[--] spike"), "{text}");
    }

    #[test]
    fn no_installed_agents_is_a_clear_note_not_an_error() {
        let r = tmp();
        let env = env_in(r.path(), &[], &["default"], &[]);
        let (res, text) = run(|o| cmd_install(o, &env, &Selection::default(), false, true));
        res.unwrap();
        assert!(text.contains("No hay agentes"), "{text}");
        assert!(!r.path().join("home").exists());
    }

    #[test]
    fn unknown_skill_or_agent_is_an_error() {
        let r = tmp();
        let env = env_in(r.path(), &["claude"], &["default"], &[]);
        let (res, _) = run(|o| cmd_install(o, &env, &sel(&[], &["nada"], None), false, false));
        assert!(res.is_err());
        let (res, _) = run(|o| cmd_install(o, &env, &sel(&["mock"], &[], None), false, false));
        assert!(res.is_err());
    }
}
