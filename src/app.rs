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
    lead_path: String,
}

impl App {
    pub async fn new(lead_path: Option<String>) -> Result<Self> {
        let lead_path = lead_path.unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| ".".to_string())
        });
        Ok(App { state: SwarmState::new(), lead_path })
    }

    pub async fn run(&mut self) -> Result<()> {
        enable_raw_mode()?;
        let mut stdout = std::io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        // Canal watcher JSONL
        let (watch_tx, mut watch_rx) = mpsc::channel::<WatchEvent>(256);
        let lead_path = self.lead_path.clone();
        tokio::spawn(async move { let _ = watch_sessions(watch_tx, lead_path).await; });

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
                    WatchEvent::LanesDiscovered { paths } => {
                        self.state.discover_lanes(paths);
                    }
                    WatchEvent::ScanComplete => {
                        self.state.mark_scan_complete();
                    }
                    WatchEvent::SessionUpdate { session_id, project_path, session_file, new_records, compose_project_name, is_lead } => {
                        self.state.register_session(&session_id, &project_path, session_file, compose_project_name, is_lead);
                        for e in extract_events(&new_records) {
                            self.state.apply_event(&session_id, e);
                        }
                    }
                    WatchEvent::ChildDiscovered { parent_tool_use_id, parent_session_id, child_session_id, child_session_file, new_records } => {
                        if !parent_tool_use_id.is_empty() || parent_session_id.is_some() {
                            self.state.register_child_session(
                                &parent_tool_use_id,
                                parent_session_id.as_deref(),
                                &child_session_id,
                                child_session_file,
                            );
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
