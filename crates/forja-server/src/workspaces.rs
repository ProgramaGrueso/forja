use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use axum::extract::{Query, State};
use axum::Json;
use forja_core::{ForjaError, Result};
use serde::{Deserialize, Serialize};

use crate::{ApiError, AppState};

const MAX_RECENT: usize = 15;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub path: String,
    pub name: String,
    pub is_git: bool,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspacesResponse {
    pub active: WorkspaceInfo,
    pub recent: Vec<WorkspaceInfo>,
}

#[derive(Debug, Deserialize)]
pub struct SelectWorkspaceRequest {
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct BrowseQuery {
    pub path: Option<String>,
    #[serde(default)]
    pub show_hidden: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowseEntry {
    pub name: String,
    pub path: String,
    pub is_git: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowseResponse {
    pub current: String,
    pub parent: Option<String>,
    pub is_git: bool,
    pub entries: Vec<BrowseEntry>,
}

#[derive(Clone)]
pub struct WorkspaceService {
    active_root: Arc<RwLock<PathBuf>>,
    recent_path: PathBuf,
    recent: Arc<RwLock<Vec<String>>>,
}

pub fn default_workspaces_path() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        let trimmed = xdg.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join("forja").join("workspaces.json");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let trimmed = home.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed)
                .join(".config")
                .join("forja")
                .join("workspaces.json");
        }
    }
    PathBuf::from(".config/forja/workspaces.json")
}

impl WorkspaceService {
    pub fn new(initial_root: PathBuf, recent_path: PathBuf) -> Self {
        let active = initial_root.canonicalize().unwrap_or(initial_root);
        let loaded = load_recent(&recent_path);
        Self {
            active_root: Arc::new(RwLock::new(active)),
            recent_path,
            recent: Arc::new(RwLock::new(loaded)),
        }
    }

    pub fn active_root(&self) -> PathBuf {
        self.active_root
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn recent_paths(&self) -> Vec<String> {
        self.recent
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set_active_and_record(&self, canon: PathBuf) -> Result<Vec<String>> {
        let path_str = canon.to_string_lossy().into_owned();
        *self.active_root.write().unwrap_or_else(|e| e.into_inner()) = canon;

        let updated = {
            let mut recent = self.recent.write().unwrap_or_else(|e| e.into_inner());
            recent.retain(|item| item != &path_str);
            recent.insert(0, path_str);
            recent.truncate(MAX_RECENT);
            recent.clone()
        };

        save_recent(&self.recent_path, &updated)?;
        Ok(updated)
    }
}

fn load_recent(path: &Path) -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<Vec<String>>(&raw) else {
        return Vec::new();
    };
    let mut deduped = Vec::with_capacity(parsed.len().min(MAX_RECENT));
    for item in parsed {
        let trimmed = item.trim();
        if !trimmed.is_empty() && !deduped.iter().any(|existing| existing == trimmed) {
            deduped.push(trimmed.to_string());
            if deduped.len() >= MAX_RECENT {
                break;
            }
        }
    }
    deduped
}

fn save_recent(path: &Path, items: &[String]) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let json = serde_json::to_string_pretty(items)?;
    std::fs::write(path, json)?;
    Ok(())
}

pub fn workspace_info(path: &Path) -> WorkspaceInfo {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    let branch = forja_git::current_branch(path).ok();
    let is_git = branch.is_some();
    WorkspaceInfo {
        path: path.to_string_lossy().into_owned(),
        name,
        is_git,
        branch,
    }
}

/// `GET /api/workspaces`: workspace activo y lista de recientes.
pub(crate) async fn get_workspaces(
    State(state): State<AppState>,
) -> std::result::Result<Json<WorkspacesResponse>, ApiError> {
    let active_root = state.workspaces.active_root();
    let recent_paths = state.workspaces.recent_paths();

    let resp = tokio::task::spawn_blocking(move || {
        let active = workspace_info(&active_root);
        let recent = recent_paths
            .iter()
            .map(|p| workspace_info(Path::new(p)))
            .collect();
        WorkspacesResponse { active, recent }
    })
    .await
    .map_err(|err| ForjaError::InvalidInput(format!("fallo al consultar workspaces: {err}")))?;

    Ok(Json(resp))
}

