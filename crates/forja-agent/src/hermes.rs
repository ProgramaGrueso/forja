//! Adaptador de Hermes.
//!
//! Ejecuta el CLI real con la forma `hermes -p <perfil> chat --query-file
//! <archivo> -Q --oneshot` (flags confirmadas con `hermes --help` y
//! `hermes chat --help`) sin pasar por ninguna shell: el prompt viaja
//! íntegro en un archivo temporal con nombre único (uuid v4) creado en
//! `std::env::temp_dir()` y borrado al terminar, incluso si hay error.
//!
//! Los perfiles se DESCUBREN (`default` + subdirectorios de
//! `<HERMES_HOME o ~/.hermes>/profiles`, ver [`discover_profiles_in`]) y se
//! pueden ocultar con la lista configurable de [`ProfileRules`] (ajuste
//! `hidden_profiles` y variable `FORJA_HIDDEN_PROFILES`). Un perfil inexistente
//! u oculto se rechaza con `ForjaError::InvalidInput` ANTES de crear archivos o
//! procesos.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use forja_core::{AgentKind, ForjaError, Result};
use uuid::Uuid;

use crate::{Agent, AgentFuture, AgentOutput, RunContext, Runner};

/// Perfil por defecto del adaptador Hermes.
pub const DEFAULT_PROFILE: &str = "default";

/// Variable de entorno con perfiles ocultos (lista separada por comas).
pub const HIDDEN_PROFILES_ENV: &str = "FORJA_HIDDEN_PROFILES";

/// Opciones puras para armar el argv de hermes (no ejecuta nada).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HermesCommandOptions {
    /// Perfil ya validado (existente y no oculto).
    pub profile: String,
    /// Ruta del archivo con el prompt; el CLI lo lee tal cual.
    pub query_file: PathBuf,
}

impl HermesCommandOptions {
    pub fn new(profile: impl Into<String>, query_file: impl Into<PathBuf>) -> Self {
        Self {
            profile: profile.into(),
            query_file: query_file.into(),
        }
    }
}

/// Arma el vector de argumentos de hermes SIN ejecutar nada (función pura).
///
/// Forma confirmada con la ayuda real del CLI: `-p <perfil>` aparece en el
/// epílogo de `hermes --help` ("Run any command against a named profile's
/// home (also --profile)"; se consume antes de argparse, por eso no sale en
/// la línea de uso) y `--query-file PATH`, `-Q`/`--quiet` y `--oneshot`
/// aparecen en `hermes chat --help`. El prompt nunca va aquí: viaja por el
/// archivo `query_file`, seguro para comillas, saltos de línea y cualquier
/// metacarácter de shell.
pub fn build_command(opts: &HermesCommandOptions) -> Vec<String> {
    vec![
        "hermes".to_string(),
        "-p".to_string(),
        opts.profile.clone(),
        "chat".to_string(),
        "--query-file".to_string(),
        opts.query_file.to_string_lossy().into_owned(),
        "-Q".to_string(),
        "--oneshot".to_string(),
    ]
}

/// Normaliza y valida el FORMATO del perfil (sin tocar el disco).
///
/// La normalización (recortar y pasar a minúsculas) espeja lo que hace el
/// propio hermes con `-p`. Un nombre válido solo tiene letras, números, guion y
/// guion bajo, no empieza por guion ni guion bajo y mide como máximo 64
/// caracteres (espeja la regla interna de hermes `^[a-z0-9][a-z0-9_-]{0,63}$`).
pub fn validate_profile_format(profile: &str) -> Result<String> {
    let normalized = profile.trim().to_lowercase();
    if !is_valid_profile_name(&normalized) {
        return Err(ForjaError::InvalidInput(format!(
            "nombre de perfil de hermes inválido: \"{profile}\" \
             (solo letras, números, guion y guion bajo)"
        )));
    }
    Ok(normalized)
}

