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
