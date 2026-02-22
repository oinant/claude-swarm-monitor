//! Rendu Ratatui: swim lanes + vue detail avec navigation clavier

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};
use crate::docker::DockerStack;
use crate::state::{Agent, AgentRole, AgentStatus, RepoLane, SubAgent, SwarmState};

pub enum ViewMode {
    List { selected: usize },
    Detail { project_path: String, scroll: usize },
}

const AGENT_CARD_W: u16 = 26;
const AGENT_CARD_H: u16 = 10;
const DOCKER_SECTION_H: u16 = 2; // 1 ligne statut + 1 ligne stats

// ── Entrée principale ─────────────────────────────────────────────────────────

pub fn render(frame: &mut Frame, state: &SwarmState, view: &ViewMode) {
    let area = frame.area();
    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
    match view {
        ViewMode::List { selected } => {
            render_header(frame, chunks[0], state.docker_available, false);
            render_lanes(frame, chunks[1], state, *selected);
        }
        ViewMode::Detail { project_path, scroll } => {
            render_header(frame, chunks[0], state.docker_available, true);
            if let Some(lane) = state.lanes.get(project_path.as_str()) {
                render_detail_view(frame, chunks[1], lane, *scroll);
            }
        }
    }
}

// ── Header ────────────────────────────────────────────────────────────────────

fn render_header(frame: &mut Frame, area: Rect, docker_ok: bool, detail_mode: bool) {
    let docker_indicator = if docker_ok {
        Span::styled(" 🐳 docker:ok ", Style::default().fg(Color::Cyan))
    } else {
        Span::styled(" 🐳 docker:off ", Style::default().fg(Color::DarkGray))
    };
    let hints = if detail_mode {
        Span::styled(" ↑↓ scroll  Esc back  q quit ", Style::default().fg(Color::DarkGray))
    } else {
        Span::styled(" ↑↓ select  Enter detail  q quit ", Style::default().fg(Color::DarkGray))
    };
    frame.render_widget(Paragraph::new(Line::from(vec![
        Span::styled(" 🐝 claude-swarm ", Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::styled("  monitoring your agent fleet  ", Style::default().fg(Color::DarkGray)),
        docker_indicator,
        hints,
    ])), area);
}

// ── Mode liste ────────────────────────────────────────────────────────────────

/// Agents à afficher : actifs et récents en priorité, sinon le plus récent seulement.
fn visible_agents(lane: &RepoLane) -> Vec<&Agent> {
    const STALE_SECS: u64 = 300;
    let active: Vec<&Agent> = lane.agents.iter()
        .filter(|a| {
            !matches!(a.status, AgentStatus::Completed | AgentStatus::Idle)
            && a.last_activity.elapsed().as_secs() < STALE_SECS
        })
        .collect();
    if !active.is_empty() { return active; }
    lane.agents.iter().max_by_key(|a| a.last_activity).into_iter().collect()
}

/// Sub-agents à afficher en mode liste : seulement les actifs (pas Completed)
fn visible_subs(agent: &Agent) -> Vec<&SubAgent> {
    agent.sub_agents.iter()
        .filter(|s| !matches!(s.status, AgentStatus::Completed))
        .collect()
}

fn lane_height(lane: &RepoLane) -> u16 {
    let agents = visible_agents(lane);
    let agents_h = if lane.is_scanning && agents.is_empty() {
        2
    } else if agents.is_empty() {
        2
    } else {
        agents.len() as u16 * (AGENT_CARD_H + 1)
    };
    let docker_h = if lane.has_docker() { DOCKER_SECTION_H } else { 0 };
    agents_h + docker_h + 2
}

fn render_lanes(frame: &mut Frame, area: Rect, state: &SwarmState, selected: usize) {
    let lanes = state.sorted_lanes();
    if lanes.is_empty() {
        frame.render_widget(Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled("  No active Claude Code sessions found.", Style::default().fg(Color::DarkGray))),
            Line::from(Span::styled("  Watching ~/.claude/projects/ ...", Style::default().fg(Color::DarkGray))),
        ]), area);
        return;
    }
    let constraints: Vec<Constraint> = lanes.iter()
        .map(|l| Constraint::Length(lane_height(l)))
        .collect();
    let lane_areas = Layout::vertical(constraints).split(area);
    for (i, (lane, lane_area)) in lanes.iter().zip(lane_areas.iter()).enumerate() {
        render_repo_lane(frame, *lane_area, lane, i == selected);
    }
}

