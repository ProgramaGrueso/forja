//! Rutas protegidas: qué carpetas se pueden abrir como workspace y dónde no
//! escribe la API de archivos. Todo se decide sobre rutas **canónicas** (los
//! symlinks ya resueltos hasta su destino final).

use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use forja_core::{ForjaError, Result};

/// Carpetas del sistema que no se abren como workspace (ni nada dentro).
const SYSTEM_DIRS: &[&str] = &["/etc", "/proc", "/sys", "/dev", "/boot", "/root", "/run"];

/// Tope de lectura de un archivo `.git` o `commondir` (son una sola línea).
const MAX_POINTER_BYTES: u64 = 8 * 1024;

fn name_is(part: &OsStr, name: &str) -> bool {
    part.to_str().is_some_and(|p| p.eq_ignore_ascii_case(name))
}

/// `true` si algún componente de la ruta es `name`, sin distinguir mayúsculas.
fn has_component(path: &Path, name: &str) -> bool {
    path.components()
        .any(|c| matches!(c, Component::Normal(part) if name_is(part, name)))
}

/// Estructura de directorio git, se llame como se llame (`--separate-git-dir`,
/// repos *bare*, directorio de un worktree).
fn looks_like_git_dir(dir: &Path) -> bool {
    dir.join("HEAD").is_file()
        && (dir.join("commondir").is_file()
            || (dir.join("objects").is_dir() && dir.join("refs").is_dir()))
}

fn read_pointer(file: &Path) -> Option<String> {
    let mut text = String::new();
    fs::File::open(file)
        .ok()?
        .take(MAX_POINTER_BYTES)
        .read_to_string(&mut text)
        .ok()?;
    Some(text.lines().next()?.trim().to_string())
}

/// Directorios git reales que declara `dir/.git`: el propio `.git` (aunque sea
/// un symlink) o, si es un archivo, el `gitdir:` al que apunta y su `commondir`.
fn declared_git_dirs(dir: &Path) -> Vec<PathBuf> {
    let dotgit = dir.join(".git");
    let Ok(meta) = fs::metadata(&dotgit) else {
        return Vec::new();
    };
    if meta.is_dir() {
        return dotgit.canonicalize().into_iter().collect();
    }
    if !meta.is_file() {
        return Vec::new();
    }
    let Some(gitdir) = read_pointer(&dotgit)
        .and_then(|line| line.strip_prefix("gitdir:").map(|p| p.trim().to_string()))
        .and_then(|p| dir.join(p).canonicalize().ok())
    else {
        return Vec::new();
    };
    let common =
        read_pointer(&gitdir.join("commondir")).and_then(|p| gitdir.join(p).canonicalize().ok());
    let mut dirs = vec![gitdir];
    dirs.extend(common);
    dirs
}

/// `true` si la ruta canónica es un directorio git o está dentro de uno.
pub(crate) fn inside_git_dir(canon: &Path) -> bool {
    has_component(canon, ".git")
        || canon.ancestors().any(|dir| {
            looks_like_git_dir(dir)
                || declared_git_dirs(dir)
                    .iter()
                    .any(|git| canon.starts_with(git))
        })
}

/// `$HOME` canónico, si está definido.
pub(crate) fn home_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    let home = PathBuf::from(home);
    Some(home.canonicalize().unwrap_or(home))
}

/// Decide si la carpeta canónica `canon` se puede abrir como workspace.
pub(crate) fn check_workspace_dir(canon: &Path, home: Option<&Path>) -> Result<()> {
    let deny = |reason: &str| {
        Err(ForjaError::InvalidInput(format!(
            "carpeta no permitida '{}': {reason}. Elige la carpeta de un proyecto",
            canon.display()
        )))
    };
    if canon.parent().is_none() {
        return deny("es la raíz del sistema");
    }
    // Los discos extraíbles se montan en /run/media/<usuario>/<disco>.
    let removable = canon.starts_with("/run/media");
    let system = SYSTEM_DIRS.iter().map(Path::new).any(|dir| {
        canon.starts_with(dir) || dir.canonicalize().is_ok_and(|real| canon.starts_with(real))
    });
    if system && !removable {
        return deny("es una carpeta del sistema (/etc, /proc, /sys, /dev, /boot, /root, /run)");
    }
    if let Some(home) = home {
        if canon == home {
            return deny("es tu carpeta personal completa");
        }
        if home.starts_with(canon) {
            return deny("contiene tu carpeta personal");
        }
        if let Ok(rest) = canon.strip_prefix(home) {
            if rest
                .components()
                .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
            {
                return deny("es una carpeta oculta de tu carpeta personal (.ssh, .config…)");
            }
        }
    }
    if inside_git_dir(canon) {
        return deny("está dentro de un directorio git (.git)");
    }
    if has_component(canon, ".forja") {
        return deny("es una carpeta interna de Forja (.forja)");
    }
    Ok(())
}

/// Datos propios de Forja que la API de archivos nunca escribe, se llamen como
/// se llamen sus carpetas (`--store-dir`).
#[derive(Debug, Clone)]
pub(crate) struct ForjaData {
    pub store_dir: PathBuf,
    pub settings_path: Option<PathBuf>,
}

/// Canónica aunque el último componente todavía no exista.
fn canonical_maybe_new(path: &Path) -> Option<PathBuf> {
    path.canonicalize().ok().or_else(|| {
        let parent = path.parent()?.canonicalize().ok()?;
        Some(parent.join(path.file_name()?))
    })
}

