use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{Query, State};
use axum::Json;
use forja_core::{ForjaError, Result};
use serde::{Deserialize, Serialize};

use crate::guard::{check_write_target, ForjaData};
use crate::{ApiError, AppState};

pub const MAX_FILE_BYTES: usize = 512 * 1024;
const MAX_SEARCH_DEPTH: usize = 8;
const MAX_SEARCH_RESULTS: usize = 60;
/// Tope de entradas (archivos y carpetas) que recorre una búsqueda.
const MAX_SEARCH_ENTRIES: usize = 50_000;
/// Tope de tiempo de una búsqueda.
const MAX_SEARCH_TIME: Duration = Duration::from_secs(2);
const IGNORED_NAMES: &[&str] = &[".git", "target", "node_modules", ".forja"];

#[derive(Debug, Deserialize)]
pub struct TreeQuery {
    #[serde(default)]
    pub path: String,
    pub run_id: Option<String>,
    #[serde(default)]
    pub show_hidden: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileTreeEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileTreeResponse {
    pub root: String,
    pub path: String,
    pub read_only: bool,
    pub entries: Vec<FileTreeEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ReadQuery {
    pub path: String,
    pub run_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileReadResponse {
    pub path: String,
    pub content: String,
    pub size: usize,
    pub read_only: bool,
    pub language: String,
    /// Identificador de la carpeta de la que se leyó; guardar exige devolverlo.
    pub workspace: String,
}

#[derive(Debug, Deserialize)]
pub struct WriteFileRequest {
    pub path: String,
    pub content: String,
    /// El `workspace` que devolvió la lectura. Obligatorio.
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileWriteResponse {
    pub ok: bool,
    pub path: String,
    pub size: usize,
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    pub q: String,
    pub run_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSearchMatch {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSearchResponse {
    pub query: String,
    pub matches: Vec<FileSearchMatch>,
    /// La búsqueda paró por un límite: puede haber más coincidencias.
    pub truncated: bool,
}

async fn resolve_root(state: &AppState, run_id: Option<&str>) -> Result<(PathBuf, bool)> {
    let run_id = run_id.map(str::trim).filter(|s| !s.is_empty());
    match run_id {
        None => {
            let root = state.workspaces.active_root();
            let canon = root.canonicalize().map_err(|err| {
                ForjaError::InvalidInput(format!("workspace activo inaccesible: {err}"))
            })?;
            state.workspaces.check_allowed(&canon)?;
            Ok((canon, false))
        }
        Some(id) => {
            if !forja_sched::is_valid_run_id(id) {
                return Err(ForjaError::NotFound(id.to_string()));
            }
            let run = state
                .scheduler
                .get_run(id)
                .await
                .ok_or_else(|| ForjaError::NotFound(id.to_string()))?;
            let wt_raw = run.worktree.as_deref().ok_or_else(|| {
                ForjaError::NotFound(format!("la ejecución '{id}' no tiene worktree"))
            })?;
            let canon_wt = PathBuf::from(wt_raw)
                .canonicalize()
                .map_err(|_| ForjaError::NotFound(format!("el worktree de '{id}' no existe")))?;
            let canon_base = state
                .scheduler
                .worktrees_dir()
                .canonicalize()
                .map_err(|_| {
                    ForjaError::NotConfigured("la carpeta de worktrees no existe".to_string())
                })?;
            if !canon_wt.starts_with(&canon_base) {
                return Err(ForjaError::InvalidInput(
                    "el worktree registrado está fuera de la carpeta de worktrees".to_string(),
                ));
            }
            Ok((canon_wt, true))
        }
    }
}

fn validate_relative(rel: &str) -> Result<PathBuf> {
    if rel.starts_with('/') || rel.starts_with('\\') || rel.contains('\0') {
        return Err(ForjaError::InvalidInput(
            "ruta relativa inválida".to_string(),
        ));
    }
    let mut clean = PathBuf::new();
    for comp in Path::new(rel).components() {
        match comp {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(ForjaError::InvalidInput(
                    "no se permiten rutas absolutas ni '..'".to_string(),
                ));
            }
        }
    }
    Ok(clean)
}

fn resolve_safe_existing(root: &Path, rel: &str) -> Result<PathBuf> {
    let trimmed = rel.trim();
    if trimmed.is_empty() {
        return Ok(root.to_path_buf());
    }
    let clean = validate_relative(trimmed)?;
    let candidate = root.join(&clean);
    let canon = candidate.canonicalize().map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            ForjaError::NotFound(format!("ruta no encontrada: {trimmed}"))
        } else {
            ForjaError::InvalidInput(format!("ruta inaccesible '{trimmed}': {err}"))
        }
    })?;
    if !canon.starts_with(root) {
        return Err(ForjaError::InvalidInput(
            "acceso fuera del workspace denegado".to_string(),
        ));
    }
    Ok(canon)
}

fn resolve_safe_write(root: &Path, rel: &str, data: &ForjaData) -> Result<PathBuf> {
    let trimmed = rel.trim();
    if trimmed.is_empty() {
        return Err(ForjaError::InvalidInput(
            "la ruta del archivo no puede estar vacía".to_string(),
        ));
    }
    let clean = validate_relative(trimmed)?;
    if clean.as_os_str().is_empty() {
        return Err(ForjaError::InvalidInput(
            "la ruta del archivo no puede estar vacía".to_string(),
        ));
    }
    let target = canonical_write_target(root, &clean)?;
    // Sobre el destino final: un symlink no cambia lo que está protegido.
    check_write_target(root, &target, data)?;
    Ok(target)
}

fn canonical_write_target(root: &Path, clean: &Path) -> Result<PathBuf> {
    let candidate = root.join(clean);
    if let Ok(meta) = fs::symlink_metadata(&candidate) {
        let canon = candidate.canonicalize().map_err(|_| {
            ForjaError::InvalidInput("no se puede resolver el destino del archivo".to_string())
        })?;
        if !canon.starts_with(root) {
            return Err(ForjaError::InvalidInput(
                "acceso fuera del workspace denegado".to_string(),
            ));
        }
        if meta.is_dir() || canon.is_dir() {
            return Err(ForjaError::InvalidInput(
                "la ruta corresponde a un directorio".to_string(),
            ));
        }
        return Ok(canon);
    }

    let file_name = candidate
        .file_name()
        .ok_or_else(|| ForjaError::InvalidInput("nombre de archivo inválido".to_string()))?;
    let parent = candidate
        .parent()
        .ok_or_else(|| ForjaError::InvalidInput("directorio padre inválido".to_string()))?;
    let parent_canon = parent
        .canonicalize()
        .map_err(|err| ForjaError::InvalidInput(format!("el directorio padre no existe: {err}")))?;
    if !parent_canon.starts_with(root) {
        return Err(ForjaError::InvalidInput(
            "acceso fuera del workspace denegado".to_string(),
        ));
    }
    Ok(parent_canon.join(file_name))
}

/// Sustituye `target` de forma atómica conservando sus permisos. El temporal
/// nunca es más permisivo que el destino.
fn replace_file(target: &Path, content: &[u8]) -> Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| ForjaError::InvalidInput("directorio padre inválido".to_string()))?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let tmp_path = parent.join(format!(".forja-write-{}-{nanos}.tmp", std::process::id()));
    let original = match fs::metadata(target) {
        Ok(meta) => Some(meta.permissions()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err.into()),
    };

    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if let Some(perms) = &original {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.mode(perms.mode() & 0o600);
    }
    let mut tmp = options.open(&tmp_path)?;

