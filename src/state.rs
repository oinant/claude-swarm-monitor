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
    #[allow(dead_code)]
    pub session_file: PathBuf,
    pub role: AgentRole,
    pub status: AgentStatus,
    pub last_tool: Option<String>,
    pub last_tool_input: Option<String>,
    pub last_message: Option<String>,
    pub last_activity: Instant,
    pub sub_agents: Vec<SubAgent>,
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
            pending_tool: false,
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
    #[allow(dead_code)]
    pub project_path: String,
    pub is_lead: bool,
    pub is_scanning: bool,
    pub compose_project_name: Option<String>,
    pub agents: Vec<Agent>,
    pub docker_stacks: Vec<DockerStack>,
}

impl RepoLane {
    pub fn new(project_path: String, compose_project_name: Option<String>, is_lead: bool) -> Self {
        let repo_name = Path::new(&project_path)
            .file_name().and_then(|n| n.to_str()).unwrap_or(&project_path).to_string();
        RepoLane { repo_name, project_path, is_lead, is_scanning: true, compose_project_name, agents: vec![], docker_stacks: vec![] }
    }

    pub fn has_active_agents(&self) -> bool {
        self.agents.iter().any(|a| !matches!(a.status, AgentStatus::Completed))
    }

    pub fn has_docker(&self) -> bool {
        !self.docker_stacks.is_empty()
    }

    /// Vérifie si un nom de projet Compose correspond à ce lane (exact, case-insensitive)
    pub fn matches_compose_project(&self, project_name: &str) -> bool {
        self.compose_project_name.as_deref()
            .map(|n| n.eq_ignore_ascii_case(project_name))
            .unwrap_or(false)
    }
}

// ── SwarmState ────────────────────────────────────────────────────────────────

struct PendingChild {
    parent_tool_use_id: String,
    parent_session_id: Option<String>,
    child_session_id: String,
    #[allow(dead_code)]
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

    /// Crée les lanes vides immédiatement (avant tout scan de sessions)
    pub fn discover_lanes(&mut self, paths: Vec<(String, bool, Option<String>)>) {
        for (path, is_lead, compose_name) in paths {
            self.lanes.entry(path.clone())
                .or_insert_with(|| RepoLane::new(path, compose_name, is_lead));
        }
    }

    /// Fin du scan initial : toutes les lanes passent en mode "prêt"
    pub fn mark_scan_complete(&mut self) {
        for lane in self.lanes.values_mut() {
            lane.is_scanning = false;
        }
    }

    pub fn register_session(
        &mut self,
        session_id: &str,
        project_path: &str,
        session_file: PathBuf,
        compose_project_name: Option<String>,
        is_lead: bool,
    ) {
        if self.child_index.contains_key(session_id) { return; }
        self.session_index.insert(session_id.to_string(), project_path.to_string());
        let lane = self.lanes.entry(project_path.to_string())
            .or_insert_with(|| RepoLane::new(project_path.to_string(), compose_project_name, is_lead));
        lane.is_scanning = false;
        if !lane.agents.iter().any(|a| a.session_id == session_id) {
            lane.agents.push(Agent::new(session_id.to_string(), session_file));
        }
        // Tenter de matcher les stacks Docker en attente
        self.rematch_docker_stacks();
        self.resolve_pending();
    }

    pub fn register_child_session(
        &mut self,
        parent_tool_use_id: &str,
        parent_session_id: Option<&str>,
        child_session_id: &str,
        child_session_file: PathBuf,
    ) {
        // Essai par session_id si fourni
        if let Some(psid) = parent_session_id {
            if self.try_link_by_session_id(psid, child_session_id) { return; }
        }
        // Essai par tool_use_id
        if !parent_tool_use_id.is_empty() {
            if self.try_link(parent_tool_use_id, child_session_id) { return; }
        }
        // En attente
        self.pending_children.push(PendingChild {
            parent_tool_use_id: parent_tool_use_id.to_string(),
            parent_session_id: parent_session_id.map(|s| s.to_string()),
            child_session_id: child_session_id.to_string(),
            child_session_file,
        });
    }

    fn try_link_by_session_id(&mut self, parent_session_id: &str, child_session_id: &str) -> bool {
        if let Some(proj) = self.session_index.get(parent_session_id).cloned() {
            if let Some(lane) = self.lanes.get_mut(&proj) {
                if let Some(agent) = lane.agents.iter_mut().find(|a| a.session_id == parent_session_id) {
                    if !agent.sub_agents.iter().any(|s| s.task_tool_use_id == child_session_id) {
                        agent.role = AgentRole::Lead;
                        agent.sub_agents.push(SubAgent {
                            task_tool_use_id: child_session_id.to_string(),
                            prompt_summary: String::new(),
                            session_id: Some(child_session_id.to_string()),
                            status: AgentStatus::Working,
                            last_tool: None, last_tool_input: None, last_activity: None,
                        });
                    }
                    self.child_index.insert(child_session_id.to_string(), parent_session_id.to_string());
                    return true;
                }
            }
        }
        false
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
            let mut linked = false;
            if let Some(ref psid) = p.parent_session_id {
                linked = self.try_link_by_session_id(psid, &p.child_session_id);
            }
            if !linked && !p.parent_tool_use_id.is_empty() {
                linked = self.try_link(&p.parent_tool_use_id, &p.child_session_id);
            }
            if !linked {
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
            b.is_lead.cmp(&a.is_lead)
                .then(a.repo_name.cmp(&b.repo_name))
        });
        lanes
    }
}
