#!/bin/bash
# bootstrap-claude-swarm.sh
# Génère la structure complète du projet claude-swarm avec monitoring Docker
# Usage: bash bootstrap-claude-swarm.sh [destination]

set -e
DEST="${1:-./claude-swarm}"
echo "🐝 Création de claude-swarm dans $DEST ..."
mkdir -p "$DEST/src"
cd "$DEST"

# ── Cargo.toml ────────────────────────────────────────────────────────────────
cat > Cargo.toml << 'EOF'
[package]
name = "claude-swarm"
version = "0.1.0"
edition = "2021"

[dependencies]
ratatui = "0.29"
crossterm = "0.28"
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
anyhow = "1"
chrono = { version = "0.4", features = ["serde"] }
bollard = "0.17"
futures-util = "0.3"
EOF

# ── src/main.rs ───────────────────────────────────────────────────────────────
cat > src/main.rs << 'EOF'
mod parser;
mod watcher;
mod docker;
mod state;
mod ui;
mod app;

use anyhow::Result;
use app::App;

#[tokio::main]
async fn main() -> Result<()> {
    let mut app = App::new().await?;
    app.run().await?;
    Ok(())
}
EOF

# ── src/parser.rs ─────────────────────────────────────────────────────────────
cat > src/parser.rs << 'EOF'
//! Parsing des fichiers JSONL de session Claude Code.
//!
//! Chemin: ~/.claude/projects/<url-encoded-path>/sessions/<uuid>.jsonl
//! Chaque ligne = un Record typé.

use anyhow::Result;
use serde::Deserialize;
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Debug, Deserialize, Clone)]
pub struct Record {
    #[serde(rename = "type")]
    pub record_type: RecordType,
    pub uuid: Option<String>,
    #[serde(rename = "parentUuid")]
    pub parent_uuid: Option<String>,
    pub timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub message: Option<Message>,
    #[serde(rename = "parentToolUseId")]
    pub parent_tool_use_id: Option<String>,
    #[serde(rename = "agentId")]
    pub agent_id: Option<String>,
    #[serde(rename = "agentType")]
    pub agent_type: Option<String>,
    #[serde(rename = "teamName")]
    pub team_name: Option<String>,
    #[serde(rename = "isError")]
    pub is_error: Option<bool>,
    pub cost: Option<f64>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum RecordType {
    User,
    Assistant,
    ToolResult,
    System,
    Summary,
    Result,
    #[serde(rename = "file-history-snapshot")]
    FileHistorySnapshot,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Message {
    pub role: Option<String>,
    pub model: Option<String>,
    pub content: Option<MessageContent>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Deserialize, Clone)]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    pub text: Option<String>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub input: Option<serde_json::Value>,
    pub thinking: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    ToolCall {
        tool_name: String,
        tool_input_summary: String,
        timestamp: Option<String>,
    },
    ToolResult {
        is_error: bool,
        timestamp: Option<String>,
    },
    TextResponse {
        text: String,
        timestamp: Option<String>,
    },
    SpawnSubAgent {
        task_tool_use_id: String,
        prompt_summary: String,
        timestamp: Option<String>,
    },
    Completed {
        is_error: bool,
        timestamp: Option<String>,
    },
}

pub async fn parse_session_file(path: &Path) -> Result<Vec<Record>> {
    let file = File::open(path).await?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();
    let mut records = Vec::new();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim().to_string();
        if line.is_empty() { continue; }
        if let Ok(r) = serde_json::from_str::<Record>(&line) {
            records.push(r);
        }
    }
    Ok(records)
}

pub fn parse_line(line: &str) -> Option<Record> {
    let line = line.trim();
    if line.is_empty() { return None; }
    serde_json::from_str(line).ok()
}