    let written = (|| -> std::io::Result<()> {
        tmp.write_all(content)?;
        if let Some(perms) = original {
            #[cfg(unix)]
            let perms = {
                use std::os::unix::fs::PermissionsExt;
                fs::Permissions::from_mode(perms.mode() & 0o777)
            };
            tmp.set_permissions(perms)?;
        }
        drop(tmp);
        fs::rename(&tmp_path, target)
    })();
    if let Err(err) = written {
        let _ = fs::remove_file(&tmp_path);
        return Err(err.into());
    }
    Ok(())
}

fn to_rel_string(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let mut parts = Vec::new();
    for comp in rel.components() {
        if let Component::Normal(s) = comp {
            parts.push(s.to_string_lossy().into_owned());
        }
    }
    parts.join("/")
}

fn detect_language(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("rs") => "rust",
        Some("js" | "mjs" | "cjs" | "jsx") => "javascript",
        Some("ts" | "mts" | "cts" | "tsx") => "typescript",
        Some("html" | "htm") => "html",
        Some("css" | "scss") => "css",
        Some("json" | "jsonc") => "json",
        Some("toml") => "toml",
        Some("md" | "markdown") => "markdown",
        Some("yaml" | "yml") => "yaml",
        Some("sh" | "bash" | "zsh") => "bash",
        Some("py") => "python",
        _ => "text",
    }
}

