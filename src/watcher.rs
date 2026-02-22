//! Polling des fichiers JSONL Claude Code toutes les 500ms.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::mpsc;
use anyhow::Result;

use crate::parser::{parse_line, Record, RecordType};

pub enum WatchEvent {
    /// Émis une seule fois au démarrage, avant tout scan de sessions
    LanesDiscovered {
        /// (project_path, is_lead, compose_project_name)
        paths: Vec<(String, bool, Option<String>)>,
    },
    /// Émis après le premier scan complet (y compris les fichiers anciens skippés)
    ScanComplete,
    SessionUpdate {
        session_id: String,
        project_path: String,
        session_file: PathBuf,
        new_records: Vec<Record>,
        compose_project_name: Option<String>,
        is_lead: bool,
    },
    ChildDiscovered {
        parent_tool_use_id: String,
        parent_session_id: Option<String>,
        child_session_id: String,
        child_session_file: PathBuf,
        new_records: Vec<Record>,
    },
}

pub fn encode_project_path(path: &str) -> String {
    path.replace('/', "-")
}

async fn read_compose_project_name(project_path: &str) -> Option<String> {
    let env_path = PathBuf::from(project_path).join("docker").join(".env");
    let content = tokio::fs::read_to_string(env_path).await.ok()?;
    for line in content.lines() {
        if let Some(val) = line.strip_prefix("COMPOSE_PROJECT_NAME=") {
            let v = val.trim().trim_matches('"').trim_matches('\'').to_string();
            if !v.is_empty() { return Some(v); }
        }
    }
    None
}

fn claude_projects_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
        .join(".claude").join("projects")
}

async fn read_worktree_paths(lead_path: &str) -> Vec<String> {
    let worktrees_dir = PathBuf::from(lead_path).join(".git").join("worktrees");
    let mut result = Vec::new();
    let Ok(mut entries) = fs::read_dir(&worktrees_dir).await else { return result };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let gitdir_path = entry.path().join("gitdir");
        if let Ok(content) = tokio::fs::read_to_string(&gitdir_path).await {
            let trimmed = content.trim();
            // gitdir contient e.g. "/mnt/data/AC/wt-1/.git"
            let worker_path = trimmed.strip_suffix("/.git").unwrap_or(trimmed);
            if !worker_path.is_empty() {
                result.push(worker_path.to_string());
            }
        }
    }
    result
}

struct ScanEntry {
    project_path: String,
    path: PathBuf,
    parent_session_id: Option<String>, // Some(uuid) si depuis uuid/subagents/
}

async fn scan_project_dirs(paths: &[String]) -> Vec<ScanEntry> {
    let base = claude_projects_dir();
    let mut result = Vec::new();
    for project_path in paths {
        let encoded = encode_project_path(project_path);
        let project_dir = base.join(&encoded);

        // Scan *.jsonl directement dans le project dir (sessions principales)
        if let Ok(mut entries) = fs::read_dir(&project_dir).await {
            while let Ok(Some(e)) = entries.next_entry().await {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                    result.push(ScanEntry {
                        project_path: project_path.clone(),
                        path: p,
                        parent_session_id: None,
                    });
                }
            }
        }

        // Scan <uuid>/subagents/*.jsonl
        if let Ok(mut entries) = fs::read_dir(&project_dir).await {
            while let Ok(Some(e)) = entries.next_entry().await {
                let uuid_dir = e.path();
                if !uuid_dir.is_dir() { continue; }
                let uuid = match uuid_dir.file_name().and_then(|n| n.to_str()) {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                let subagents_dir = uuid_dir.join("subagents");
                if let Ok(mut subentries) = fs::read_dir(&subagents_dir).await {
                    while let Ok(Some(se)) = subentries.next_entry().await {
                        let p = se.path();
                        if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                            result.push(ScanEntry {
                                project_path: project_path.clone(),
                                path: p,
                                parent_session_id: Some(uuid.clone()),
                            });
                        }
                    }
                }
            }
        }
    }
    result
}

async fn read_new_lines(path: &Path, offset: &mut u64) -> Vec<Record> {
    let Ok(mut file) = tokio::fs::File::open(path).await else { return vec![] };
    let Ok(meta) = file.metadata().await else { return vec![] };
    if meta.len() <= *offset { return vec![]; }
    let Ok(_) = file.seek(tokio::io::SeekFrom::Start(*offset)).await else { return vec![] };
    let mut buf = String::new();
    let Ok(n) = file.read_to_string(&mut buf).await else { return vec![] };
    *offset += n as u64;
    buf.lines().filter_map(|l| parse_line(l)).collect()
}

/// Vrai si le fichier a été modifié dans les dernières MAX_AGE_SECS secondes
async fn was_modified_recently(path: &Path) -> bool {
    const MAX_AGE_SECS: u64 = 7200; // 2 heures
    let Ok(meta) = tokio::fs::metadata(path).await else { return true };
    let Ok(modified) = meta.modified() else { return true };
    let Ok(age) = modified.elapsed() else { return true };
    age.as_secs() < MAX_AGE_SECS
}

