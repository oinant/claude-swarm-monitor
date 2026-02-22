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
use futures_util::{StreamExt, future::join_all};
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

    #[allow(dead_code)]
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

    #[allow(dead_code)]
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
    #[allow(dead_code)]
    pub image: String,        // image:tag
    pub status: ContainerStatus,
    pub compose_service: Option<String>,  // label com.docker.compose.service
    pub compose_project: Option<String>,  // label com.docker.compose.project
    pub cpu_percent: f64,
    pub mem_mb: f64,
    pub mem_limit_mb: f64,
    #[allow(dead_code)]
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
    #[allow(dead_code)]
    pub fn has_running(&self) -> bool {
        self.containers.iter().any(|c| c.status.is_active())
    }

    #[allow(dead_code)]
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

    // Enrichir avec les stats CPU/RAM en parallèle (best-effort)
    let mut result = Vec::new();
    for (project_name, mut containers) in stacks {
        let futs: Vec<_> = containers.iter()
            .map(|c| {
                let id = c.id.clone();
                let active = c.status.is_active();
                async move {
                    if active { fetch_stats(docker, &id).await.ok() } else { None }
                }
            })
            .collect();
        let stats_results = join_all(futs).await;
        for (container, stats) in containers.iter_mut().zip(stats_results) {
            if let Some((cpu, mem, mem_limit)) = stats {
                container.cpu_percent = cpu;
                container.mem_mb = mem;
                container.mem_limit_mb = mem_limit;
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
    let status_str = c.state.unwrap_or_default().to_lowercase();
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

/// Récupère un snapshot stats CPU/RAM — équivalent à `docker stats --no-stream`.
/// `stream: false, one_shot: false` retourne un seul sample AVEC precpu_stats
/// (contrairement à one_shot: true qui vide precpu_stats → delta CPU = 0).
async fn fetch_stats(docker: &Docker, container_id: &str) -> Result<(f64, f64, f64)> {
    let mut stream = docker.stats(container_id, Some(StatsOptions {
        stream: false,
        one_shot: false,
    }));

    let s = match stream.next().await {
        Some(Ok(s)) => s,
        _ => return Ok((0.0, 0.0, 0.0)),
    };

    // CPU % via delta precpu_stats → cpu_stats (même logique que le CLI docker stats)
    let cpu_delta = s.cpu_stats.cpu_usage.total_usage as f64
        - s.precpu_stats.cpu_usage.total_usage as f64;
    let sys_delta = s.cpu_stats.system_cpu_usage.unwrap_or(0) as f64
        - s.precpu_stats.system_cpu_usage.unwrap_or(0) as f64;
    let num_cpus = s.cpu_stats.online_cpus.unwrap_or(1) as f64;
    let cpu_pct = if sys_delta > 0.0 { (cpu_delta / sys_delta) * num_cpus * 100.0 } else { 0.0 };

    // RAM en MB
    let mem_mb = s.memory_stats.usage.unwrap_or(0) as f64 / 1_048_576.0;
    let mem_limit_mb = s.memory_stats.limit.unwrap_or(1) as f64 / 1_048_576.0;

    Ok((cpu_pct, mem_mb, mem_limit_mb))
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
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    }
}