fn is_ignored(name: &str) -> bool {
    IGNORED_NAMES.contains(&name)
}

/// `GET /api/files/tree?path=&run_id=&show_hidden=false`
pub(crate) async fn get_tree(
    State(state): State<AppState>,
    Query(query): Query<TreeQuery>,
) -> std::result::Result<Json<FileTreeResponse>, ApiError> {
    let (root, read_only) = resolve_root(&state, query.run_id.as_deref()).await?;
    let rel = query.path;
    let show_hidden = query.show_hidden;

    let resp = tokio::task::spawn_blocking(move || -> Result<FileTreeResponse> {
        let dir = resolve_safe_existing(&root, &rel)?;
        if !dir.is_dir() {
            return Err(ForjaError::InvalidInput(
                "la ruta solicitada no es un directorio".to_string(),
            ));
        }

        let read_dir = fs::read_dir(&dir).map_err(|err| {
            ForjaError::InvalidInput(format!("no se puede leer el directorio: {err}"))
        })?;

        let mut entries = Vec::new();
        for item in read_dir {
            let Ok(entry) = item else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if !show_hidden && is_ignored(&name) {
                continue;
            }
            let entry_path = entry.path();
            let Ok(canon_entry) = entry_path.canonicalize() else {
                continue;
            };
            if !canon_entry.starts_with(&root) {
                continue;
            }
            let Ok(meta) = fs::metadata(&canon_entry) else {
                continue;
            };
            let is_dir = meta.is_dir();
            let size = if is_dir { 0 } else { meta.len() };
            entries.push(FileTreeEntry {
                name,
                path: to_rel_string(&root, &entry_path),
                is_dir,
                size,
            });
        }

        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.name.cmp(&b.name))
        });

        Ok(FileTreeResponse {
            root: root.to_string_lossy().into_owned(),
            path: to_rel_string(&root, &dir),
            read_only,
            entries,
        })
    })
    .await
    .map_err(|err| ForjaError::InvalidInput(format!("fallo al listar archivos: {err}")))??;

    Ok(Json(resp))
}

/// `GET /api/files/read?path=...&run_id=`
pub(crate) async fn read_file(
    State(state): State<AppState>,
    Query(query): Query<ReadQuery>,
) -> std::result::Result<Json<FileReadResponse>, ApiError> {
    let trimmed = query.path.trim().to_string();
    if trimmed.is_empty() {
        return Err(ApiError(ForjaError::InvalidInput(
            "la ruta del archivo es obligatoria".to_string(),
        )));
    }
    let (root, read_only) = resolve_root(&state, query.run_id.as_deref()).await?;

    let resp = tokio::task::spawn_blocking(move || -> Result<FileReadResponse> {
        let target = resolve_safe_existing(&root, &trimmed)?;
        let meta = fs::metadata(&target).map_err(|err| {
            ForjaError::InvalidInput(format!("no se puede leer metadatos del archivo: {err}"))
        })?;
        if !meta.is_file() {
            return Err(ForjaError::InvalidInput(
                "la ruta solicitada no es un archivo regular".to_string(),
            ));
        }
        if meta.len() as usize > MAX_FILE_BYTES {
            return Err(ForjaError::InvalidInput(format!(
                "el archivo supera el límite máximo de {} bytes",
                MAX_FILE_BYTES
            )));
        }

        let bytes = fs::read(&target)?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(ForjaError::InvalidInput(format!(
                "el archivo supera el límite máximo de {} bytes",
                MAX_FILE_BYTES
            )));
        }
        if bytes.contains(&0) {
            return Err(ForjaError::InvalidInput(
                "archivo binario o no UTF-8".to_string(),
            ));
        }
        let content = String::from_utf8(bytes)
            .map_err(|_| ForjaError::InvalidInput("archivo binario o no UTF-8".to_string()))?;
        let size = content.len();
        let language = detect_language(&target).to_string();

        Ok(FileReadResponse {
            path: to_rel_string(&root, &target),
            content,
            size,
            read_only,
            language,
            workspace: root.to_string_lossy().into_owned(),
        })
    })
    .await
    .map_err(|err| ForjaError::InvalidInput(format!("fallo al leer archivo: {err}")))??;

    Ok(Json(resp))
}