/// Comprueba el destino canónico de una escritura dentro del workspace `root`.
pub(crate) fn check_write_target(root: &Path, target: &Path, data: &ForjaData) -> Result<()> {
    if inside_git_dir(target) {
        return Err(ForjaError::InvalidInput(
            "no se permite escribir dentro de .git (ni en el directorio git real del repositorio)"
                .to_string(),
        ));
    }
    let relative = target.strip_prefix(root).unwrap_or(target);
    // Si el workspace contiene al store entero (`--store-dir .`) solo se protege
    // settings.json; si no, todo lo que haya en el store.
    let in_store = data
        .store_dir
        .canonicalize()
        .is_ok_and(|store| !root.starts_with(&store) && target.starts_with(&store));
    let is_settings = data
        .settings_path
        .as_deref()
        .and_then(canonical_maybe_new)
        .is_some_and(|settings| settings == target);
    if has_component(relative, ".forja") || in_store || is_settings {
        return Err(ForjaError::InvalidInput(
            "no se permite escribir en .forja ni en los datos internos de Forja".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_rules_on_canonical_paths() {
        let tmp = tempfile::tempdir().expect("tmp");
        let base = tmp.path().canonicalize().expect("canon");
        let home = base.join("users/ana");
        for dir in ["projects/x", ".ssh", "projects/.oculta/y"] {
            fs::create_dir_all(home.join(dir)).expect("mkdir");
        }
        fs::create_dir_all(base.join("repo/.GIT/hooks")).expect("mkdir");
        fs::create_dir_all(base.join("repo/.Forja/store")).expect("mkdir");
        fs::create_dir_all(base.join("suelto/objects")).expect("mkdir");
        fs::create_dir_all(base.join("suelto/refs")).expect("mkdir");
        fs::write(base.join("suelto/HEAD"), "ref: refs/heads/main\n").expect("HEAD");

        let check = |p: &Path| check_workspace_dir(p, Some(&home));
        for bad in [
            PathBuf::from("/"),
            home.clone(),
            base.join("users"),
            base.clone(),
            home.join(".ssh"),
            home.join("projects/.oculta/y"),
            base.join("repo/.GIT"),
            base.join("repo/.GIT/hooks"),
            base.join("repo/.Forja/store"),
            base.join("suelto"),
            base.join("suelto/refs"),
            PathBuf::from("/etc"),
            PathBuf::from("/etc/ssl"),
            PathBuf::from("/proc/self"),
            PathBuf::from("/sys"),
            PathBuf::from("/dev/shm"),
            PathBuf::from("/boot"),
            PathBuf::from("/root"),
            PathBuf::from("/run/user"),
        ] {
            assert!(check(&bad).is_err(), "debería rechazarse: {bad:?}");
        }
        for good in [
            home.join("projects"),
            home.join("projects/x"),
            base.join("repo"),
            PathBuf::from("/mnt/datos/proyecto"),
            PathBuf::from("/tmp/proyecto"),
            PathBuf::from("/etcetera"),
            PathBuf::from("/run/media/ana/disco/proyecto"),
        ] {
            assert!(check(&good).is_ok(), "debería permitirse: {good:?}");
        }
    }

    #[test]
    fn git_dirs_are_detected_through_pointer_files_and_links() {
        let tmp = tempfile::tempdir().expect("tmp");
        let base = tmp.path().canonicalize().expect("canon");
        // `.git` como archivo (worktree/submódulo) con gitdir y commondir que no
        // se llaman `.git` ni tienen estructura completa.
        let wt = base.join("wt");
        fs::create_dir_all(&wt).expect("wt");
        fs::create_dir_all(wt.join("meta")).expect("meta");
        fs::create_dir_all(wt.join("comun")).expect("comun");
        fs::write(wt.join(".git"), "gitdir: meta\n").expect(".git");
        fs::write(wt.join("meta/commondir"), "../comun\n").expect("commondir");
        assert!(inside_git_dir(&wt.join(".git")));
        assert!(inside_git_dir(&wt.join("meta/config")));
        assert!(inside_git_dir(&wt.join("comun/config")));
        assert!(!inside_git_dir(&wt.join("src.rs")));

        // `.git` como symlink a una carpeta con otro nombre.
        let repo = base.join("repo");
        fs::create_dir_all(repo.join("real")).expect("real");
        std::os::unix::fs::symlink("real", repo.join(".git")).expect("link");
        assert!(inside_git_dir(&repo.join("real/config")));
        assert!(!inside_git_dir(&repo.join("README.md")));
    }

    #[test]
    fn forja_data_is_protected_even_with_a_custom_store_name() {
        let tmp = tempfile::tempdir().expect("tmp");
        let root = tmp.path().canonicalize().expect("canon");
        fs::create_dir_all(root.join("datos")).expect("datos");
        let data = ForjaData {
            store_dir: root.join("datos"),
            settings_path: Some(root.join("datos/settings.json")),
        };
        let check = |rel: &str| check_write_target(&root, &root.join(rel), &data);
        assert!(check("datos/settings.json").is_err());
        assert!(check("datos/runs.jsonl").is_err());
        assert!(check(".forja/store/settings.json").is_err());
        assert!(check("sub/.FORJA/x").is_err());
        assert!(check(".Git/config").is_err());
        assert!(check("src/main.rs").is_ok());
        assert!(check(".gitignore").is_ok());

        // Store en la propia raíz: solo se protege settings.json.
        let flat = ForjaData {
            store_dir: root.clone(),
            settings_path: Some(root.join("settings.json")),
        };
        assert!(check_write_target(&root, &root.join("settings.json"), &flat).is_err());
        assert!(check_write_target(&root, &root.join("a.txt"), &flat).is_ok());
    }
}
