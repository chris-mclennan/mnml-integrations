# mnml-virt-docker

A terminal browser for [Docker](https://www.docker.com/) — list containers, images, volumes, networks, and per-project compose services without leaving the keyboard. **First sibling in the `mnml-virt-*` family** (planned siblings: `mnml-virt-k8s`, `mnml-virt-podman`, `mnml-virt-colima`).

Shells out to the `docker` CLI for everything (same pattern the AWS family uses with `aws`). No SDK dep, no API tokens — Docker's socket IS the auth boundary.

Runs **standalone in any terminal** today. v0.2 will add mnml-hosted pane mode via the [blit-host protocol](https://mnml.sh/manual/integrations/building/).

```
┌─ docker ──────────────────────────────────────────────────────────────┐
│ ▸1.containers (8)  2.images (47)  3.volumes (12)  4.networks (5)      │
└───────────────────────────────────────────────────────────────────────┘
┌─ containers (8) ──────────────┐ ┌─ inspect ───────────────────────────┐
│ ▸ ● redis              redis:7│ │ Name           redis                │
│   ● postgres-pg        pg:16  │ │ ID             abc1234def56         │
│   ○ example-api         acme…│ │ Image          redis:7              │
│   ↺ web-1              myorg/…│ │ State          running              │
│   · created-only       redis:…│ │ Status         Up 2 hours           │
│                               │ │ Ports          6379/tcp             │
│                               │ │                                     │
│                               │ │  inspect                            │
│                               │ │  [ { "Id": "abc1234...",            │
│                               │ │      "Created": "2026-06-07T…",     │
│                               │ │      "State": { "Running": true …   │
└───────────────────────────────┘ └─────────────────────────────────────┘
  1-9 tab · ↑↓/jk move · o desktop · y ID · l logs · e exec · s/S stop/start · R rm · L ECR · r refresh · q quit
```

## Install

```sh
cargo install --git https://github.com/chris-mclennan/mnml-virt-docker mnml-virt-docker
mnml-virt-docker --install
```

## Pre-requisite

You need **Docker installed and running**. Either [Docker Desktop](https://www.docker.com/get-started/) (macOS / Windows / Linux) or any other build of the Docker Engine.

`mnml-virt-docker` shells out to `docker` and never opens the daemon socket directly — whichever context the `docker` CLI is configured for is the one this binary sees. v0.1 supports the default context only; multi-context switching is queued for v0.2.

If the daemon isn't running on launch, the body switches to a "Docker daemon not running" notice. Start Docker Desktop, press `r`, and the connection re-probes.

## Setup

1. **Verify the docker CLI works.** `docker version` must print server info.
2. **Run once** to scaffold the config: `mnml-virt-docker`.
3. **Edit `~/.config/mnml-virt-docker/config.toml`** if you want a compose tab.
4. **Re-run**.

## Config

```toml
refresh_interval_secs = 60

[[tabs]]
name = "containers"
kind = "containers"

[[tabs]]
name = "images"
kind = "images"

[[tabs]]
name = "volumes"
kind = "volumes"

[[tabs]]
name = "networks"
kind = "networks"

# Add per-project compose tabs by pointing at the project directory:
# [[tabs]]
# name = "myapp"
# kind = "compose"
# project_path = "/Users/me/Projects/myapp"
```

### Tab kinds

| `kind` | What it shows | Required fields |
|---|---|---|
| `containers` (default) | Every container in the local engine | none |
| `images` | Every image | none |
| `volumes` | Every volume | none |
| `networks` | Every network | none |
| `compose` | Services in one compose project (uses `compose.yaml` / `compose.yml` / `docker-compose.yml` in this order) | `project_path` |

Compose tabs are opt-in — there's no useful default since the project directory is per-user.

## Layout

- **Tab strip:** one tab per `[[tabs]]` entry with a per-tab count badge.
- **Items list (left, 45%):** focused row gets a cyan highlight. The leading glyph encodes container state:
  - `●` green — running
  - `○` gray — exited · red — dead
  - `↺` yellow — restarting
  - `‖` yellow — paused
  - `·` gray — created
- **Detail panel (right, 55%):** focused-item header (Name / ID / Image / State / …) plus the full pretty-printed `docker inspect` output below. Inspect runs lazily — only the focused item pays the cost.

## Keys

| Chord | Action |
|---|---|
| `1`–`9` | Switch to that tab |
| `Tab` / `BackTab` | Cycle tabs |
| `↑` / `k`, `↓` / `j` | Move selection (triggers inspect for the new focus) |
| `PgUp` / `PgDn` | Jump 10 rows |
| `g` / `G` | Top / bottom |
| `o` | Open Docker Desktop (macOS only — toast on Linux / Windows) |
| `y` | Yank focused item's full ID / name |
| `l` | Tail logs for the focused container (containers tab only) |
| `e` | Exec a shell into the focused running container (`/bin/bash` if available, else `/bin/sh`) |
| `s` | Stop the focused container (no confirm — reversible) |
| `S` | Start the focused container |
| `R` | Remove the focused item — **confirms first**. `y` confirms, `n` / `Esc` cancels |
| `L` | If the focused image's repo is an ECR URL (`<acct>.dkr.ecr.<region>.amazonaws.com/...`), spawn `mnml-aws-ecr --region <region>` |
| `r` | Refresh the active tab (and re-probe the daemon if it was offline) |
| `q` / `Esc` / `Ctrl+C` | Quit |

The `R` confirmation overlay is the only destructive flow — everything else (`s`, `S`, `e`, `l`) either is reversible or just spawns a read-only follower.

## Run modes

### Standalone

```sh
mnml-virt-docker
```

### Blit-host (hosted by mnml) — *coming v0.2*

```vim
:host.launch mnml-virt-docker
```

The blit channel isn't wired in v0.1 yet — `l` / `e` eat the controlling terminal in standalone mode for now. v0.2 will add the blit-host path *and* route the pty actions through mnml's pty pane plumbing so they live inside the cell grid.

## Wire it into mnml's left rail

Once published, this sibling will register as a default chip in mnml's rail under **INTEGRATIONS**. The whichkey chord will be `<leader>iv` (mnemonic: "virt") and the palette command `forge.open_virt_docker`.

## Not yet supported (v0.1)

- **Pull from a registry** — use `docker pull` in your terminal
- **Build images** — use `docker build` in your terminal (running a build inside a TUI tab is a no-fit)
- **Kubernetes** — that's the next `mnml-virt-*` sibling (`mnml-virt-k8s`)
- **buildx** — separate surface from `docker images`; held back
- **Swarm services / stacks** — overlaps with kubernetes; deferred
- **Multi-host docker contexts** — v0.1 watches the default context only

## Security note on `R` (rm)

The rm action **always** prompts before destroying. Once confirmed it is irreversible:

- `docker rm -f <id>` for containers (force — stops a running container first)
- `docker rmi <id>` for images
- `docker volume rm <name>` for volumes
- `docker network rm <name>` for networks

Removing a container does **not** remove its named volumes — those persist independently and have to be removed from the volumes tab. If you `rm` a volume by accident, the data is gone; there's no undo.

## Status

**v0.1** — five tab kinds (containers / images / volumes / networks / compose), per-tab inspect detail panel, ECR cross-sibling jump, stop / start / rm container, logs follow, exec shell, daemon-down recovery via `r`. Standalone only; blit-host mode is queued for v0.2.

## Source

[github.com/chris-mclennan/mnml-virt-docker](https://github.com/chris-mclennan/mnml-virt-docker). MIT.