/// `PUT /api/files/write`
pub(crate) async fn write_file(
    State(state): State<AppState>,
    Json(req): Json<WriteFileRequest>,
) -> std::result::Result<Json<FileWriteResponse>, ApiError> {
    if req.content.len() > MAX_FILE_BYTES {
        return Err(ApiError(ForjaError::InvalidInput(format!(
            "el contenido supera el límite máximo de {} bytes",
            MAX_FILE_BYTES
        ))));
    }
    if req.content.as_bytes().contains(&0) {
        return Err(ApiError(ForjaError::InvalidInput(
            "no se permite escribir contenido binario".to_string(),
        )));
    }

    let expected = req.workspace.filter(|w| !w.is_empty()).ok_or_else(|| {
        ForjaError::InvalidInput(
            "falta el campo 'workspace': envía el que devuelve GET /api/files/read \
                 (recarga la página si guardas desde el panel)"
                .to_string(),
        )
    })?;
    let workspaces = state.workspaces.clone();
    let data = ForjaData {
        store_dir: state.scheduler.store_dir(),
        settings_path: state.settings_path.clone(),
    };
    let rel = req.path;
    let content = req.content;

    let resp = tokio::task::spawn_blocking(move || -> Result<FileWriteResponse> {
        // La carpeta activa no puede cambiar entre la comprobación y el guardado.
        workspaces.with_active_root(|active| {
            let root = active.canonicalize().map_err(|err| {
                ForjaError::InvalidInput(format!("workspace activo inaccesible: {err}"))
            })?;
            if root.to_string_lossy() != expected.as_str() {
                return Err(ForjaError::Conflict(format!(
                    "la carpeta activa cambió: el archivo se abrió en '{expected}' y ahora está \
                     activa '{}'. Vuelve a la carpeta original para guardarlo",
                    root.display()
                )));
            }
            workspaces.check_allowed(&root)?;
            let target = resolve_safe_write(&root, &rel, &data)?;
            replace_file(&target, content.as_bytes())?;

            Ok(FileWriteResponse {
                ok: true,
                path: to_rel_string(&root, &target),
                size: content.len(),
            })
        })
    })
    .await
    .map_err(|err| ForjaError::InvalidInput(format!("fallo al guardar archivo: {err}")))??;

    Ok(Json(resp))
}

/// `GET /api/files/search?q=...&run_id=`
pub(crate) async fn search_files(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> std::result::Result<Json<FileSearchResponse>, ApiError> {
    let (root, _) = resolve_root(&state, query.run_id.as_deref()).await?;
    let raw_query = query.q;
    let needle = raw_query.trim().to_lowercase();

    if needle.is_empty() {
        return Ok(Json(FileSearchResponse {
            query: raw_query,
            matches: Vec::new(),
            truncated: false,
        }));
    }

    let (matches, truncated) = tokio::task::spawn_blocking(move || {
        let mut search = Search::new(&root, &needle, MAX_SEARCH_ENTRIES, MAX_SEARCH_TIME);
        search.walk(&root, 0);
        (search.matches, search.truncated)
    })
    .await
    .map_err(|err| ForjaError::InvalidInput(format!("fallo al buscar archivos: {err}")))?;

    Ok(Json(FileSearchResponse {
        query: raw_query,
        matches,
        truncated,
    }))
}

/// Recorrido de una búsqueda con límite de resultados, entradas y tiempo.
struct Search<'a> {
    root: &'a Path,
    needle: &'a str,
    entries_left: usize,
    deadline: Instant,
    matches: Vec<FileSearchMatch>,
    truncated: bool,
}