pub fn extract_events(records: &[Record]) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    for record in records {
        match record.record_type {
            RecordType::Assistant => {
                if let Some(msg) = &record.message {
                    if let Some(MessageContent::Blocks(blocks)) = &msg.content {
                        for block in blocks {
                            match block.block_type.as_str() {
                                "tool_use" => {
                                    let tool_name = block.name.clone().unwrap_or_default();
                                    if tool_name == "Task" {
                                        let prompt = block.input.as_ref()
                                            .and_then(|v| v.get("prompt"))
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .chars().take(80).collect::<String>();
                                        events.push(AgentEvent::SpawnSubAgent {
                                            task_tool_use_id: block.id.clone().unwrap_or_default(),
                                            prompt_summary: prompt,
                                            timestamp: record.timestamp.clone(),
                                        });
                                    } else {
                                        let summary = summarize_tool_input(&tool_name, &block.input);
                                        events.push(AgentEvent::ToolCall {
                                            tool_name,
                                            tool_input_summary: summary,
                                            timestamp: record.timestamp.clone(),
                                        });
                                    }
                                }
                                "text" => {
                                    if let Some(text) = &block.text {
                                        if !text.trim().is_empty() {
                                            events.push(AgentEvent::TextResponse {
                                                text: text.chars().take(120).collect(),
                                                timestamp: record.timestamp.clone(),
                                            });
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            RecordType::Result => {
                events.push(AgentEvent::Completed {
                    is_error: record.is_error.unwrap_or(false),
                    timestamp: record.timestamp.clone(),
                });
            }
            _ => {}
        }
    }
    events
}

fn summarize_tool_input(tool_name: &str, input: &Option<serde_json::Value>) -> String {
    let Some(input) = input else { return String::new() };
    match tool_name {
        "Bash" => input.get("command").and_then(|v| v.as_str()).unwrap_or("").chars().take(60).collect(),
        "Read" | "Write" | "Edit" | "MultiEdit" => input.get("file_path").and_then(|v| v.as_str()).unwrap_or("").chars().take(60).collect(),
        "Glob" | "Grep" => input.get("pattern").and_then(|v| v.as_str()).unwrap_or("").chars().take(60).collect(),
        _ => input.to_string().chars().take(60).collect(),
    }
}
EOF

# ── src/docker.rs ─────────────────────────────────────────────────────────────
cat > src/docker.rs << 'EOF'
//! Monitoring Docker via bollard (Unix socket /var/run/docker.sock).
//!
//! On poll toutes les 2s l'état des containers.
//! On stream les événements Docker en temps réel pour les transitions d'état.
//!
//! Lien avec les agents Claude Code :
//!   label `com.docker.compose.project` = nom du projet Compose
//!   → matché contre le repo_name de la RepoLane (ou le cwd de l'agent)

use anyhow::Result;
use bollard::Docker;
use bollard::container::{ListContainersOptions, StatsOptions};
use bollard::models::ContainerSummary;
use bollard::system::EventsOptions;
use futures_util::StreamExt;
use std::collections::HashMap;
use tokio::sync::mpsc;
use std::time::Instant;

// ── Types publics ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum ContainerStatus {
    Running,
    Paused,
    Restarting,
    Exited(i64),  // code de sortie
    Dead,
    Created,
    Unknown,
}

impl ContainerStatus {
    pub fn from_str(s: &str) -> Self {
        match s {
            "running"    => ContainerStatus::Running,
            "paused"     => ContainerStatus::Paused,
            "restarting" => ContainerStatus::Restarting,
            "dead"       => ContainerStatus::Dead,
            "created"    => ContainerStatus::Created,
            s if s.starts_with("exited") => {
                // "exited (0)" ou juste "exited"
                let code = s.trim_start_matches("exited")
                    .trim_matches(|c: char| !c.is_ascii_digit())
                    .parse().unwrap_or(0);
                ContainerStatus::Exited(code)
            }
            _ => ContainerStatus::Unknown,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            ContainerStatus::Running    => "🟢 running",
            ContainerStatus::Paused     => "⏸  paused",
            ContainerStatus::Restarting => "🔄 restarting",
            ContainerStatus::Exited(0)  => "⚪ exited(0)",
            ContainerStatus::Exited(_)  => "🔴 exited(err)",
            ContainerStatus::Dead       => "💀 dead",
            ContainerStatus::Created    => "🔵 created",
            ContainerStatus::Unknown    => "❓ unknown",
        }
    }

    pub fn color(&self) -> ratatui::style::Color {
        use ratatui::style::Color;
        match self {
            ContainerStatus::Running    => Color::Green,
            ContainerStatus::Restarting => Color::Yellow,
            ContainerStatus::Exited(0)  => Color::DarkGray,
            ContainerStatus::Exited(_)  => Color::Red,
            ContainerStatus::Dead       => Color::Red,
            _                           => Color::DarkGray,
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self, ContainerStatus::Running | ContainerStatus::Restarting)
    }
}

#[derive(Debug, Clone)]
pub struct ContainerInfo {
    pub id: String,           // short (12 chars)
    pub name: String,         // sans le '/' préfixe
    pub image: String,        // image:tag
    pub status: ContainerStatus,
    pub compose_service: Option<String>,  // label com.docker.compose.service
    pub compose_project: Option<String>,  // label com.docker.compose.project
    pub cpu_percent: f64,
    pub mem_mb: f64,
    pub mem_limit_mb: f64,
    pub last_updated: Instant,
}

impl ContainerInfo {
    pub fn service_name(&self) -> String {
        self.compose_service.clone()
            .unwrap_or_else(|| self.name.clone())
    }
}

#[derive(Debug, Clone)]
pub struct DockerStack {
    pub project_name: String,
    pub containers: Vec<ContainerInfo>,
}

impl DockerStack {
    pub fn has_running(&self) -> bool {
        self.containers.iter().any(|c| c.status.is_active())
    }

    pub fn all_healthy(&self) -> bool {
        self.containers.iter().all(|c| c.status == ContainerStatus::Running)
    }
}

// ── Events émis vers l'app ────────────────────────────────────────────────────

pub enum DockerEvent {
    /// Snapshot complet de toutes les stacks (poll périodique)
    StacksSnapshot(Vec<DockerStack>),
    /// Un container a changé d'état (stream d'events Docker)
    ContainerStateChange {
        container_id: String,
        new_status: ContainerStatus,
    },
}

// ── Connexion Docker ─────────────────────────────────────────────────────────

pub async fn connect() -> Result<Docker> {
    // Essaie la socket Unix en premier, puis TCP (pour Docker Desktop / remote)
    Docker::connect_with_socket_defaults()
        .or_else(|_| Docker::connect_with_local_defaults())
        .map_err(|e| anyhow::anyhow!("Cannot connect to Docker socket: {e}\nIs Docker running?"))
}

// ── Poll des containers ───────────────────────────────────────────────────────

pub async fn poll_stacks(docker: &Docker) -> Result<Vec<DockerStack>> {
    let mut filters = HashMap::new();
    // On veut uniquement les containers Compose (qui ont le label project)
    filters.insert("label", vec!["com.docker.compose.project"]);

    let containers = docker.list_containers(Some(ListContainersOptions {
        all: true,
        filters,
        ..Default::default()
    })).await?;

    // Grouper par projet Compose
    let mut stacks: HashMap<String, Vec<ContainerInfo>> = HashMap::new();

    for c in containers {
        let info = container_summary_to_info(c);
        let project = info.compose_project.clone().unwrap_or_else(|| "unknown".to_string());
        stacks.entry(project).or_default().push(info);
    }

    // Enrichir avec les stats CPU/RAM (best-effort, on ignore les erreurs)
    let mut result = Vec::new();
    for (project_name, mut containers) in stacks {
        for container in containers.iter_mut() {
            if container.status.is_active() {
                if let Ok(stats) = fetch_stats(docker, &container.id).await {
                    container.cpu_percent = stats.0;
                    container.mem_mb = stats.1;
                    container.mem_limit_mb = stats.2;
                }
            }
        }
        // Trier par nom de service
        containers.sort_by(|a, b| a.service_name().cmp(&b.service_name()));
        result.push(DockerStack { project_name, containers });
    }

    // Trier les stacks par nom
    result.sort_by(|a, b| a.project_name.cmp(&b.project_name));
    Ok(result)
}

fn container_summary_to_info(c: ContainerSummary) -> ContainerInfo {
    let labels = c.labels.unwrap_or_default();
    let id = c.id.unwrap_or_default();
    let short_id = id.chars().take(12).collect();

    let name = c.names.unwrap_or_default()
        .into_iter().next().unwrap_or_default()
        .trim_start_matches('/').to_string();

    let image = c.image.unwrap_or_default();
    let status_str = c.status.unwrap_or_default().to_lowercase();
    let status = ContainerStatus::from_str(&status_str);

    ContainerInfo {
        id: short_id,
        name,
        image,
        status,
        compose_service: labels.get("com.docker.compose.service").cloned(),
        compose_project: labels.get("com.docker.compose.project").cloned(),
        cpu_percent: 0.0,
        mem_mb: 0.0,
        mem_limit_mb: 0.0,
        last_updated: Instant::now(),
    }
}

/// Récupère une seule mesure de stats CPU/RAM pour un container
/// (bollard retourne un stream — on prend le 2e sample pour avoir un delta CPU valide)
async fn fetch_stats(docker: &Docker, container_id: &str) -> Result<(f64, f64, f64)> {
    let mut stream = docker.stats(container_id, Some(StatsOptions {
        stream: true,
        one_shot: false,
    }));

    // Premier sample (pas de delta CPU valide)
    let s1 = match stream.next().await {
        Some(Ok(s)) => s,
        _ => return Ok((0.0, 0.0, 0.0)),
    };

    // Deuxième sample pour le delta
    let s2 = match stream.next().await {
        Some(Ok(s)) => s,
        _ => return Ok((0.0, 0.0, 0.0)),
    };

    // CPU %
    let cpu_delta = s2.cpu_stats.cpu_usage.total_usage as f64
        - s1.cpu_stats.cpu_usage.total_usage as f64;
    let sys_delta = s2.cpu_stats.system_cpu_usage.unwrap_or(0) as f64
        - s1.cpu_stats.system_cpu_usage.unwrap_or(0) as f64;
    let num_cpus = s2.cpu_stats.online_cpus.unwrap_or(1) as f64;
    let cpu_pct = if sys_delta > 0.0 { (cpu_delta / sys_delta) * num_cpus * 100.0 } else { 0.0 };

    // RAM
    let mem_usage = s2.memory_stats.usage.unwrap_or(0) as f64 / 1_048_576.0;
    let mem_limit = s2.memory_stats.limit.unwrap_or(1) as f64 / 1_048_576.0;

    Ok((cpu_pct, mem_usage, mem_limit))
}

// ── Stream d'events Docker ────────────────────────────────────────────────────

pub async fn stream_events(docker: Docker, tx: mpsc::Sender<DockerEvent>) -> Result<()> {
    let mut filters = HashMap::new();
    filters.insert("type", vec!["container"]);

    let mut events = docker.events(Some(EventsOptions {
        filters,
        ..Default::default()
    }));

    while let Some(event) = events.next().await {
        let Ok(ev) = event else { continue };

        let action = ev.action.as_deref().unwrap_or("");
        let new_status = match action {
            "start"   => Some(ContainerStatus::Running),
            "die"     => Some(ContainerStatus::Exited(0)),
            "kill"    => Some(ContainerStatus::Dead),
            "pause"   => Some(ContainerStatus::Paused),
            "unpause" => Some(ContainerStatus::Running),
            "restart" => Some(ContainerStatus::Restarting),
            _         => None,
        };

        if let Some(status) = new_status {
            let container_id = ev.actor
                .and_then(|a| a.id)
                .unwrap_or_default()
                .chars().take(12).collect();

            let _ = tx.send(DockerEvent::ContainerStateChange {
                container_id,
                new_status: status,
            }).await;
        }
    }
    Ok(())
}

// ── Tâche de poll périodique ──────────────────────────────────────────────────

pub async fn poll_loop(docker: Docker, tx: mpsc::Sender<DockerEvent>) -> Result<()> {
    loop {
        if let Ok(stacks) = poll_stacks(&docker).await {
            let _ = tx.send(DockerEvent::StacksSnapshot(stacks)).await;
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    }
}
EOF

# ── src/state.rs ──────────────────────────────────────────────────────────────
cat > src/state.rs << 'EOF'
//! State global: SwarmState → RepoLane → Agent + DockerStack

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use crate::parser::AgentEvent;
use crate::docker::{DockerStack, ContainerStatus};

const IDLE_THRESHOLD_SECS: u64 = 30;

// ── AgentStatus ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum AgentStatus {
    Working,
    WaitingForYou,
    Idle,
    Completed,
    Error,
}

impl AgentStatus {
    pub fn label(&self) -> &'static str {
        match self {
            AgentStatus::Working       => "● WORKING",
            AgentStatus::WaitingForYou => "⏸ WAITING FOR YOU",
            AgentStatus::Idle          => "◌ IDLE",
            AgentStatus::Completed     => "✓ DONE",
            AgentStatus::Error         => "✗ ERROR",
        }
    }
    pub fn color(&self) -> ratatui::style::Color {
        use ratatui::style::Color;
        match self {
            AgentStatus::Working       => Color::Green,
            AgentStatus::WaitingForYou => Color::Yellow,
            AgentStatus::Idle          => Color::DarkGray,
            AgentStatus::Completed     => Color::Cyan,
            AgentStatus::Error         => Color::Red,
        }
    }
}

// ── SubAgent ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SubAgent {
    pub task_tool_use_id: String,
    pub prompt_summary: String,
    pub session_id: Option<String>,
    pub status: AgentStatus,
    pub last_tool: Option<String>,
    pub last_tool_input: Option<String>,
    pub last_activity: Option<Instant>,
}

impl SubAgent {
    pub fn elapsed_str(&self) -> String {
        let Some(t) = self.last_activity else { return "—".to_string() };
        let secs = t.elapsed().as_secs();
        if secs < 60 { format!("{}s ago", secs) }
        else { format!("{}m{}s ago", secs / 60, secs % 60) }
    }
}

// ── Agent ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum AgentRole { Lead, Worker }

#[derive(Debug, Clone)]
pub struct Agent {
    pub session_id: String,
    pub session_file: PathBuf,
    pub role: AgentRole,
    pub status: AgentStatus,
    pub last_tool: Option<String>,
    pub last_tool_input: Option<String>,
    pub last_message: Option<String>,
    pub last_activity: Instant,
    pub sub_agents: Vec<SubAgent>,
    pub file_offset: u64,
    pub pending_tool: bool,
}

impl Agent {
    pub fn new(session_id: String, session_file: PathBuf) -> Self {
        Agent {
            session_id, session_file,
            role: AgentRole::Worker,
            status: AgentStatus::Idle,
            last_tool: None, last_tool_input: None, last_message: None,
            last_activity: Instant::now(),
            sub_agents: vec![],
            file_offset: 0, pending_tool: false,
        }
    }

    pub fn apply_event(&mut self, event: AgentEvent) {
        self.last_activity = Instant::now();
        match event {
            AgentEvent::ToolCall { tool_name, tool_input_summary, .. } => {
                self.last_tool = Some(tool_name);
                self.last_tool_input = Some(tool_input_summary);
                self.pending_tool = true;
                self.last_message = None;
                self.status = AgentStatus::Working;
            }
            AgentEvent::ToolResult { is_error, .. } => {
                self.pending_tool = false;
                self.status = if is_error { AgentStatus::Error } else { AgentStatus::Working };
            }
            AgentEvent::TextResponse { text, .. } => {
                self.last_message = Some(text);
                self.pending_tool = false;
                self.status = AgentStatus::WaitingForYou;
            }
            AgentEvent::SpawnSubAgent { task_tool_use_id, prompt_summary, .. } => {
                self.role = AgentRole::Lead;
                self.status = AgentStatus::Working;
                if !self.sub_agents.iter().any(|s| s.task_tool_use_id == task_tool_use_id) {
                    self.sub_agents.push(SubAgent {
                        task_tool_use_id, prompt_summary,
                        session_id: None, status: AgentStatus::Working,
                        last_tool: None, last_tool_input: None, last_activity: None,
                    });
                }
            }
            AgentEvent::Completed { is_error, .. } => {
                self.pending_tool = false;
                self.status = if is_error { AgentStatus::Error } else { AgentStatus::Completed };
            }
        }
    }

    pub fn apply_event_to_sub(&mut self, session_id: &str, event: AgentEvent) {
        let Some(sub) = self.sub_agents.iter_mut()
            .find(|s| s.session_id.as_deref() == Some(session_id)) else { return };
        sub.last_activity = Some(Instant::now());
        match event {
            AgentEvent::ToolCall { tool_name, tool_input_summary, .. } => {
                sub.last_tool = Some(tool_name);
                sub.last_tool_input = Some(tool_input_summary);
                sub.status = AgentStatus::Working;
            }
            AgentEvent::ToolResult { is_error, .. } => {
                sub.status = if is_error { AgentStatus::Error } else { AgentStatus::Working };
            }
            AgentEvent::TextResponse { .. } => { sub.status = AgentStatus::WaitingForYou; }
            AgentEvent::Completed { is_error, .. } => {
                sub.status = if is_error { AgentStatus::Error } else { AgentStatus::Completed };
            }
            _ => {}
        }
    }

    pub fn refresh_idle(&mut self) {
        if self.status == AgentStatus::Working
            && self.last_activity.elapsed() > Duration::from_secs(IDLE_THRESHOLD_SECS)
        {
            self.status = AgentStatus::Idle;
        }
    }

    pub fn short_id(&self) -> String { self.session_id.chars().take(8).collect() }

    pub fn elapsed_str(&self) -> String {
        let secs = self.last_activity.elapsed().as_secs();
        if secs < 60 { format!("{}s ago", secs) }
        else if secs < 3600 { format!("{}m{}s ago", secs / 60, secs % 60) }
        else { format!("{}h ago", secs / 3600) }
    }
}

// ── RepoLane ──────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct RepoLane {
    pub repo_name: String,
    pub project_path: String,
    pub agents: Vec<Agent>,
    /// Stack(s) Docker associées à ce repo
    /// Matching: compose project name contient le repo_name (case-insensitive)
    pub docker_stacks: Vec<DockerStack>,
}

impl RepoLane {
    pub fn new(project_path: String) -> Self {
        let repo_name = Path::new(&project_path)
            .file_name().and_then(|n| n.to_str()).unwrap_or(&project_path).to_string();
        RepoLane { repo_name, project_path, agents: vec![], docker_stacks: vec![] }
    }

    pub fn has_active_agents(&self) -> bool {
        self.agents.iter().any(|a| !matches!(a.status, AgentStatus::Completed))
    }

    pub fn has_docker(&self) -> bool {
        !self.docker_stacks.is_empty()
    }

    /// Vérifie si un nom de projet Compose correspond à ce repo
    pub fn matches_compose_project(&self, project_name: &str) -> bool {
        let repo_lower = self.repo_name.to_lowercase();
        let project_lower = project_name.to_lowercase();
        // Match si le project Compose contient le nom du repo, ou l'inverse
        project_lower.contains(&repo_lower) || repo_lower.contains(&project_lower)
    }
}

// ── SwarmState ────────────────────────────────────────────────────────────────

struct PendingChild {
    parent_tool_use_id: String,
    child_session_id: String,
    child_session_file: PathBuf,
}

#[derive(Default)]
pub struct SwarmState {
    pub lanes: HashMap<String, RepoLane>,
    pub session_index: HashMap<String, String>,   // session_id → project_path
    pub child_index: HashMap<String, String>,      // child_session_id → parent_session_id
    pending_children: Vec<PendingChild>,
    /// Stacks Docker non encore associées à une lane (en attente)
    pub unmatched_stacks: Vec<DockerStack>,
    /// Docker disponible ?
    pub docker_available: bool,
}

impl SwarmState {
    pub fn new() -> Self { SwarmState::default() }

    pub fn register_session(&mut self, session_id: &str, project_path: &str, session_file: PathBuf) {
        if self.child_index.contains_key(session_id) { return; }
        self.session_index.insert(session_id.to_string(), project_path.to_string());
        let lane = self.lanes.entry(project_path.to_string())
            .or_insert_with(|| RepoLane::new(project_path.to_string()));
        if !lane.agents.iter().any(|a| a.session_id == session_id) {
            lane.agents.push(Agent::new(session_id.to_string(), session_file));
        }
        // Tenter de matcher les stacks Docker en attente
        self.rematch_docker_stacks();
        self.resolve_pending();
    }

    pub fn register_child_session(
        &mut self, parent_tool_use_id: &str, child_session_id: &str, child_session_file: PathBuf,
    ) {
        if self.try_link(parent_tool_use_id, child_session_id) { return; }
        self.pending_children.push(PendingChild {
            parent_tool_use_id: parent_tool_use_id.to_string(),
            child_session_id: child_session_id.to_string(),
            child_session_file,
        });
    }

    fn try_link(&mut self, parent_tool_use_id: &str, child_session_id: &str) -> bool {
        for lane in self.lanes.values_mut() {
            for agent in lane.agents.iter_mut() {
                if let Some(sub) = agent.sub_agents.iter_mut()
                    .find(|s| s.task_tool_use_id == parent_tool_use_id && s.session_id.is_none())
                {
                    sub.session_id = Some(child_session_id.to_string());
                    self.child_index.insert(child_session_id.to_string(), agent.session_id.clone());
                    return true;
                }
            }
        }
        false
    }

    fn resolve_pending(&mut self) {
        let pending = std::mem::take(&mut self.pending_children);
        let mut still = Vec::new();
        for p in pending {
            if !self.try_link(&p.parent_tool_use_id, &p.child_session_id) {
                still.push(p);
            }
        }
        self.pending_children = still;
    }

    pub fn apply_event(&mut self, session_id: &str, event: AgentEvent) {
        if let Some(parent_id) = self.child_index.get(session_id).cloned() {
            if let Some(proj) = self.session_index.get(&parent_id).cloned() {
                if let Some(lane) = self.lanes.get_mut(&proj) {
                    if let Some(agent) = lane.agents.iter_mut().find(|a| a.session_id == parent_id) {
                        agent.apply_event_to_sub(session_id, event);
                        return;
                    }
                }
            }
        }
        if let Some(proj) = self.session_index.get(session_id).cloned() {
            if let Some(lane) = self.lanes.get_mut(&proj) {
                if let Some(agent) = lane.agents.iter_mut().find(|a| a.session_id == session_id) {
                    agent.apply_event(event);
                }
            }
        }
    }

    /// Met à jour toutes les stacks Docker depuis un snapshot complet
    pub fn update_docker_stacks(&mut self, stacks: Vec<DockerStack>) {
        self.docker_available = true;

        // Reset toutes les stacks dans les lanes
        for lane in self.lanes.values_mut() {
            lane.docker_stacks.clear();
        }
        self.unmatched_stacks.clear();

        for stack in stacks {
            if !self.try_assign_stack_to_lane(&stack) {
                self.unmatched_stacks.push(stack);
            }
        }
    }

    fn try_assign_stack_to_lane(&mut self, stack: &DockerStack) -> bool {
        let project = stack.project_name.clone();
        for lane in self.lanes.values_mut() {
            if lane.matches_compose_project(&project) {
                lane.docker_stacks.push(stack.clone());
                return true;
            }
        }
        false
    }

    fn rematch_docker_stacks(&mut self) {
        let unmatched = std::mem::take(&mut self.unmatched_stacks);
        for stack in unmatched {
            if !self.try_assign_stack_to_lane(&stack) {
                self.unmatched_stacks.push(stack);
            }
        }
    }

    /// Met à jour le statut d'un container suite à un event Docker temps réel
    pub fn apply_docker_event(&mut self, container_id: &str, new_status: ContainerStatus) {
        for lane in self.lanes.values_mut() {
            for stack in lane.docker_stacks.iter_mut() {
                for container in stack.containers.iter_mut() {
                    if container.id == container_id {
                        container.status = new_status;
                        return;
                    }
                }
            }
        }
    }

    pub fn tick(&mut self) {
        for lane in self.lanes.values_mut() {
            for agent in lane.agents.iter_mut() {
                agent.refresh_idle();
            }
        }
    }

    pub fn sorted_lanes(&self) -> Vec<&RepoLane> {
        let mut lanes: Vec<&RepoLane> = self.lanes.values().collect();
        lanes.sort_by(|a, b| {
            b.has_active_agents().cmp(&a.has_active_agents()).then(a.repo_name.cmp(&b.repo_name))
        });
        lanes
    }
}
EOF

# ── src/watcher.rs ────────────────────────────────────────────────────────────
cat > src/watcher.rs << 'EOF'
//! Polling des fichiers JSONL Claude Code toutes les 500ms.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::mpsc;
use anyhow::Result;

use crate::parser::{parse_line, Record, RecordType};

pub enum WatchEvent {
    SessionUpdate {
        session_id: String,
        project_path: String,
        session_file: PathBuf,
        new_records: Vec<Record>,
        is_new: bool,
    },
    ChildDiscovered {
        parent_tool_use_id: String,
        child_session_id: String,
        child_session_file: PathBuf,
        new_records: Vec<Record>,
    },
}

pub fn decode_project_path(encoded: &str) -> String {
    let decoded = encoded.replacen('-', "/", 1).replace('-', "/");
    if decoded.starts_with('/') { decoded } else { format!("/{}", decoded) }
}

fn claude_projects_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
        .join(".claude").join("projects")
}

async fn scan_sessions() -> Vec<(String, PathBuf)> {
    let base = claude_projects_dir();
    let mut result = Vec::new();
    let Ok(mut dirs) = fs::read_dir(&base).await else { return result };
    while let Ok(Some(entry)) = dirs.next_entry().await {
        let dir = entry.path();
        if !dir.is_dir() { continue; }
        let encoded = dir.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        let project_path = decode_project_path(&encoded);
        let sessions_dir = dir.join("sessions");
        let scan = if sessions_dir.exists() { sessions_dir } else { dir };
        if let Ok(mut entries) = fs::read_dir(&scan).await {
            while let Ok(Some(e)) = entries.next_entry().await {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                    result.push((project_path.clone(), p));
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

pub async fn watch_sessions(tx: mpsc::Sender<WatchEvent>) -> Result<()> {
    let mut offsets: HashMap<PathBuf, u64> = HashMap::new();
    let mut known: HashMap<PathBuf, String> = HashMap::new();
    let mut is_child: HashMap<PathBuf, bool> = HashMap::new();

    loop {
        for (project_path, session_file) in scan_sessions().await {
            let offset = offsets.entry(session_file.clone()).or_insert(0);
            let is_new = !known.contains_key(&session_file);
            let session_id = session_file.file_stem()
                .and_then(|s| s.to_str()).unwrap_or("unknown").to_string();
            known.insert(session_file.clone(), session_id.clone());

            let new_records = read_new_lines(&session_file, offset).await;

            if is_new {
                if let Some(parent_tool_use_id) = peek_parent_tool_use_id(&session_file).await {
                    is_child.insert(session_file.clone(), true);
                    let _ = tx.send(WatchEvent::ChildDiscovered {
                        parent_tool_use_id, child_session_id: session_id,
                        child_session_file: session_file.clone(), new_records,
                    }).await;
                    continue;
                }
                is_child.insert(session_file.clone(), false);
            }

            let child = is_child.get(&session_file).copied().unwrap_or(false);
            if !child && (!new_records.is_empty() || is_new) {
                let _ = tx.send(WatchEvent::SessionUpdate {
                    session_id, project_path, session_file: session_file.clone(),
                    new_records, is_new,
                }).await;
            } else if child && !new_records.is_empty() {
                let _ = tx.send(WatchEvent::ChildDiscovered {
                    parent_tool_use_id: String::new(),
                    child_session_id: session_id,
                    child_session_file: session_file.clone(),
                    new_records,
                }).await;
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }
}
EOF

# ── src/ui.rs ─────────────────────────────────────────────────────────────────
cat > src/ui.rs << 'EOF'
//! Rendu Ratatui: swim lanes imbriquées + section Docker par lane

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};
use crate::docker::DockerStack;
use crate::state::{Agent, AgentRole, AgentStatus, RepoLane, SubAgent, SwarmState};

const AGENT_CARD_W: u16 = 26;
const AGENT_CARD_H: u16 = 10;
const DOCKER_CARD_W: u16 = 22;
const DOCKER_CARD_H: u16 = 6;
const DOCKER_SECTION_H: u16 = DOCKER_CARD_H + 2; // cards + label ligne

pub fn render(frame: &mut Frame, state: &SwarmState) {
    let area = frame.area();
    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
    render_header(frame, chunks[0], state.docker_available);
    render_lanes(frame, chunks[1], state);
}

fn render_header(frame: &mut Frame, area: Rect, docker_ok: bool) {
    let docker_indicator = if docker_ok {
        Span::styled(" 🐳 docker:ok ", Style::default().fg(Color::Cyan))
    } else {
        Span::styled(" 🐳 docker:off ", Style::default().fg(Color::DarkGray))
    };
    let p = Paragraph::new(Line::from(vec![
        Span::styled(" 🐝 claude-swarm ", Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::styled("  monitoring your agent fleet  ", Style::default().fg(Color::DarkGray)),
        docker_indicator,
        Span::styled(" q:quit ", Style::default().fg(Color::DarkGray)),
    ]));
    frame.render_widget(p, area);
}

fn lane_height(lane: &RepoLane) -> u16 {
    let agents_h = lane.agents.len().max(1) as u16 * (AGENT_CARD_H + 1);
    let docker_h = if lane.has_docker() { DOCKER_SECTION_H } else { 0 };
    agents_h + docker_h + 2  // +2 pour la border de la lane
}

fn render_lanes(frame: &mut Frame, area: Rect, state: &SwarmState) {
    let lanes = state.sorted_lanes();
    if lanes.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled("  No active Claude Code sessions found.", Style::default().fg(Color::DarkGray))),
                Line::from(Span::styled("  Watching ~/.claude/projects/ ...", Style::default().fg(Color::DarkGray))),
            ]),
            area,
        );
        return;
    }
    let constraints: Vec<Constraint> = lanes.iter()
        .map(|l| Constraint::Length(lane_height(l)))
        .collect();
    let lane_areas = Layout::vertical(constraints).split(area);
    for (lane, lane_area) in lanes.iter().zip(lane_areas.iter()) {
        render_repo_lane(frame, *lane_area, lane);
    }
}

fn render_repo_lane(frame: &mut Frame, area: Rect, lane: &RepoLane) {
    let color = if lane.has_active_agents() { Color::Cyan } else { Color::DarkGray };
    let block = Block::default()
        .borders(Borders::ALL).border_type(BorderType::Double)
        .border_style(Style::default().fg(color))
        .title(Line::from(vec![
            Span::styled(" 🗂  ", Style::default().fg(Color::Yellow)),
            Span::styled(lane.repo_name.clone(), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Split inner: agents en haut, docker en bas si présent
    let docker_h = if lane.has_docker() { DOCKER_SECTION_H } else { 0 };
    let agent_h = inner.height.saturating_sub(docker_h);

    let sections = Layout::vertical([
        Constraint::Length(agent_h),
        Constraint::Length(docker_h),
    ]).split(inner);

    // Section agents
    if !lane.agents.is_empty() {
        let row_h = AGENT_CARD_H + 1;
        let row_constraints: Vec<Constraint> = lane.agents.iter()
            .map(|_| Constraint::Length(row_h)).collect();
        let rows = Layout::vertical(row_constraints).split(sections[0]);
        for (agent, row_area) in lane.agents.iter().zip(rows.iter()) {
            render_agent_row(frame, *row_area, agent);
        }
    }

    // Section Docker
    if lane.has_docker() {
        render_docker_section(frame, sections[1], &lane.docker_stacks);
    }
}

fn render_agent_row(frame: &mut Frame, area: Rect, agent: &Agent) {
    let total = 1 + agent.sub_agents.len();
    let mut constraints: Vec<Constraint> = (0..total).map(|_| Constraint::Length(AGENT_CARD_W)).collect();
    constraints.push(Constraint::Min(0));
    let areas = Layout::horizontal(constraints).split(area);
    render_agent_card(frame, areas[0], agent);
    for (i, sub) in agent.sub_agents.iter().enumerate() {
        if i + 1 < areas.len() - 1 {
            render_sub_card(frame, areas[i + 1], sub);
        }
    }
}

fn render_agent_card(frame: &mut Frame, area: Rect, agent: &Agent) {
    let border_color = match agent.status {
        AgentStatus::WaitingForYou => Color::Yellow,
        AgentStatus::Working       => Color::Green,
        AgentStatus::Error         => Color::Red,
        _                          => Color::DarkGray,
    };
    let role_icon  = if agent.role == AgentRole::Lead { "👑" } else { "⚙ " };
    let role_label = if agent.role == AgentRole::Lead { "Lead" } else { "Worker" };
    let block = Block::default()
        .borders(Borders::ALL).border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Line::from(Span::styled(
            format!(" {} {} ", role_icon, role_label),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let w = (AGENT_CARD_W - 4) as usize;
    let mut lines = vec![
        Line::from(vec![
            Span::styled("ID: ", Style::default().fg(Color::DarkGray)),
            Span::styled(agent.short_id(), Style::default().fg(Color::Gray)),
        ]),
        Line::from(Span::styled(agent.status.label(), Style::default().fg(agent.status.color()).add_modifier(Modifier::BOLD))),
        Line::from(""),
    ];
    if let Some(t) = &agent.last_tool {
        lines.push(Line::from(vec![
            Span::styled("tool: ", Style::default().fg(Color::DarkGray)),
            Span::styled(t.clone(), Style::default().fg(Color::Magenta)),
        ]));
        if let Some(inp) = &agent.last_tool_input {
            lines.push(Line::from(Span::styled(inp.chars().take(w).collect::<String>(), Style::default().fg(Color::Gray))));
        }
    }
    if let Some(msg) = &agent.last_message {
        lines.push(Line::from(Span::styled(msg.chars().take(w * 2).collect::<String>(), Style::default().fg(Color::Yellow))));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(agent.elapsed_str(), Style::default().fg(Color::DarkGray))));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

fn render_sub_card(frame: &mut Frame, area: Rect, sub: &SubAgent) {
    let block = Block::default()
        .borders(Borders::ALL).border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Blue))
        .title(Line::from(Span::styled(" ◎ Sub ", Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD))));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let w = (AGENT_CARD_W - 4) as usize;
    let lines = vec![
        Line::from(vec![
            Span::styled("ID: ", Style::default().fg(Color::DarkGray)),
            Span::styled(sub.task_tool_use_id.chars().take(8).collect::<String>(), Style::default().fg(Color::Gray)),
        ]),
        Line::from(Span::styled(sub.status.label(), Style::default().fg(sub.status.color()).add_modifier(Modifier::BOLD))),
        Line::from(""),
        Line::from(Span::styled(sub.prompt_summary.chars().take(w * 2).collect::<String>(), Style::default().fg(Color::Gray))),
        Line::from(""),
        if let Some(t) = &sub.last_tool {
            Line::from(vec![Span::styled("tool: ", Style::default().fg(Color::DarkGray)), Span::styled(t.clone(), Style::default().fg(Color::Magenta))])
        } else {
            Line::from(Span::styled("waiting...", Style::default().fg(Color::DarkGray)))
        },
        Line::from(Span::styled(sub.elapsed_str(), Style::default().fg(Color::DarkGray))),
    ];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

// ── Section Docker ─────────────────────────────────────────────────────────────

fn render_docker_section(frame: &mut Frame, area: Rect, stacks: &[DockerStack]) {
    // Label "🐳 <project-name>" + cards containers côte à côte
    let label_h = 1u16;
    let cards_h = area.height.saturating_sub(label_h);

    let sections = Layout::vertical([
        Constraint::Length(label_h),
        Constraint::Length(cards_h),
    ]).split(area);

    // On affiche toutes les stacks horizontalement
    for stack in stacks {
        let label = Paragraph::new(Line::from(vec![
            Span::styled("  🐳 ", Style::default().fg(Color::Cyan)),
            Span::styled(stack.project_name.clone(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled(
                if stack.all_healthy() { "  ✓ all healthy" } else { "  ⚠ issues" },
                Style::default().fg(if stack.all_healthy() { Color::Green } else { Color::Yellow }),
            ),
        ]));
        frame.render_widget(label, sections[0]);

        // Cards containers
        let n = stack.containers.len().max(1);
        let mut constraints: Vec<Constraint> = (0..n).map(|_| Constraint::Length(DOCKER_CARD_W)).collect();
        constraints.push(Constraint::Min(0));
        let card_areas = Layout::horizontal(constraints).split(sections[1]);

        for (i, container) in stack.containers.iter().enumerate() {
            if i < card_areas.len() - 1 {
                render_docker_card(frame, card_areas[i], container);
            }
        }
    }
}

fn render_docker_card(frame: &mut Frame, area: Rect, container: &crate::docker::ContainerInfo) {
    let border_color = container.status.color();
    let block = Block::default()
        .borders(Borders::ALL).border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Line::from(Span::styled(
            format!(" {} ", container.service_name()),
            Style::default().fg(Color::White),
        )));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let cpu_bar = mini_bar(container.cpu_percent, 100.0);
    let mem_pct = if container.mem_limit_mb > 0.0 {
        (container.mem_mb / container.mem_limit_mb) * 100.0
    } else { 0.0 };
    let mem_bar = mini_bar(mem_pct, 100.0);

    let lines = vec![
        Line::from(Span::styled(container.status.label(), Style::default().fg(border_color))),
        Line::from(""),
        Line::from(vec![
            Span::styled("cpu ", Style::default().fg(Color::DarkGray)),
            Span::styled(cpu_bar, Style::default().fg(Color::Green)),
            Span::styled(format!(" {:.1}%", container.cpu_percent), Style::default().fg(Color::Gray)),
        ]),
        Line::from(vec![
            Span::styled("mem ", Style::default().fg(Color::DarkGray)),
            Span::styled(mem_bar, Style::default().fg(Color::Blue)),
            Span::styled(format!(" {:.0}M", container.mem_mb), Style::default().fg(Color::Gray)),
        ]),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Petite barre de progression ASCII sur 6 chars
fn mini_bar(value: f64, max: f64) -> String {
    let pct = (value / max).clamp(0.0, 1.0);
    let filled = (pct * 6.0).round() as usize;
    let empty = 6 - filled;
    format!("[{}{}]", "█".repeat(filled), "░".repeat(empty))
}
EOF

# ── src/app.rs ────────────────────────────────────────────────────────────────
cat > src/app.rs << 'EOF'
//! Boucle principale: watcher JSONL + Docker + state + renderer

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::mpsc;

use crate::docker::{self, DockerEvent};
use crate::parser::extract_events;
use crate::state::SwarmState;
use crate::ui;
use crate::watcher::{watch_sessions, WatchEvent};

pub struct App {
    state: SwarmState,
}

impl App {
    pub async fn new() -> Result<Self> {
        Ok(App { state: SwarmState::new() })
    }

    pub async fn run(&mut self) -> Result<()> {
        enable_raw_mode()?;
        let mut stdout = std::io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        // Canal watcher JSONL
        let (watch_tx, mut watch_rx) = mpsc::channel::<WatchEvent>(256);
        tokio::spawn(async move { let _ = watch_sessions(watch_tx).await; });

        // Canal Docker (optionnel — si Docker n'est pas dispo on continue sans)
        let (docker_tx, mut docker_rx) = mpsc::channel::<DockerEvent>(64);
        match docker::connect().await {
            Ok(d) => {
                self.state.docker_available = true;
                let d2 = d.clone();
                let tx1 = docker_tx.clone();
                let tx2 = docker_tx.clone();
                // Poll périodique
                tokio::spawn(async move { let _ = docker::poll_loop(d, tx1).await; });
                // Stream d'events temps réel
                tokio::spawn(async move { let _ = docker::stream_events(d2, tx2).await; });
            }
            Err(e) => {
                // Docker non disponible — la TUI fonctionne quand même
                eprintln!("Docker unavailable: {e}");
            }
        }

        let tick = std::time::Duration::from_millis(250);

        loop {
            // Events JSONL
            while let Ok(event) = watch_rx.try_recv() {
                match event {
                    WatchEvent::SessionUpdate { session_id, project_path, session_file, new_records, .. } => {
                        self.state.register_session(&session_id, &project_path, session_file);
                        for e in extract_events(&new_records) {
                            self.state.apply_event(&session_id, e);
                        }
                    }
                    WatchEvent::ChildDiscovered { parent_tool_use_id, child_session_id, child_session_file, new_records } => {
                        if !parent_tool_use_id.is_empty() {
                            self.state.register_child_session(&parent_tool_use_id, &child_session_id, child_session_file);
                        }
                        for e in extract_events(&new_records) {
                            self.state.apply_event(&child_session_id, e);
                        }
                    }
                }
            }

            // Events Docker
            while let Ok(event) = docker_rx.try_recv() {
                match event {
                    DockerEvent::StacksSnapshot(stacks) => {
                        self.state.update_docker_stacks(stacks);
                    }
                    DockerEvent::ContainerStateChange { container_id, new_status } => {
                        self.state.apply_docker_event(&container_id, new_status);
                    }
                }
            }

            self.state.tick();
            terminal.draw(|f| ui::render(f, &self.state))?;

            if event::poll(tick)? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) { break; }
                    }
                }
            }
        }

        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        terminal.show_cursor()?;
        Ok(())
    }
}
EOF

# ── CLAUDE.md ─────────────────────────────────────────────────────────────────
cat > CLAUDE.md << 'EOF'
# claude-swarm — CLAUDE.md

TUI Ratatui en Rust pour monitorer en live les sessions Claude Code
ET les stacks Docker associées, groupées par repo en nested swim lanes.

## Build & run

```bash
cargo build --release
./target/release/claude-swarm
```

Rust >= 1.80 requis (bollard 0.17 nécessite edition2021+ récent).

## Architecture

```
src/
  main.rs    → entrée tokio async
  parser.rs  → désérialise JSONL Claude Code → AgentEvent
  state.rs   → SwarmState / RepoLane / Agent / SubAgent
  watcher.rs → poll 500ms sur ~/.claude/projects/
  docker.rs  → bollard: poll containers + stream events Docker
  ui.rs      → Ratatui: swim lanes + agent cards + docker cards
  app.rs     → boucle principale (deux canaux mpsc: JSONL + Docker)
```

## Format JSONL Claude Code

```
~/.claude/projects/<url-encoded-path>/sessions/<uuid>.jsonl
```

Types de records: user | assistant | tool_result | system | summary | result

Champs clés pour le monitoring:
- `message.content[].type == "tool_use"` → tool appelé (Bash, Read, Write, Task...)
- `message.content[].type == "text"` → réponse texte (souvent = waiting for you)
- `parentToolUseId` dans le 1er record `system` d'un fichier enfant
  → contient le `tool_use.id` du call `Task` dans le parent (linkage parent→enfant)
- `type == "result"` + `isError` → session terminée

## Linkage sub-agents (parentToolUseId)

Quand le Lead spawne un sub-agent via `Task`:
1. Parent écrit un `tool_use` avec `id: "toolu_01abc"` et `name: "Task"`
2. Enfant démarre, son 1er record `system` contient `parentToolUseId: "toolu_01abc"`
3. Le watcher détecte ça et émet `WatchEvent::ChildDiscovered`
4. `SwarmState.register_child_session()` fait le lien via `try_link()`
5. Race condition gérée par `pending_children` (si l'enfant arrive avant le parent)

## Monitoring Docker

Connexion via `/var/run/docker.sock` (bollard).

**Matching repo ↔ stack Compose :**
`com.docker.compose.project` label → `RepoLane.matches_compose_project()`
Matching case-insensitive, substring dans les deux sens.
Ex: repo `accurate-core` matche project `accuratecore-lead`.

**Deux sources de données :**
- `poll_loop()` : snapshot complet toutes les 2s (état + CPU/RAM)
- `stream_events()` : events temps réel start/stop/die pour transitions immédiates

**Stats CPU :** bollard retourne un stream — on prend 2 samples pour calculer le delta
(le 1er sample n'a pas de delta valide). Peut introduire ~1s de latence au refresh.

**Si Docker n'est pas disponible :** la TUI démarre quand même, l'indicateur
header affiche `🐳 docker:off` et les sections Docker sont absentes.

## État actuel (✅ implémenté)

- [x] Parser JSONL complet (7 types de records)
- [x] Machine à états agent: Working / WaitingForYou / Idle / Completed / Error
- [x] Détection rôle Lead (agent qui émet SpawnSubAgent)
- [x] Linkage parent→enfant via parentToolUseId + pending queue
- [x] Watcher tail-mode (offset par fichier, lit seulement les nouveaux bytes)
- [x] Docker: poll containers groupés par projet Compose
- [x] Docker: stream events temps réel
- [x] Docker: stats CPU % + RAM MB
- [x] UI: swim lane macro par repo (double border)
- [x] UI: agent cards horizontales (Lead + sub-agents à droite)
- [x] UI: section Docker en bas de chaque lane avec mini progress bars
- [x] Graceful degradation si Docker non disponible

## Prochaines étapes (P0 en premier)

### P0 — Validation sur vraies sessions
```bash
# Vérifier le décodage des paths
ls ~/.claude/projects/

# Vérifier que parentToolUseId existe bien dans tes sessions enfants
jq -r 'select(.type == "system") | .parentToolUseId // empty' \
  ~/.claude/projects/*/sessions/*.jsonl | head -5

# Vérifier les labels Docker de tes stacks
docker ps --format '{{.Labels}}' | tr ',' '\n' | grep compose
```

### P1 — Matching repo↔Docker à affiner
`decode_project_path()` dans watcher.rs fait un remplacement naïf `-` → `/`.
À valider et corriger selon le vrai encoding de ta version de Claude Code.

Le matching `matches_compose_project()` est substring-based — peut avoir des
faux positifs si des projets ont des noms similaires. Affiner si besoin.

### P2 — Scroll vertical
Si les lanes dépassent la hauteur du terminal, ajouter `scroll_offset: usize`
dans SwarmState + flèches ↑↓ dans app.rs.

### P3 — Filtre statut
Touche `f` → masquer IDLE/COMPLETED, ne garder que les actifs.

### P4 — Détail au survol  
Touche `Enter` sur une card → panel expandé en bas avec full message /
liste complète des tool calls récents.

### P5 — Notification WAITING FOR YOU
Bell ANSI (`\x07`) ou `notify-send` quand un agent passe en WaitingForYou.

### P6 — PID réel
Croiser avec `ps aux | grep claude` pour afficher le vrai PID du process
plutôt que le session_id tronqué.

### P7 — Logs container inline
Touche `l` sur une docker card → afficher les dernières lignes de logs
du container via `docker.logs()` bollard.
EOF

# ── Fin ───────────────────────────────────────────────────────────────────────
echo ""
echo "✅  Structure générée dans $DEST"
echo ""
echo "  cd $DEST"
echo "  cargo build --release"
echo "  ./target/release/claude-swarm"
echo ""
echo "  (Rust >= 1.80 requis pour bollard 0.17)"