/// Valida un perfil contra la lista descubierta y la de ocultos (función pura).
///
/// Orden: formato, oculto, existencia. Devuelve el perfil normalizado.
pub fn validate_profile_with(
    profile: &str,
    available: &[String],
    hidden: &[String],
) -> Result<String> {
    let normalized = validate_profile_format(profile)?;
    if hidden.iter().any(|h| h == &normalized) {
        return Err(ForjaError::InvalidInput(format!(
            "perfil de hermes no permitido: \"{normalized}\" está oculto \
             (hidden_profiles / {HIDDEN_PROFILES_ENV})"
        )));
    }
    if !available.iter().any(|a| a == &normalized) {
        return Err(ForjaError::InvalidInput(format!(
            "perfil de hermes no permitido: \"{normalized}\" no existe \
             (los disponibles son: {})",
            available.join(", ")
        )));
    }
    Ok(normalized)
}

fn is_valid_profile_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() || first.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && name.chars().count() <= 64
}

/// Directorio base de Hermes: `HERMES_HOME` o `~/.hermes`. No toca el disco.
pub fn hermes_base() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HERMES_HOME").filter(|h| !h.is_empty()) {
        return Some(PathBuf::from(home));
    }
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".hermes"))
}

/// Descubre perfiles bajo `base`: `default` más cada subdirectorio de
/// `<base>/profiles` con nombre válido. Ordenados alfabéticamente tras `default`.
/// Función pura respecto al entorno: solo lista el directorio que recibe.
pub fn discover_profiles_in(base: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(base.join("profiles"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| is_valid_profile_name(n))
        .filter(|n| n != "default")
        .collect();
    names.sort();
    names.dedup();
    let mut out = vec!["default".to_string()];
    out.extend(names);
    out
}

/// Interpreta una lista separada por comas (normaliza, quita vacíos y repetidos).
pub fn parse_hidden_list(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in raw.split(',') {
        let item = item.trim().to_lowercase();
        if !item.is_empty() && !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

/// Reglas de perfiles: dónde descubrirlos y cuáles están ocultos. Los clones
/// comparten la lista de ocultos, de modo que un cambio en caliente llega al
/// agente y al orquestador a la vez.
#[derive(Debug, Clone, Default)]
pub struct ProfileRules {
    /// Base de Hermes; `None` => `HERMES_HOME` o `~/.hermes`.
    base: Option<PathBuf>,
    /// Ocultos de los ajustes (se suman a los de `FORJA_HIDDEN_PROFILES`).
    hidden: Arc<RwLock<Vec<String>>>,
}

impl ProfileRules {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fija la base de Hermes (tests y despliegues con `HERMES_HOME` propio).
    pub fn with_base(mut self, base: impl Into<PathBuf>) -> Self {
        self.base = Some(base.into());
        self
    }

    /// Reemplaza la lista de ocultos configurada (los ajustes).
    pub fn set_hidden(&self, hidden: &[String]) {
        let mut g = self.hidden.write().unwrap_or_else(|e| e.into_inner());
        *g = hidden.iter().map(|h| h.trim().to_lowercase()).collect();
    }

    /// Ocultos efectivos: ajustes ∪ `FORJA_HIDDEN_PROFILES`.
    pub fn hidden(&self) -> Vec<String> {
        let env = std::env::var(HIDDEN_PROFILES_ENV).unwrap_or_default();
        self.hidden_with_env(&env)
    }

    /// Como [`Self::hidden`] pero con el valor del entorno inyectado.
    pub fn hidden_with_env(&self, env: &str) -> Vec<String> {
        let mut out = self
            .hidden
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for h in parse_hidden_list(env) {
            if !out.contains(&h) {
                out.push(h);
            }
        }
        out
    }

    /// Todos los perfiles que existen (incluidos los ocultos).
    pub fn discover_all(&self) -> Vec<String> {
        match self.base.clone().or_else(hermes_base) {
            Some(base) => discover_profiles_in(&base),
            None => vec!["default".to_string()],
        }
    }

    /// Perfiles elegibles: descubiertos menos los ocultos.
    pub fn discover(&self) -> Vec<String> {
        let hidden = self.hidden();
        self.discover_all()
            .into_iter()
            .filter(|p| !hidden.contains(p))
            .collect()
    }

    /// Valida un perfil: formato, no oculto y existente.
    pub fn validate(&self, profile: &str) -> Result<String> {
        self.validate_with_extra_hidden(profile, &[])
    }

    /// Igual que [`Self::validate`] sumando ocultos propuestos (ajustes que aún
    /// no se aplicaron).
    pub fn validate_with_extra_hidden(&self, profile: &str, extra: &[String]) -> Result<String> {
        let mut hidden = self.hidden();
        hidden.extend(extra.iter().map(|h| h.trim().to_lowercase()));
        validate_profile_with(profile, &self.discover_all(), &hidden)
    }
}

/// Valida un perfil con las reglas por defecto (entorno real). Para tests y
/// código con reglas propias usa [`ProfileRules`] o [`validate_profile_with`].
pub fn validate_profile(profile: &str) -> Result<String> {
    ProfileRules::new().validate(profile)
}

/// Parseo tolerante de la salida de hermes: nunca hace panic.
///
/// Con `-Q --oneshot` hermes escribe SOLO la respuesta final en stdout y
/// deja el `session_id` en stderr (contrato del modo quieto: la respuesta va
/// a stdout y el identificador de sesión a stderr), así que stdout ya es la
/// respuesta. Si está vacía se devuelve texto vacío con un aviso en
/// `warnings`; si no, el texto crudo (recortado) es la respuesta, sin
/// advertencias espurias por respuestas que no sean JSON.
pub fn parse_hermes_output(stdout: &str, stderr: &str, exit_code: Option<i32>) -> AgentOutput {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return AgentOutput {
            text: String::new(),
            raw_stdout: stdout.to_string(),
            raw_stderr: stderr.to_string(),
            exit_code,
            warnings: vec!["salida vacía de hermes (revisa raw_stderr)".to_string()],
        };
    }

    AgentOutput {
        text: trimmed.to_string(),
        raw_stdout: stdout.to_string(),
        raw_stderr: stderr.to_string(),
        exit_code,
        warnings: Vec::new(),
    }
}

/// Adaptador real de Hermes: ejecuta el CLI con el prompt en un archivo
/// temporal y captura su salida con timeout.
#[derive(Debug, Clone)]
pub struct HermesAgent {
    /// Perfil de hermes (`-p`); se valida (existe y no está oculto) en cada ejecución.
    pub profile: String,
    /// Reglas de descubrimiento y ocultos.
    pub rules: ProfileRules,
    /// Programa que se ejecuta (por defecto "hermes"); los tests lo apuntan
    /// a un script falso.
    pub program: String,
}

impl Default for HermesAgent {
    fn default() -> Self {
        Self::new(DEFAULT_PROFILE)
    }
}

impl HermesAgent {
    pub fn new(profile: impl Into<String>) -> Self {
        Self {
            profile: profile.into(),
            program: "hermes".to_string(),
            rules: ProfileRules::new(),
        }
    }

    /// Usa reglas de perfiles propias (ocultos compartidos, base de Hermes).
    pub fn with_rules(mut self, rules: ProfileRules) -> Self {
        self.rules = rules;
        self
    }

    /// Cambia el programa a ejecutar (p. ej. un script falso en tests).
    pub fn with_program(mut self, program: impl Into<String>) -> Self {
        self.program = program.into();
        self
    }

    /// Ejecuta el CLI con el prompt ya escrito en `prompt_path`.
    async fn execute(
        &self,
        profile: &str,
        prompt_path: &Path,
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
        let opts = HermesCommandOptions {
            profile: profile.to_string(),
            query_file: prompt_path.to_path_buf(),
        };
        let mut argv = build_command(&opts);
        argv[0] = self.program.clone();

        // cwd del proceso = worktree de la tarea; argv directo, sin shell.
        let res = match Runner::run_argv(&argv, &ctx.worktree, ctx.timeout_secs).await {
            Ok(res) => res,
            // Programa ausente: mensaje claro en español en lugar del io::Error crudo.
            Err(ForjaError::Io(err)) if err.kind() == ErrorKind::NotFound => {
                return Err(ForjaError::Agent(format!(
                    "no se encontró el programa \"{}\" \
                     (¿está instalado y en el PATH?); error original: {err}",
                    self.program
                )));
            }
            Err(other) => return Err(other),
        };

        Ok(parse_hermes_output(&res.stdout, &res.stderr, res.exit_code))
    }
}

/// Borra el archivo del prompt al soltar el guard. Se crea ANTES de escribir
/// el archivo y cubre cualquier cancelación del futuro a mitad de la
/// ejecución (`Scheduler::cancel_run`), incluida la propia escritura, en la
/// que el flujo normal de `run` nunca llegaría a su paso de limpieza.
struct PromptFileGuard(PathBuf);

impl Drop for PromptFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl Agent for HermesAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Hermes
    }

    fn run<'a>(&'a self, ctx: &'a RunContext) -> AgentFuture<'a> {
        Box::pin(async move {
            // 1. Validar el perfil ANTES de crear archivos o procesos: debe
            //    existir y no estar oculto.
            let profile = self.rules.validate(&self.profile)?;

            // 2. Prompt en un archivo temporal único (uuid v4) en temp_dir().
            let prompt_path =
                std::env::temp_dir().join(format!("forja-hermes-prompt-{}.txt", Uuid::new_v4()));

            // Red de seguridad ANTES de crear el archivo: si el futuro se
            // cancela en cualquier punto (cancel_run), el guard borra el
            // temporal al soltar el futuro, incluso a mitad de la escritura.
            let _guard = PromptFileGuard(prompt_path.clone());

            if let Err(write_err) = tokio::fs::write(&prompt_path, ctx.task.prompt.as_bytes()).await
            {
                // El archivo puede haber quedado creado a medias: limpieza best-effort.
                let _ = tokio::fs::remove_file(&prompt_path).await;
                return Err(write_err.into());
            }

            // 3. Ejecutar (cwd = worktree) con timeout y captura de salida.
            let mut executed = self.execute(&profile, &prompt_path, ctx).await;

            // 4. Borrar el archivo temporal SIEMPRE, incluso si hubo error.
            if let Err(remove_err) = tokio::fs::remove_file(&prompt_path).await {
                if let Ok(out) = executed.as_mut() {
                    out.warnings.push(format!(
                        "no se pudo borrar el archivo temporal {}: {remove_err}",
                        prompt_path.display()
                    ));
                }
            }

            executed
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use forja_core::{AgentKind, ForjaError, Privacy, Task};
    use tempfile::tempdir;

    use super::*;

    fn fake_hermes_path() -> String {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fake_hermes.sh")
            .to_string_lossy()
            .into_owned()
    }

    /// Base de Hermes falsa con los perfiles dados (nunca toca el ~/.hermes real).
    fn fake_home(profiles: &[&str]) -> (tempfile::TempDir, ProfileRules) {
        let dir = tempdir().expect("tempdir");
        for p in profiles {
            std::fs::create_dir_all(dir.path().join("profiles").join(p)).expect("perfil falso");
        }
        let rules = ProfileRules::new().with_base(dir.path());
        (dir, rules)
    }

    fn agent_with(profile: &str, rules: &ProfileRules) -> HermesAgent {
        HermesAgent::new(profile)
            .with_program(fake_hermes_path())
            .with_rules(rules.clone())
    }

    fn ctx_for(prompt: &str, worktree: &Path, timeout_secs: u64) -> RunContext {
        RunContext {
            task: Task::new("Tarea Hermes", prompt, Privacy::Open),
            worktree: worktree.to_path_buf(),
            timeout_secs,
        }
    }

    /// Extrae de stderr la ruta del archivo de prompt que vio el script
    /// falso (línea "query_file: <ruta>"), para verificar que Forja la borró.
    fn query_file_from(stderr: &str) -> PathBuf {
        let line = stderr
            .lines()
            .find(|l| l.starts_with("query_file: "))
            .expect("fake_hermes debe informar el archivo de prompt en stderr");
        PathBuf::from(line.trim_start_matches("query_file: "))
    }

    #[test]
    fn hermes_agent_keeps_profile_and_program_defaults() {
        let agent = HermesAgent::default();
        assert_eq!(agent.profile, DEFAULT_PROFILE);
        assert_eq!(agent.profile, "default");
        assert_eq!(agent.program, "hermes");
        assert_eq!(HermesAgent::new("otro-perfil").profile, "otro-perfil");
        assert_eq!(
            HermesAgent::new("yuuki")
                .with_program("script-falso")
                .program,
            "script-falso"
        );
    }

    #[test]
    fn hermes_build_command_exact_shape_with_spaces_and_quotes() {
        let opts = HermesCommandOptions::new("yuuki", "/tmp/mi dir/consulta 'rara' \"x\".txt");
        assert_eq!(
            build_command(&opts),
            vec![
                "hermes",
                "-p",
                "yuuki",
                "chat",
                "--query-file",
                "/tmp/mi dir/consulta 'rara' \"x\".txt",
                "-Q",
                "--oneshot",
            ]
        );
    }

    #[test]
    fn hermes_build_command_replaces_only_the_program() {
        // El argv que arma el agente es idéntico salvo argv[0], que apunta
        // al programa configurable (script falso en tests).
        let opts = HermesCommandOptions::new("claudia", "/tmp/consulta.txt");
        let mut argv = build_command(&opts);
        assert_eq!(argv[0], "hermes");
        argv[0] = "otro-programa".to_string();
        assert_eq!(argv[0], "otro-programa");
        assert_eq!(argv[1], "-p");
        assert_eq!(argv[2], "claudia");
        assert_eq!(argv[4], "--query-file");
        assert_eq!(argv[5], "/tmp/consulta.txt");
        assert_eq!(argv[6], "-Q");
        assert_eq!(argv[7], "--oneshot");
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn hermes_validate_profile_accepts_discovered_and_normalizes() {
        let available = names(&["default", "claudia", "yuuki"]);
        assert_eq!(
            validate_profile_with("  Yuuki ", &available, &[]).expect("normaliza"),
            "yuuki"
        );
        assert_eq!(
            validate_profile_with("default", &available, &[]).expect("default"),
            "default"
        );
    }

    #[test]
    fn hermes_validate_profile_rejects_unknown_and_hidden() {
        let available = names(&["default", "claudia", "secreto"]);
        let hidden = names(&["secreto"]);
        for (profile, needle) in [("SECRETO", "oculto"), ("trabajo", "no existe")] {
            let err = validate_profile_with(profile, &available, &hidden)
                .expect_err("oculto o inexistente");
            match err {
                ForjaError::InvalidInput(msg) => {
                    assert!(msg.contains("no permitido"), "mensaje poco claro: {msg}");
                    assert!(msg.contains(needle), "esperaba {needle:?}: {msg}");
                    assert!(
                        msg.contains(profile.to_lowercase().as_str()),
                        "debe nombrar el perfil rechazado: {msg}"
                    );
                }
                other => panic!("esperaba InvalidInput para {profile}, obtuve {other:?}"),
            }
        }
    }

    #[test]
    fn hermes_discover_profiles_lists_default_plus_valid_subdirs() {
        let dir = tempdir().expect("tempdir");
        let profiles = dir.path().join("profiles");
        for d in ["zeta", "alfa", "Mayus", "-malo", "con espacio", "default"] {
            std::fs::create_dir_all(profiles.join(d)).expect("dir");
        }
        // Un archivo suelto no es un perfil.
        std::fs::write(profiles.join("archivo"), "x").expect("archivo");
        assert_eq!(
            discover_profiles_in(dir.path()),
            names(&["default", "alfa", "zeta"])
        );
        // Sin directorio profiles: solo default.
        let vacio = tempdir().expect("tempdir");
        assert_eq!(discover_profiles_in(vacio.path()), names(&["default"]));
    }

    #[test]
    fn hermes_parse_hidden_list_normalizes_and_dedups() {
        assert_eq!(parse_hidden_list(" A, b ,,a,C "), names(&["a", "b", "c"]));
        assert!(parse_hidden_list("").is_empty());
        assert!(parse_hidden_list(" , ").is_empty());
    }

    #[test]
    fn hermes_rules_hide_profiles_from_discovery_and_validation() {
        let (_dir, rules) = fake_home(&["claudia", "privado"]);
        assert_eq!(rules.discover(), names(&["default", "claudia", "privado"]));
        rules.set_hidden(&names(&["Privado"]));
        assert_eq!(rules.hidden_with_env(""), names(&["privado"]));
        assert_eq!(rules.hidden_with_env("otro"), names(&["privado", "otro"]));
        let visibles = rules.discover();
        assert!(!visibles.contains(&"privado".to_string()));
        assert_eq!(rules.discover_all().len(), 3);
        assert!(rules.validate("privado").is_err());
        assert!(rules.validate("claudia").is_ok());
        // Un oculto propuesto (ajustes aún sin aplicar) también se rechaza.
        assert!(rules
            .validate_with_extra_hidden("claudia", &names(&["claudia"]))
            .is_err());
        // Los clones comparten la lista de ocultos.
        let otro = rules.clone();
        otro.set_hidden(&[]);
        assert!(rules.validate("privado").is_ok());
    }

    #[test]
    fn hermes_validate_profile_rejects_invalid_names() {
        let largo = "x".repeat(65);
        for profile in [
            "",
            "   ",
            "../yuuki",
            "yu uki",
            "-yuuki",
            "_yuuki",
            "yu/ki",
            "yuuki;rm",
            largo.as_str(),
        ] {
            let err = validate_profile(profile).expect_err("nombre inválido");
            match err {
                ForjaError::InvalidInput(msg) => {
                    assert!(
                        msg.contains("inválido"),
                        "mensaje poco claro para {profile:?}: {msg}"
                    );
                }
                other => panic!("esperaba InvalidInput, obtuve {other:?}"),
            }
        }
    }

    #[test]
    fn hermes_parse_output_text_and_empty_cases() {
        let out = parse_hermes_output("respuesta final\n", "session_id: abc\n", Some(0));
        assert_eq!(out.text, "respuesta final");
        assert_eq!(out.raw_stderr, "session_id: abc\n");
        assert!(out.warnings.is_empty());

        let out = parse_hermes_output("   \n\t ", "session_id: abc\n", Some(0));
        assert!(out.text.is_empty());
        assert_eq!(out.exit_code, Some(0));
        assert!(
            out.warnings.iter().any(|w| w.contains("salida vacía")),
            "warnings inesperados: {:?}",
            out.warnings
        );
    }

    #[tokio::test]
    async fn hermes_success_delivers_prompt_with_quotes_and_newlines_verbatim() {
        let dir = tempdir().expect("tempdir");
        let prompt = "Di \"hola\" 'mundo' sin escapar\nlinea 2: $(whoami)\nlinea 3: `ls -la`";
        let ctx = ctx_for(prompt, dir.path(), 15);
        let (_home, rules) = fake_home(&[]);

        let agent = agent_with("default", &rules);
        assert_eq!(agent.kind(), AgentKind::Hermes);

        let out = agent
            .run(&ctx)
            .await
            .expect("fake_hermes debe responder ok");

        // El prompt con comillas, saltos de línea y metacaracteres de shell
        // llega íntegro por el archivo temporal, nunca por una shell.
        assert_eq!(out.text, prompt);
        assert_eq!(out.exit_code, Some(0));
        assert!(out.warnings.is_empty(), "warnings: {:?}", out.warnings);

        // El proceso corrió con cwd = worktree de la tarea.
        let worktree_name = dir
            .path()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        assert!(
            out.raw_stderr.contains(&worktree_name),
            "esperaba el worktree {worktree_name} en stderr: {}",
            out.raw_stderr
        );

        // El archivo temporal se borró al terminar.
        let query_file = query_file_from(&out.raw_stderr);
        assert!(
            !query_file.exists(),
            "el archivo temporal {query_file:?} debe borrarse tras la ejecución"
        );
    }

    #[tokio::test]
    async fn hermes_nonzero_exit_code_fails_with_clear_error_and_cleans_up() {
        let dir = tempdir().expect("tempdir");
        let ctx = ctx_for("FAKE:fail con detalle", dir.path(), 15);
        let (_home, rules) = fake_home(&[]);
        let agent = agent_with("default", &rules);

        let err = agent
            .run(&ctx)
            .await
            .expect_err("fake_hermes con código 1 debe fallar");

        match err {
            ForjaError::Agent(msg) => {
                assert!(msg.contains("1"), "esperaba el código 1 en: {msg}");
                assert!(
                    msg.contains("fallo simulado"),
                    "esperaba el stderr del proceso en: {msg}"
                );
                // El archivo temporal también se borra cuando el comando falla.
                let idx = msg
                    .find("query_file: ")
                    .expect("el error debe incluir la ruta del prompt vista por el proceso");
                let after = &msg[idx + "query_file: ".len()..];
                let query_file = PathBuf::from(after.lines().next().unwrap_or_default());
                assert!(
                    !query_file.exists(),
                    "el archivo temporal {query_file:?} debe borrarse aunque el comando falle"
                );
            }
            other => panic!("esperaba ForjaError::Agent, obtuve {other:?}"),
        }
    }

    #[tokio::test]
    async fn hermes_timeout_expires_and_kills_the_process() {
        let dir = tempdir().expect("tempdir");
        let ctx = ctx_for("FAKE:timeout", dir.path(), 1);
        let (_home, rules) = fake_home(&[]);
        let agent = agent_with("default", &rules);

        let err = agent
            .run(&ctx)
            .await
            .expect_err("fake_hermes dormido debe agotar el timeout");

        assert!(
            matches!(err, ForjaError::Timeout { seconds: 1 }),
            "esperaba Timeout {{seconds: 1}}, obtuve {err:?}"
        );
    }

    #[tokio::test]
    async fn hermes_empty_output_returns_warning_and_cleans_up() {
        let dir = tempdir().expect("tempdir");
        let ctx = ctx_for("FAKE:empty", dir.path(), 15);
        let (_home, rules) = fake_home(&[]);
        let agent = agent_with("default", &rules);

        let out = agent
            .run(&ctx)
            .await
            .expect("salida vacía con código 0 no es un error del proceso");

        assert!(out.text.is_empty());
        assert_eq!(out.exit_code, Some(0));
        assert!(
            out.warnings.iter().any(|w| w.contains("salida vacía")),
            "la salida vacía debe producir un aviso; warnings: {:?}",
            out.warnings
        );

        let query_file = query_file_from(&out.raw_stderr);
        assert!(
            !query_file.exists(),
            "el archivo temporal {query_file:?} debe borrarse tras la ejecución"
        );
    }

    #[tokio::test]
    async fn hermes_missing_program_fails_with_clear_spanish_error() {
        let dir = tempdir().expect("tempdir");
        let ctx = ctx_for("hola", dir.path(), 5);
        let agent = HermesAgent::default().with_program("hermes-falso-que-no-existe-en-el-path");

        let err = agent
            .run(&ctx)
            .await
            .expect_err("un programa inexistente debe fallar");

        match err {
            ForjaError::Agent(msg) => {
                assert!(
                    msg.contains("no se encontró el programa"),
                    "mensaje poco claro: {msg}"
                );
                assert!(
                    msg.contains("hermes-falso-que-no-existe-en-el-path"),
                    "debe nombrar el programa ausente: {msg}"
                );
                assert!(msg.contains("PATH"), "debe sugerir revisar el PATH: {msg}");
            }
            other => panic!("esperaba ForjaError::Agent, obtuve {other:?}"),
        }
    }

    #[tokio::test]
    async fn hermes_rejects_hidden_and_unknown_profiles_before_running_anything() {
        let dir = tempdir().expect("tempdir");
        let ctx = ctx_for("hola", dir.path(), 15);
        let (_home, rules) = fake_home(&["privado", "visible"]);
        rules.set_hidden(&names(&["privado"]));

        for (profile, needle) in [("privado", "oculto"), ("inexistente", "no existe")] {
            let agent = agent_with(profile, &rules);
            let err = agent
                .run(&ctx)
                .await
                .expect_err("perfil oculto o inexistente");
            match err {
                ForjaError::InvalidInput(msg) => {
                    assert!(msg.contains("no permitido"), "mensaje poco claro: {msg}");
                    assert!(msg.contains(needle), "esperaba {needle:?}: {msg}");
                    assert!(msg.contains(profile), "debe nombrar el perfil: {msg}");
                    // Se rechazó ANTES de ejecutar nada: nada del proceso en el error.
                    assert!(
                        !msg.contains("query_file"),
                        "no debió ejecutarse nada: {msg}"
                    );
                }
                other => panic!("esperaba InvalidInput para {profile}, obtuve {other:?}"),
            }
        }

        // Un perfil descubierto y visible sí corre.
        let out = agent_with("visible", &rules)
            .run(&ctx)
            .await
            .expect("perfil visible");
        assert_eq!(out.text, "hola");
    }
}