fn render_repo_lane(frame: &mut Frame, area: Rect, lane: &RepoLane, is_selected: bool) {
    let base_color = lane_base_color(lane);
    let color = if is_selected { Color::White } else { base_color };
    let (prefix, icon) = if lane.is_lead { ("* LEAD  ", "★ ") } else { ("  ", "⚙ ") };
    let name_style = if is_selected {
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
    } else if lane.is_lead {
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

    let docker_h = if lane.has_docker() { DOCKER_SECTION_H } else { 0 };
    let agent_h = inner.height.saturating_sub(docker_h);
    let sections = Layout::vertical([
        Constraint::Length(agent_h),
        Constraint::Length(docker_h),
    ]).split(inner);

    let agents = visible_agents(lane);
    if lane.is_scanning && agents.is_empty() {
        frame.render_widget(Paragraph::new(Line::from(Span::styled(
            "  ⟳ scanning sessions...", Style::default().fg(Color::DarkGray),
        ))), sections[0]);
    } else if agents.is_empty() {
        frame.render_widget(Paragraph::new(Line::from(Span::styled(
            "  ◌ no recent sessions", Style::default().fg(Color::DarkGray),
        ))), sections[0]);
    } else {
        let row_h = AGENT_CARD_H + 1;
        let row_constraints: Vec<Constraint> = agents.iter().map(|_| Constraint::Length(row_h)).collect();
        let rows = Layout::vertical(row_constraints).split(sections[0]);
        for (agent, row_area) in agents.iter().zip(rows.iter()) {
            render_agent_row(frame, *row_area, agent);
        }
    }

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
    let border_color = agent_status_color(&agent.status);
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
        Line::from(Span::styled(agent.status.label(), Style::default().fg(border_color).add_modifier(Modifier::BOLD))),
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

fn render_docker_section(frame: &mut Frame, area: Rect, stacks: &[DockerStack]) {
    if area.height < 2 { return; }
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);

    for stack in stacks {
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

        let max_cpu = stack.containers.iter().map(|c| c.cpu_percent).fold(0.0_f64, f64::max);
        let max_mem = stack.containers.iter().map(|c| c.mem_mb).fold(0.0_f64, f64::max);
        let mem_str = fmt_mem(max_mem);
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

// ── Vue detail ────────────────────────────────────────────────────────────────

fn render_detail_view(frame: &mut Frame, area: Rect, lane: &RepoLane, scroll: usize) {
    let base_color = lane_base_color(lane);
    let (prefix, icon) = if lane.is_lead { ("* LEAD  ", "★ ") } else { ("  ", "⚙ ") };
    let block = Block::default()
        .borders(Borders::ALL).border_type(BorderType::Double)
        .border_style(Style::default().fg(base_color))
        .title(Line::from(vec![
            Span::styled(format!(" {} ", icon), Style::default().fg(base_color)),
            Span::styled(
                format!("{}{}", prefix, lane.repo_name),
                Style::default().fg(if lane.is_lead { Color::Yellow } else { Color::White }).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  — DETAIL  ", Style::default().fg(Color::DarkGray)),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let w = inner.width as usize;
    let sep = "─".repeat(w);
    let mut lines: Vec<Line> = vec![];

    // ── Agents ──
    let agents = visible_agents(lane);
    for agent in &agents {
        lines.extend(agent_detail_lines(agent, &sep));
        // Sub-agents : tous en detail (y compris completed, grisés)
        for sub in &agent.sub_agents {
            lines.extend(sub_detail_lines(sub, &sep));
        }
    }
    if agents.is_empty() {
        lines.push(Line::from(Span::styled("  ◌ no recent sessions", Style::default().fg(Color::DarkGray))));
        lines.push(Line::from(""));
    }

    // ── Docker ──
    if !lane.docker_stacks.is_empty() {
        for stack in &lane.docker_stacks {
            lines.push(Line::from(Span::styled(
                format!("  🐳  {}", stack.project_name),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(sep.clone(), Style::default().fg(Color::DarkGray))));
            for c in &stack.containers {
                lines.push(container_detail_line(c));
            }
            lines.push(Line::from(""));
        }
    }

    frame.render_widget(
        Paragraph::new(lines).scroll((scroll as u16, 0)),
        inner,
    );
}

fn agent_detail_lines<'a>(agent: &'a Agent, sep: &str) -> Vec<Line<'a>> {
    let sc = agent_status_color(&agent.status);
    let role_icon  = if agent.role == AgentRole::Lead { "❯❯" } else { "❯ " };
    let role_label = if agent.role == AgentRole::Lead { "Orchestrator" } else { "Agent" };

    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("  {} {} ", role_icon, role_label), Style::default().fg(sc).add_modifier(Modifier::BOLD)),
            Span::styled(agent.session_id.clone(), Style::default().fg(Color::Gray)),
            Span::styled("   ", Style::default()),
            Span::styled(agent.status.label(), Style::default().fg(sc).add_modifier(Modifier::BOLD)),
            Span::styled(format!("   {}", agent.elapsed_str()), Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(Span::styled(sep.to_string(), Style::default().fg(Color::DarkGray))),
    ];
    if let Some(t) = &agent.last_tool {
        lines.push(Line::from(vec![
            Span::styled("  tool: ", Style::default().fg(Color::DarkGray)),
            Span::styled(t.clone(), Style::default().fg(Color::Magenta)),
        ]));
        if let Some(inp) = &agent.last_tool_input {
            lines.push(Line::from(Span::styled(format!("  {}", inp), Style::default().fg(Color::Gray))));
        }
    }
    if let Some(msg) = &agent.last_message {
        for l in msg.lines() {
            lines.push(Line::from(Span::styled(format!("  {}", l), Style::default().fg(Color::Yellow))));
        }
    }
    lines.push(Line::from(""));
    lines
}

fn sub_detail_lines<'a>(sub: &'a SubAgent, sep: &str) -> Vec<Line<'a>> {
    let is_done = matches!(sub.status, AgentStatus::Completed);
    let sc = sub.status.color();
    let dim = if is_done { Color::DarkGray } else { Color::Blue };

    let mut lines = vec![
        Line::from(vec![
            Span::styled("    ◎ Sub  ", Style::default().fg(dim).add_modifier(Modifier::BOLD)),
            Span::styled(sub.task_tool_use_id.chars().take(12).collect::<String>(), Style::default().fg(Color::DarkGray)),
            Span::styled("   ", Style::default()),
            Span::styled(sub.status.label(), Style::default().fg(sc)),
            Span::styled(format!("   {}", sub.elapsed_str()), Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(Span::styled(format!("    {}", sep.chars().skip(1).collect::<String>()), Style::default().fg(Color::DarkGray))),
    ];
    if !sub.prompt_summary.is_empty() {
        lines.push(Line::from(Span::styled(format!("    {}", sub.prompt_summary), Style::default().fg(Color::DarkGray))));
    }
    if let Some(t) = &sub.last_tool {
        lines.push(Line::from(vec![
            Span::styled("    tool: ", Style::default().fg(Color::DarkGray)),
            Span::styled(t.clone(), Style::default().fg(if is_done { Color::DarkGray } else { Color::Magenta })),
        ]));
    }
    lines.push(Line::from(""));
    lines
}

fn container_detail_line<'a>(c: &'a crate::docker::ContainerInfo) -> Line<'a> {
    let (dot, color) = match &c.status {
        crate::docker::ContainerStatus::Running     => ('●', Color::Green),
        crate::docker::ContainerStatus::Restarting  => ('◉', Color::Yellow),
        crate::docker::ContainerStatus::Exited(0)   => ('○', Color::DarkGray),
        crate::docker::ContainerStatus::Exited(_)
        | crate::docker::ContainerStatus::Dead      => ('●', Color::Red),
        _                                            => ('·', Color::DarkGray),
    };
    let name = format!("  {:<16}", c.service_name().chars().take(16).collect::<String>());
    let cpu_bar = mini_bar(c.cpu_percent, 10.0);
    let mem_str = fmt_mem(c.mem_mb);

    Line::from(vec![
        Span::styled(name, Style::default().fg(Color::White)),
        Span::styled(dot.to_string(), Style::default().fg(color)),
        Span::styled("  cpu ", Style::default().fg(Color::DarkGray)),
        Span::styled(cpu_bar, Style::default().fg(Color::Green)),
        Span::styled(format!(" {:>5.1}%", c.cpu_percent), Style::default().fg(Color::Gray)),
        Span::styled("  mem ", Style::default().fg(Color::DarkGray)),
        Span::styled(mini_bar(c.mem_mb, 4096.0), Style::default().fg(Color::Blue)),
        Span::styled(format!(" {:>6}", mem_str), Style::default().fg(Color::Gray)),
    ])
}

// ── Utilitaires ───────────────────────────────────────────────────────────────

fn lane_base_color(lane: &RepoLane) -> Color {
    if lane.is_lead {
        if lane.has_active_agents() { Color::Yellow } else { Color::DarkGray }
    } else {
        if lane.has_active_agents() { Color::Cyan } else { Color::DarkGray }
    }
}

fn agent_status_color(status: &AgentStatus) -> Color {
    match status {
        AgentStatus::WaitingForYou => Color::Yellow,
        AgentStatus::Working       => Color::Green,
        AgentStatus::Error         => Color::Red,
        _                          => Color::DarkGray,
    }
}

fn fmt_mem(mb: f64) -> String {
    if mb >= 1024.0 { format!("{:.1}G", mb / 1024.0) } else { format!("{:.0}M", mb) }
}

fn mini_bar(value: f64, max: f64) -> String {
    let pct = (value / max).clamp(0.0, 1.0);
    let filled = (pct * 6.0).round() as usize;
    format!("[{}{}]", "█".repeat(filled), "░".repeat(6 - filled))
}