/// `POST /api/workspaces/select`: cambia la carpeta activa del workspace.
pub(crate) async fn select_workspace(
    State(state): State<AppState>,
    Json(req): Json<SelectWorkspaceRequest>,
) -> std::result::Result<Json<WorkspacesResponse>, ApiError> {
    let raw = req.path.trim().to_string();
    if raw.is_empty() {
        return Err(ApiError(ForjaError::InvalidInput(
            "la ruta del workspace no puede estar vacía".to_string(),
        )));
    }

    let (canon, info) = tokio::task::spawn_blocking(move || -> Result<(PathBuf, WorkspaceInfo)> {
        let path = PathBuf::from(&raw);
        if !path.exists() {
            return Err(ForjaError::InvalidInput(format!(
                "la carpeta no existe: {raw}"
            )));
        }
        if !path.is_dir() {
            return Err(ForjaError::InvalidInput(format!(
                "la ruta no es un directorio: {raw}"
            )));
        }
        let canon = path.canonicalize().map_err(|err| {
            ForjaError::InvalidInput(format!("no se pudo resolver la ruta '{raw}': {err}"))
        })?;
        let info = workspace_info(&canon);
        Ok((canon, info))
    })
    .await
    .map_err(|err| ForjaError::InvalidInput(format!("fallo al validar workspace: {err}")))??;

    let repo_arg = if info.is_git {
        Some(canon.clone())
    } else {
        None
    };
    let branch_arg = info.branch.clone().unwrap_or_else(|| "main".to_string());

    if let Err(err) = state.scheduler.set_workspace(repo_arg, branch_arg).await {
        return Err(ApiError(ForjaError::Conflict(err.to_string())));
    }

    let recent_paths = state.workspaces.set_active_and_record(canon)?;
    let recent = tokio::task::spawn_blocking(move || {
        recent_paths
            .iter()
            .map(|p| workspace_info(Path::new(p)))
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|err| ForjaError::InvalidInput(format!("fallo al leer recientes: {err}")))?;

    Ok(Json(WorkspacesResponse {
        active: info,
        recent,
    }))
}

/// `GET /api/workspaces/browse?path=...&show_hidden=false`: explora carpetas del sistema.
pub(crate) async fn browse_workspaces(
    State(state): State<AppState>,
    Query(query): Query<BrowseQuery>,
) -> std::result::Result<Json<BrowseResponse>, ApiError> {
    let fallback = state.workspaces.active_root();
    let requested = query
        .path
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let show_hidden = query.show_hidden;

    let resp = tokio::task::spawn_blocking(move || -> Result<BrowseResponse> {
        let target = match requested {
            Some(p) => PathBuf::from(p),
            None => {
                if fallback.is_dir() {
                    fallback
                } else {
                    std::env::var("HOME")
                        .ok()
                        .map(PathBuf::from)
                        .filter(|p| p.is_dir())
                        .unwrap_or_else(|| PathBuf::from("/"))
                }
            }
        };

        if !target.exists() || !target.is_dir() {
            return Err(ForjaError::InvalidInput(format!(
                "directorio no encontrado: {}",
                target.display()
            )));
        }
        let canon = target.canonicalize().map_err(|err| {
            ForjaError::InvalidInput(format!("no se pudo resolver el directorio: {err}"))
        })?;

        let read_dir = std::fs::read_dir(&canon).map_err(|err| {
            ForjaError::InvalidInput(format!("no se puede leer el directorio: {err}"))
        })?;

        let mut entries = Vec::new();
        for item in read_dir {
            let Ok(entry) = item else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if !show_hidden && name.starts_with('.') {
                continue;
            }
            let entry_path = entry.path();
            let Ok(meta) = std::fs::metadata(&entry_path) else {
                continue;
            };
            if !meta.is_dir() {
                continue;
            }
            let is_git = entry_path.join(".git").exists();
            entries.push(BrowseEntry {
                name,
                path: entry_path.to_string_lossy().into_owned(),
                is_git,
            });
        }

        entries.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.name.cmp(&b.name))
        });

        let parent = canon.parent().map(|p| p.to_string_lossy().into_owned());
        let is_git = forja_git::current_branch(&canon).is_ok();

        Ok(BrowseResponse {
            current: canon.to_string_lossy().into_owned(),
            parent,
            is_git,
            entries,
        })
    })
    .await
    .map_err(|err| ForjaError::InvalidInput(format!("fallo al explorar directorios: {err}")))??;

    Ok(Json(resp))
}