impl<'a> Search<'a> {
    fn new(root: &'a Path, needle: &'a str, max_entries: usize, max_time: Duration) -> Self {
        Self {
            root,
            needle,
            entries_left: max_entries,
            deadline: Instant::now() + max_time,
            matches: Vec::new(),
            truncated: false,
        }
    }

    fn out_of_time(&mut self) -> bool {
        let late = Instant::now() >= self.deadline;
        self.truncated |= late;
        late
    }

    fn walk(&mut self, dir: &Path, depth: usize) {
        if self.matches.len() >= MAX_SEARCH_RESULTS || self.out_of_time() {
            return;
        }
        let Ok(read_dir) = fs::read_dir(dir) else {
            return;
        };
        // El límite se aplica ya al listar: una sola carpeta puede ser enorme.
        let mut items = Vec::new();
        for entry in read_dir.flatten() {
            if self.entries_left == 0 {
                self.truncated = true;
                break;
            }
            if self.out_of_time() {
                break;
            }
            self.entries_left -= 1;
            items.push(entry);
        }
        items.sort_by_key(|e| e.file_name());

        for entry in items {
            if self.matches.len() >= MAX_SEARCH_RESULTS {
                self.truncated = true;
                return;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_ignored(&name) {
                continue;
            }
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            let is_dir = meta.is_dir();
            let rel = to_rel_string(self.root, &path);
            if rel.to_lowercase().contains(self.needle) {
                self.matches.push(FileSearchMatch {
                    name,
                    path: rel,
                    is_dir,
                });
            }
            if is_dir && depth < MAX_SEARCH_DEPTH {
                self.walk(&path, depth + 1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tmp");
        for dir in 0..4 {
            let sub = tmp.path().join(format!("d{dir}"));
            fs::create_dir(&sub).expect("dir");
            for file in 0..30 {
                fs::write(sub.join(format!("f{file}.txt")), "x").expect("file");
            }
        }
        tmp
    }

    #[test]
    fn search_stops_at_entry_limit_and_time_limit() {
        let tmp = tree();
        let long = Duration::from_secs(60);

        let mut full = Search::new(tmp.path(), "f1.txt", 10_000, long);
        full.walk(tmp.path(), 0);
        assert_eq!(full.matches.len(), 4);
        assert!(!full.truncated);

        let mut few = Search::new(tmp.path(), "f1.txt", 10, long);
        few.walk(tmp.path(), 0);
        assert!(few.truncated);
        assert_eq!(few.entries_left, 0);
        assert!(few.matches.len() <= 1);

        let mut late = Search::new(tmp.path(), "f1.txt", 10_000, Duration::ZERO);
        late.walk(tmp.path(), 0);
        assert!(late.truncated);
        assert!(late.matches.is_empty());

        let mut many = Search::new(tmp.path(), "txt", 10_000, long);
        many.walk(tmp.path(), 0);
        assert_eq!(many.matches.len(), MAX_SEARCH_RESULTS);
        assert!(many.truncated);
    }

    #[cfg(unix)]
    #[test]
    fn replace_file_keeps_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("tmp");
        for mode in [0o600, 0o755, 0o640, 0o400, 0o664] {
            let file = tmp.path().join(format!("m{mode:o}"));
            fs::write(&file, "antes").expect("write");
            fs::set_permissions(&file, fs::Permissions::from_mode(mode)).expect("chmod");
            replace_file(&file, b"despues").expect("replace");
            let got = fs::metadata(&file).expect("meta").permissions().mode() & 0o777;
            assert_eq!(got, mode, "modo {mode:o}");
            assert_eq!(fs::read(&file).expect("read"), b"despues");
        }
        let left = fs::read_dir(tmp.path()).expect("dir").count();
        assert_eq!(left, 5, "no quedan temporales");
    }
}