async fn peek_parent_tool_use_id(path: &Path) -> Option<String> {
    let Ok(content) = tokio::fs::read_to_string(path).await else { return None };
    for line in content.lines().take(5) {
        if let Some(r) = parse_line(line) {
            if matches!(r.record_type, RecordType::System) {
                if let Some(id) = r.parent_tool_use_id { return Some(id); }
            }
        }
    }
    None
}

enum ChildKind {
    ByToolUseId,
    BySessionId(String), // parent_session_id
}

pub async fn watch_sessions(tx: mpsc::Sender<WatchEvent>, lead_path: String) -> Result<()> {
    let mut offsets: HashMap<PathBuf, u64> = HashMap::new();
    let mut known: HashMap<PathBuf, String> = HashMap::new();
    let mut child_kind: HashMap<PathBuf, ChildKind> = HashMap::new();
    let mut compose_cache: HashMap<String, Option<String>> = HashMap::new();
    let mut first_scan = true;

    loop {
        // Recalcule les worktrees à chaque itération (peuvent être créés/supprimés)
        let mut all_paths = vec![lead_path.clone()];
        all_paths.extend(read_worktree_paths(&lead_path).await);

        // Remplir le cache compose (une seule fois par path)
        for path in &all_paths {
            if !compose_cache.contains_key(path.as_str()) {
                let name = read_compose_project_name(path).await;
                compose_cache.insert(path.clone(), name);
            }
        }

        // Première itération : annoncer les lanes AVANT tout scan de sessions
        if first_scan {
            let paths: Vec<(String, bool, Option<String>)> = all_paths.iter()
                .map(|p| (
                    p.clone(),
                    p == &lead_path,
                    compose_cache.get(p).cloned().flatten(),
                ))
                .collect();
            let _ = tx.send(WatchEvent::LanesDiscovered { paths }).await;
        }

        for entry in scan_project_dirs(&all_paths).await {
            let ScanEntry { project_path, path: session_file, parent_session_id } = entry;

            let offset = offsets.entry(session_file.clone()).or_insert(0);
            let is_new = !known.contains_key(&session_file);
            let session_id = session_file.file_stem()
                .and_then(|s| s.to_str()).unwrap_or("unknown").to_string();
            known.insert(session_file.clone(), session_id.clone());

            // Fichier ancien (>2h sans modification) → skip définitif au démarrage
            if is_new && !was_modified_recently(&session_file).await {
                if let Ok(meta) = tokio::fs::metadata(&session_file).await {
                    *offset = meta.len(); // fast-forward à la fin
                }
                continue; // pas d'event, pas d'agent fantôme
            }

            let new_records = read_new_lines(&session_file, offset).await;

            if is_new {
                // Fichier dans uuid/subagents/ → child par session_id
                if let Some(ref psid) = parent_session_id {
                    child_kind.insert(session_file.clone(), ChildKind::BySessionId(psid.clone()));
                    let _ = tx.send(WatchEvent::ChildDiscovered {
                        parent_tool_use_id: String::new(),
                        parent_session_id: Some(psid.clone()),
                        child_session_id: session_id,
                        child_session_file: session_file.clone(),
                        new_records,
                    }).await;
                    continue;
                }
                // Vérifier si c'est un child via parentToolUseId
                if let Some(parent_tool_use_id) = peek_parent_tool_use_id(&session_file).await {
                    child_kind.insert(session_file.clone(), ChildKind::ByToolUseId);
                    let _ = tx.send(WatchEvent::ChildDiscovered {
                        parent_tool_use_id,
                        parent_session_id: None,
                        child_session_id: session_id,
                        child_session_file: session_file.clone(),
                        new_records,
                    }).await;
                    continue;
                }
                // Session normale : pas de ChildKind → will fall through to SessionUpdate
            }

            let is_child = child_kind.contains_key(&session_file);
            if !is_child && (!new_records.is_empty() || is_new) {
                let compose_project_name = compose_cache.get(&project_path).cloned().flatten();
                let is_lead = project_path == lead_path;
                let _ = tx.send(WatchEvent::SessionUpdate {
                    session_id, project_path, session_file: session_file.clone(),
                    new_records, compose_project_name, is_lead,
                }).await;
            } else if is_child && !new_records.is_empty() {
                let psi = match child_kind.get(&session_file) {
                    Some(ChildKind::BySessionId(psid)) => Some(psid.clone()),
                    _ => None,
                };
                let _ = tx.send(WatchEvent::ChildDiscovered {
                    parent_tool_use_id: String::new(),
                    parent_session_id: psi,
                    child_session_id: session_id,
                    child_session_file: session_file.clone(),
                    new_records,
                }).await;
            }
        }
        if first_scan {
            let _ = tx.send(WatchEvent::ScanComplete).await;
            first_scan = false;
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }
}
