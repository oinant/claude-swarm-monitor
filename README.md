# claude-swarm-monitor

A terminal dashboard for multi-agent Claude Code workflows.

## The problem it solves

When you run Claude Code across several git worktrees in parallel — each agent working on its own branch — it's hard to know at a glance which agent is done, which is still working, and which is waiting for your input. You end up switching between terminals or watching multiple windows.

**claude-swarm-monitor** gives you a single pane of glass: one swim lane per agent, live status, and Docker stack health, all in your terminal.

## What it does

- **One swim lane per agent** — lead repo + each git worktree gets its own lane, sorted lead-first
- **Live status** — `Working` / `Waiting For You` / `Idle` / `Done` / `Error`, streamed directly from Claude Code's JSONL session files
- **Sub-agent tracking** — when an agent spawns sub-agents via the `Task` tool, they appear as cards within the parent lane
- **Docker stacks** — each lane shows its associated Compose stack (matched via `COMPOSE_PROJECT_NAME` in `docker/.env`), with live CPU % and memory stats
- **Keyboard navigation** — `↑↓` to select a lane, `Enter` for a full detail view (agents + containers), `Esc` to go back, `q` to quit

In short: keep this monitor open in a corner, and the moment any agent switches to **Waiting For You**, you know exactly where to focus.

## Prerequisites

- **Rust ≥ 1.80** (uses `edition = "2021"` with recent async features)
- **Claude Code** with at least one active session (reads `~/.claude/projects/`)
- **Docker** *(optional)* — if available, container stats are shown per lane; the TUI starts fine without it

## Build & run

```bash
git clone https://github.com/oinant/claude-swarm-monitor
cd claude-swarm-monitor
cargo build --release
```

```bash
# Monitor the current directory (lead repo)
./target/release/claude-swarm

# Or pass the lead repo path explicitly
./target/release/claude-swarm /path/to/your/project
```

Git worktrees are discovered automatically from `.git/worktrees/`.

## Docker matching

Each worktree can have its own Compose stack. The monitor matches lanes to stacks via:

```
<project_path>/docker/.env   →   COMPOSE_PROJECT_NAME=your-stack-name
```

If this file exists, the lane's Docker section shows only the containers belonging to that stack.

## Keyboard shortcuts

| Key      | Action                              |
|----------|-------------------------------------|
| `↑` `↓`  | Select lane (list view)             |
| `Enter`  | Open detail view for selected lane  |
| `↑` `↓`  | Scroll detail view                  |
| `Esc`    | Back to list                        |
| `q`      | Quit                                |

## Contributing

Contributions are very welcome — this tool scratches a real itch and there's plenty of room to grow.

A few areas where help would be great:

- **Notifications** — bell or `notify-send` when an agent switches to *Waiting For You*
- **Filter mode** — toggle to hide idle/completed lanes and focus on active ones
- **Log viewer** — inline container logs via `docker logs` (bollard already connected)
- **Scroll in list view** — vertical scroll when lanes exceed terminal height
- **Windows / Docker Desktop support** — currently tested on Linux only

To get started: fork, `cargo build`, hack, open a PR. The codebase is small (~500 lines per file, clearly separated modules). Issues and feature requests are equally welcome.
