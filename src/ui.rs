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
const DOCKER_SECTION_H: u16 = 2; // 1 ligne statut + 1 ligne stats

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

/// Agents à afficher : actifs et récents en priorité, sinon le plus récent seulement.
/// Les sessions "fantômes" (Working mais sans activité depuis >5 min) sont ignorées.
fn visible_agents(lane: &RepoLane) -> Vec<&Agent> {
    const STALE_SECS: u64 = 300; // 5 minutes
    let active: Vec<&Agent> = lane.agents.iter()
        .filter(|a| {
            !matches!(a.status, AgentStatus::Completed | AgentStatus::Idle)
            && a.last_activity.elapsed().as_secs() < STALE_SECS
        })
        .collect();
    if !active.is_empty() { return active; }
    // Toutes sessions terminées/idle/stale → montrer seulement la plus récente
    lane.agents.iter()
        .max_by_key(|a| a.last_activity)
        .into_iter()
        .collect()
}

/// Sub-agents à afficher : seulement les actifs (pas Completed)
fn visible_subs(agent: &Agent) -> Vec<&SubAgent> {
    agent.sub_agents.iter()
        .filter(|s| !matches!(s.status, AgentStatus::Completed))
        .collect()
}

fn lane_height(lane: &RepoLane) -> u16 {
    let agents = visible_agents(lane);
    let agents_h = if lane.is_scanning && agents.is_empty() {
        2  // "⟳ scanning..." placeholder
    } else if agents.is_empty() {
        2  // "◌ no recent sessions" placeholder
    } else {
        agents.len() as u16 * (AGENT_CARD_H + 1)
    };
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
    let (color, prefix, icon) = if lane.is_lead {
        let c = if lane.has_active_agents() { Color::Yellow } else { Color::DarkGray };
        (c, "* LEAD  ", "★ ")
    } else {
        let c = if lane.has_active_agents() { Color::Cyan } else { Color::DarkGray };
        (c, "  ", "⚙ ")
    };
    let name_style = if lane.is_lead {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
    };
    let block = Block::default()
        .borders(Borders::ALL).border_type(BorderType::Double)
        .border_style(Style::default().fg(color))
        .title(Line::from(vec![
            Span::styled(format!(" {} ", icon), Style::default().fg(color)),
            Span::styled(format!("{}{}", prefix, lane.repo_name), name_style),
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
    let agents = visible_agents(lane);
    if lane.is_scanning && agents.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  ⟳ scanning sessions...",
                Style::default().fg(Color::DarkGray),
            ))),
            sections[0],
        );
    } else if agents.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  ◌ no recent sessions",
                Style::default().fg(Color::DarkGray),
            ))),
            sections[0],
        );
    } else if !agents.is_empty() {
        let row_h = AGENT_CARD_H + 1;
        let row_constraints: Vec<Constraint> = agents.iter()
            .map(|_| Constraint::Length(row_h)).collect();
        let rows = Layout::vertical(row_constraints).split(sections[0]);
        for (agent, row_area) in agents.iter().zip(rows.iter()) {
            render_agent_row(frame, *row_area, agent);
        }
    }

    // Section Docker
    if lane.has_docker() {
        render_docker_section(frame, sections[1], &lane.docker_stacks);
    }
}

fn render_agent_row(frame: &mut Frame, area: Rect, agent: &Agent) {
    let subs = visible_subs(agent);
    let total = 1 + subs.len();
    let mut constraints: Vec<Constraint> = (0..total).map(|_| Constraint::Length(AGENT_CARD_W)).collect();
    constraints.push(Constraint::Min(0));
    let areas = Layout::horizontal(constraints).split(area);
    render_agent_card(frame, areas[0], agent);
    for (i, sub) in subs.iter().enumerate() {
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
    let role_icon  = if agent.role == AgentRole::Lead { "❯❯" } else { "❯ " };
    let role_label = if agent.role == AgentRole::Lead { "Orchestrator" } else { "Agent" };
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

// ── Section Docker compacte (2 lignes par stack) ──────────────────────────────
//   ligne 1 : 🐳 <projet>  ●●○●...  n stopped
//   ligne 2 :      cpu [██░░░░] 6.5%  mem [████░░] 2.5G  max

fn render_docker_section(frame: &mut Frame, area: Rect, stacks: &[DockerStack]) {
    if area.height < 2 { return; }
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
    ]).split(area);

    for stack in stacks {
        // Ligne 1 : nom + un dot coloré par container + résumé
        let stopped = stack.containers.iter().filter(|c| !c.status.is_active()).count();
        let health_span = if stopped == 0 {
            Span::styled("  ✓", Style::default().fg(Color::Green))
        } else {
            Span::styled(format!("  ⚠ {} stopped", stopped), Style::default().fg(Color::Yellow))
        };

        let mut status_spans: Vec<Span> = vec![
            Span::styled("  🐳 ", Style::default().fg(Color::Cyan)),
            Span::styled(stack.project_name.clone(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
        ];
        for c in &stack.containers {
            let (dot, color) = match &c.status {
                crate::docker::ContainerStatus::Running     => ('●', Color::Green),
                crate::docker::ContainerStatus::Restarting  => ('◉', Color::Yellow),
                crate::docker::ContainerStatus::Exited(0)   => ('○', Color::DarkGray),
                crate::docker::ContainerStatus::Exited(_)
                | crate::docker::ContainerStatus::Dead      => ('●', Color::Red),
                _                                            => ('·', Color::DarkGray),
            };
            status_spans.push(Span::styled(dot.to_string(), Style::default().fg(color)));
        }
        status_spans.push(health_span);
        frame.render_widget(Paragraph::new(Line::from(status_spans)), rows[0]);

        // Ligne 2 : stats max parmi les containers actifs
        let max_cpu = stack.containers.iter().map(|c| c.cpu_percent).fold(0.0_f64, f64::max);
        let max_mem = stack.containers.iter().map(|c| c.mem_mb).fold(0.0_f64, f64::max);
        let mem_str = if max_mem >= 1024.0 {
            format!("{:.1}G", max_mem / 1024.0)
        } else {
            format!("{:.0}M", max_mem)
        };
        frame.render_widget(Paragraph::new(Line::from(vec![
            Span::styled("       cpu ", Style::default().fg(Color::DarkGray)),
            Span::styled(mini_bar(max_cpu, 10.0), Style::default().fg(Color::Green)),
            Span::styled(format!(" {:.1}%", max_cpu), Style::default().fg(Color::Gray)),
            Span::styled("  mem ", Style::default().fg(Color::DarkGray)),
            Span::styled(mini_bar(max_mem, 4096.0), Style::default().fg(Color::Blue)),
            Span::styled(format!(" {}  max", mem_str), Style::default().fg(Color::Gray)),
        ])), rows[1]);
    }
}

/// Petite barre de progression ASCII sur 6 chars
fn mini_bar(value: f64, max: f64) -> String {
    let pct = (value / max).clamp(0.0, 1.0);
    let filled = (pct * 6.0).round() as usize;
    let empty = 6 - filled;
    format!("[{}{}]", "█".repeat(filled), "░".repeat(empty))
}
