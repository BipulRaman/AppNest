# AppNest

A developer productivity tool for managing and hosting local web applications. Build, run, and monitor all your projects — .NET, Node.js, React, Angular, Vue, and more — from a single lightweight dashboard.

Stop juggling terminals. One app to build, host, and watch them all.

**Single ~2 MB executable. No runtime dependencies. System tray integration. Built with Rust.**

---

## Screenshot

![AppNest Dashboard](Dashboard.png)

## Features

- **One-click build & run** — Add your project, pick a type, and hit Start. Build steps and run command execute automatically.
- **Local hosting manager** — Host multiple apps on different ports simultaneously from one place.
- **Multi-framework presets** — .NET, Node.js, React, Next.js, Angular, Vue, Express with smart defaults (customizable via `presets.json`).
- **Static file serving** — Serve React/Angular/Vue build output directly without extra tools.
- **Live log streaming** — Runtime and build output streamed via Server-Sent Events with ANSI color preservation, clickable URLs, timestamped lines, and error/warning highlighting.
- **In-log search & follow mode** — Filter log lines on the fly, highlight matches, and pause/resume auto-scroll with a single click.
- **Inline log preview** — Quick tail of the latest output right inside each row — no need to open the full modal.
- **Command palette** — `Ctrl/Cmd+K` opens a fast fuzzy launcher for every app and action.
- **Dark & light themes** — Indigo accent with a proper dark mode; the theme toggle persists across launches.
- **Uptime, PID, port chip** — Live uptime next to each running app; port chip opens the URL in the browser (left-click) or copies it to clipboard (right-click).
- **Pending-state buttons** — Start/Restart buttons visually lock while a transition is in flight to prevent double-fires.
- **System tray** — Runs in background. Start All, Stop All, or Quit from the tray menu.
- **Auto-start** — Flag apps to start automatically when AppNest launches.
- **Persistent logs** — Export or copy logs. File-based logs stored per app with timestamps.
- **Open folder / terminal** — From the logs modal, jump straight into the project folder in Explorer or a native terminal (Windows Terminal, PowerShell, macOS Terminal, or the user's preferred Linux emulator).
- **Drag-to-reorder** — Arrange apps in the order you want them listed.
- **Self-update notifier** — Optional check against GitHub releases, with an unobtrusive banner that links to the release page.
- **Native file dialogs** — OS-native folder/file pickers for selecting project directories and scripts.
- **Zero config** — No YAML, no Docker, no config files. Everything is configured through the UI.
- **Portable** — Single `.exe` with all HTML/CSS/JS embedded. App data stored in `%APPDATA%\AppNest\`.

## Quick Start

1. Download `AppNest.exe` from [Releases](../../releases).
2. Run it. The dashboard opens at `http://localhost:1234`.
3. Click **New Application**, select your project type, browse to your project folder.
4. Hit **Save**, then **Start**.

That's it. Your app is built and running.

## How It Works

Each application you add has:

| Field | Description |
|-------|-------------|
| **Name** | Display name for the dashboard |
| **Project Directory** | Folder where all commands run (Browse to select) |
| **Type** | .NET, Node.js, React, Next.js, Angular, Vue, Express (driven by `presets.json`) |
| **Serve Mode** | `Command` (run a process), `Static Folder` (serve files), or `Script File` |
| **Port** | Port number — auto-injected as `PORT` env var (or `ASPNETCORE_URLS` for .NET) |
| **Build & Run Command** | Commands that run in order — last line is the run command (for Command mode) |
| **Environment Variables** | KEY=VALUE pairs passed to the process |
| **Auto-start** | Start this app automatically when AppNest launches |

When you select a project type, all fields are pre-filled with recommended defaults.

### Static Serving

For React, Angular, and Vue apps, select **Static Folder** as serve mode and point it to your build output (`./build`, `./dist`, etc.). AppNest serves the files directly with SPA fallback — no need for `npx serve` or `http-server`.

## Prerequisites

### To run (end users)
- Windows 10/11 (64-bit)
- No other dependencies — it's a self-contained executable

### To build from source
- **Rust** (stable) — [Install via rustup](https://rustup.rs)
- **MSYS2 + MinGW** — for the GNU toolchain on Windows
  ```
  winget install MSYS2.MSYS2
  ```
  Then in MSYS2 terminal:
  ```
  pacman -S mingw-w64-x86_64-gcc
  ```
- **Rust GNU target** (if not default):
  ```
  rustup default stable-x86_64-pc-windows-gnu
  ```

## Building

```powershell
cd app
$env:PATH = "C:\msys64\mingw64\bin;$env:USERPROFILE\.cargo\bin;$env:PATH"
cargo build --release
```

### Build Output

The only file you need from the build:

```
app\target\release\AppNest.exe    (≈2 MB)
```

That's it — **single file, no DLLs, no config files, no supporting files**. The HTML, CSS, and JS are compiled into the binary. Copy the exe anywhere and run it. App data is created automatically at `%APPDATA%\AppNest\` on first launch.

## Project Structure

```
AppNest/
├── README.md
├── LICENSE
├── CONTRIBUTING.md
├── Dashboard.png
├── .github/workflows/build.yml
└── app/
    ├── Cargo.toml        # Rust dependencies and config
    ├── src/
    │   ├── main.rs       # Entry point, system tray, event loop
    │   ├── manager.rs    # App lifecycle, process management, logging
    │   └── server.rs     # HTTP API (axum), embedded frontend, file dialogs
    └── public/
        ├── index.html    # Dashboard UI
        ├── style.css     # Styles
        ├── app.js        # Frontend logic
        └── presets.json  # App type presets (customizable)
```

**At build time**, the `public/` folder is compiled into the binary via `rust-embed`. The final exe has no external file dependencies.

**At runtime**, app data is stored in `%APPDATA%\AppNest\`:
- `apps.json` — saved application configurations
- `logs/` — persistent log files per app + server log

## Tech Stack

| Component | Technology |
|-----------|-----------|
| Backend | Rust, [Axum](https://github.com/tokio-rs/axum), [Tokio](https://tokio.rs) |
| Frontend | Vanilla HTML/CSS/JS (embedded via [rust-embed](https://github.com/pyrossh/rust-embed)) |
| Live log streaming | Server-Sent Events via `axum::response::sse`, `async-stream`, `futures-util` |
| System tray | [tray-icon](https://github.com/nicholasneo78/tray-icon) |
| File dialogs | [rfd](https://github.com/PolyMeilex/rfd) |
| Static serving | [tower-http](https://github.com/tower-rs/tower-http) ServeDir |
| Update check | [ureq](https://github.com/algesten/ureq) against the GitHub Releases API |

## FAQ

### General — Why not Docker?

<details>
<summary><strong>How is this different from Docker?</strong></summary>

Docker is a container runtime that virtualizes an entire OS layer, pulls images, manages volumes, networks, and orchestration. AppNest is a **lightweight process manager** — it runs your apps as native processes on your machine with zero abstraction overhead. There's no image to build, no Dockerfile to write, no daemon to keep running. You double-click an exe and your apps are live.
</details>

<details>
<summary><strong>Docker already works. Why would I switch?</strong></summary>

You don't have to switch. Docker is the right tool for production deployments, CI pipelines, and reproducible environments. AppNest targets a different problem: **day-to-day local development** where you're running 3–8 projects simultaneously and don't want to manage `docker-compose.yml`, rebuild images after every dependency change, or debug volume mount issues. If you're already happy with Docker for local dev, keep using it.
</details>

<details>
<summary><strong>Docker gives me reproducible environments. Doesn't this just "run stuff"?</strong></summary>

Yes — and that's the point. During active development, you already have your SDKs installed locally (.NET, Node.js, etc.). Wrapping every `npm start` in a container adds build time, RAM overhead, and debugging friction. AppNest leverages the tools you already have and gives you a dashboard to manage them. For reproducibility in CI/staging, keep using Docker.
</details>

<details>
<summary><strong>Docker Compose lets me start everything with one command too.</strong></summary>

True, but every dependency change means rebuilding the image. With AppNest, you change a file, hit Start, and your app picks up the change — same speed as running it from a terminal. No layer caching surprises, no stale `node_modules` in a volume, no waiting for `docker build`.
</details>

<details>
<summary><strong>Docker provides network isolation. How is running everything on localhost safe?</strong></summary>

For local development, network isolation is rarely a requirement — you're the only user. AppNest binds apps to `localhost` on ports you choose. If you need network isolation, firewall rules, or multi-machine topologies, Docker or VMs are the right tool. AppNest doesn't try to replace infrastructure tooling.
</details>

<details>
<summary><strong>My team standardizes on Docker so everyone has the same environment.</strong></summary>

That's a valid workflow for teams. AppNest is not anti-Docker — it's for the individual developer who has already set up their local toolchain and wants a faster inner-loop. You can use Docker for team consistency and AppNest for your personal dev workflow simultaneously.
</details>

### Resource & Performance

<details>
<summary><strong>How much RAM does Docker use vs AppNest?</strong></summary>

Docker Desktop on Windows typically consumes 1–4 GB of RAM for the WSL2 VM before any containers start. Each container adds its own memory footprint. AppNest itself uses ~10–15 MB of RAM — it's a native Rust binary with an embedded web UI. Your apps run as bare processes with no virtualization overhead.
</details>

<details>
<summary><strong>My machine has 8 GB RAM and Docker makes it crawl. Will this help?</strong></summary>

Significantly. With Docker Desktop eliminated, you reclaim 1–4 GB immediately. Your apps run as native processes with direct hardware access — no WSL2 VM, no container runtime, no overlay filesystem. On constrained machines, the difference is dramatic.
</details>

<details>
<summary><strong>Does this support hot reload / file watching?</strong></summary>

AppNest runs your apps natively, so whatever hot-reload your framework provides (Vite HMR, `dotnet watch`, `nodemon`) works out of the box. Docker often requires volume mounts and filesystem event forwarding (which is notoriously slow on Windows/macOS with Docker bind mounts). With AppNest, file watching is instant because there's no virtualization layer.
</details>

<details>
<summary><strong>Startup time comparison?</strong></summary>

Docker: pull image (first time) → build layers → start container → app boot. AppNest: run build commands → app is live. There's no daemon startup, no image layer resolution, no container creation. For a typical Node.js app, you save 5–30 seconds per restart.
</details>

### Private Feeds & Authentication

<details>
<summary><strong>I use private NuGet feeds / npm registries. Does this work?</strong></summary>

Yes, seamlessly. Because AppNest runs your build commands as native processes, they inherit your existing authentication setup — `.npmrc`, `nuget.config`, credential providers, environment variables — exactly the same as running from a terminal. There's nothing to configure inside AppNest.
</details>

<details>
<summary><strong>Docker requires special setup for private feeds (tokens, secrets, BuildKit). What about here?</strong></summary>

This is one of the biggest pain points AppNest eliminates. With Docker, accessing private npm registries or NuGet feeds during `docker build` requires multi-stage builds, `--mount=type=secret`, or baking tokens into images (a security risk). With AppNest, `npm install` or `dotnet restore` just works because it's running in your authenticated user session with all your credentials already available.
</details>

<details>
<summary><strong>We use Azure Artifacts / GitHub Packages / JFrog Artifactory. Any special config?</strong></summary>

None. If `npm install` or `dotnet restore` works in your terminal, it works in AppNest. Your credential helpers, PATs in `.npmrc`, Azure Artifacts Credential Provider, and `nuget.config` sources are all picked up automatically. No need to pass secrets into a container build context.
</details>

<details>
<summary><strong>Our private registry requires VPN access. Does this work?</strong></summary>

Yes. Your processes run under your OS network stack directly. If you're on the VPN, your apps can reach the private registry — no Docker DNS issues, no network mode hacks, no `host.docker.internal` workarounds.
</details>

<details>
<summary><strong>I use a corporate proxy for package downloads. Docker is a nightmare for this.</strong></summary>

With AppNest, your apps inherit the system proxy settings automatically — same as any native process. Docker's proxy configuration (daemon-level `HTTP_PROXY`, build-arg injection, WSL2 proxy forwarding) is a well-known pain point that simply doesn't exist here.
</details>

### Scrutiny & Hard Questions

<details>
<summary><strong>This is just a process manager. I can do the same with a bash script.</strong></summary>

You can. But will your bash script give you a web dashboard, live color-coded logs, one-click start/stop, system tray integration, auto-start on launch, persistent log files, and static file serving with SPA fallback? AppNest packages all of this into a single 2 MB executable with zero setup.
</details>

<details>
<summary><strong>PM2 already does process management for Node.js. Why this?</strong></summary>

PM2 is Node.js-specific and requires Node.js to be installed. AppNest manages any framework (.NET, Node.js, React, Angular, Vue, Next.js, Express) with a unified UI. It's also a single binary with no runtime dependency — you don't need Node.js installed just to manage your processes.
</details>

<details>
<summary><strong>What about Linux and macOS support?</strong></summary>

Currently Windows-only. The system tray integration and file dialogs use Windows APIs. Cross-platform support is on the roadmap but not available yet. If you develop on Linux/macOS, this tool isn't for you right now.
</details>

<details>
<summary><strong>What happens if an app crashes?</strong></summary>

AppNest detects when a process exits and updates the dashboard status. You can see the exit code and the full log output to diagnose the issue. There's no automatic restart yet — you click Start again. Auto-restart on crash is a potential future feature.
</details>

<details>
<summary><strong>Can this run databases (PostgreSQL, Redis, MongoDB)?</strong></summary>

Technically yes — if the database is installed natively on your machine, you can manage it through AppNest. But this is where Docker genuinely shines: spinning up disposable, pre-configured database instances. Most developers will use AppNest for their application code and Docker (or native installs) for databases.
</details>

<details>
<summary><strong>Can it run multiple instances of the same app on different ports?</strong></summary>

Yes. Add the same project directory multiple times with different names and ports. Each entry runs independently with its own process, logs, and lifecycle.
</details>

<details>
<summary><strong>What about microservices with 15+ services? Docker Compose handles that.</strong></summary>

AppNest can manage 15+ apps, but if your services have complex inter-dependencies, health checks, and startup ordering, Docker Compose's dependency graph (`depends_on`, health checks) is more appropriate. AppNest starts apps independently — it doesn't model dependencies between services.
</details>

<details>
<summary><strong>Docker has health checks. Does AppNest monitor app health?</strong></summary>

AppNest shows running/stopped status and live log output, but it doesn't perform HTTP health checks or liveness probes. You can see if a process is alive and inspect its logs, but there's no automated "restart if unhealthy" logic. For local dev, checking the dashboard is usually sufficient.
</details>

<details>
<summary><strong>What if two apps try to use the same port?</strong></summary>

The second app will fail to bind and you'll see the error in the logs. AppNest doesn't enforce port uniqueness at config time (you might intentionally stop one before starting another), but the error is immediately visible in the dashboard.
</details>

<details>
<summary><strong>Does it support HTTPS locally?</strong></summary>

AppNest' built-in static server uses HTTP. However, if your app itself runs HTTPS (e.g., `dotnet` with dev certs, or Node.js with a self-signed cert), that works fine since AppNest just runs your command. For the dashboard itself, it's `http://localhost:1234`.
</details>

<details>
<summary><strong>How is this different from Visual Studio's multi-project launch?</strong></summary>

VS multi-startup is IDE-specific and limited to projects within a single solution. AppNest is IDE-agnostic — it manages any project from any framework, and it runs in the background via the system tray even when no IDE is open. It's also useful for running frontend and backend projects that live in separate repos.
</details>

<details>
<summary><strong>How is this different from .NET Aspire?</strong></summary>

They solve overlapping problems from opposite ends:

| | **.NET Aspire** | **AppNest** |
|---|---|---|
| **Primary audience** | .NET developers building distributed cloud apps | Any developer running a mix of local projects |
| **Scope** | Orchestration framework + code model (`AppHost` project, C# DSL) | Standalone process manager with a web dashboard |
| **Setup** | Add Aspire workload, create an `AppHost` project, wire services in C# | Download a 2 MB exe, point it at project folders |
| **Language coupling** | .NET-centric; non-.NET apps integrated as "executables" or containers | Framework-agnostic by design (.NET, Node, React, Angular, Vue, Python, Go, …) |
| **Runtime dependencies** | .NET SDK, Aspire workload, usually Docker Desktop for containers/dashboards | None — single native binary |
| **Service discovery / config injection** | Built-in (connection strings, endpoints auto-wired between resources) | Not provided — apps use their own config, AppNest just starts processes |
| **Telemetry dashboard** | Rich OpenTelemetry dashboard (traces, metrics, structured logs) | Live stdout/stderr log streaming with search and follow mode |
| **Deployment story** | Can generate manifests for Azure Container Apps, Kubernetes, etc. | None — purely a local dev tool |
| **Footprint** | Heavy: SDK + workload + Docker + dashboard container | ~2 MB exe, ~15 MB RAM |

Use **Aspire** when you're building a .NET-first distributed system and want service discovery, OpenTelemetry, and a path to cloud deployment baked in.

Use **AppNest** when you just want to start/stop/watch a handful of heterogeneous local projects from one place, without adopting a framework, installing Docker, or writing an orchestration project in C#.

They can also coexist: run your Aspire `AppHost` as one entry in AppNest alongside unrelated Node/React/Python projects. Just add a new application in AppNest, point it at your `AppHost` project folder, pick the .NET preset (or set the run command to `dotnet run --project AppHost.csproj`), and flag it as **Auto-start** if you want Aspire to come up automatically with the rest of your stack.
</details>

### Security

<details>
<summary><strong>Is the dashboard exposed to the network?</strong></summary>

No. AppNest binds exclusively to `127.0.0.1` (localhost). The OS network stack rejects all connections from external machines — other computers on your LAN, Wi-Fi, or the internet cannot reach it. Traffic to `127.0.0.1` never leaves your network interface.
</details>

<details>
<summary><strong>Can another app on my machine access the API?</strong></summary>

Yes — any process running on the same machine can send HTTP requests to `http://127.0.0.1:1234`. However, this is not a meaningful security concern: any local process already has the same (or greater) privileges as AppNest. It can directly kill processes, read/write files, and modify your system without needing the AppNest API.
</details>

<details>
<summary><strong>Is there authentication on the API?</strong></summary>

No. Since the server only listens on localhost, authentication isn't necessary. The OS-level network boundary (loopback interface) is the security layer. Adding token auth would protect against local cross-origin attacks but is not required for the intended use case — a single-user dev tool on your own machine.
</details>

<details>
<summary><strong>What if I change the bind address to 0.0.0.0?</strong></summary>

**Don't** — unless you understand the risk. Binding to `0.0.0.0` exposes all API endpoints to your entire network with no authentication. Anyone on your LAN could start/stop apps, delete configurations, or execute commands via your projects' build steps. If you need remote access, put it behind a reverse proxy with authentication.
</details>

<details>
<summary><strong>Can a malicious website trigger requests to localhost:1234?</strong></summary>

Modern browsers enforce CORS — a webpage on a different origin cannot read responses from `localhost:1234`. However, simple POST requests (form submissions) can still be sent cross-origin. Since AppNest only modifies state via JSON `Content-Type` requests, browsers will send a CORS preflight that gets blocked. This provides reasonable protection against cross-site request forgery for typical browser-based attacks.
</details>

<details>
<summary><strong>Are environment variables and secrets stored securely?</strong></summary>

Environment variables are stored in plain text in `%APPDATA%\AppNest\apps.json`. Do not put production secrets, API keys, or passwords in the environment variables field. For sensitive values, use your OS credential store or `.env` files in your project directory (which your framework reads directly). AppNest is a local dev tool — treat it accordingly.
</details>

<details>
<summary><strong>Can I use this in CI/CD?</strong></summary>

No. AppNest is a desktop tool with a GUI, system tray, and file dialogs. It's designed for developer workstations, not headless CI environments. For CI/CD, use Docker, shell scripts, or your CI platform's native tooling.
</details>

<details>
<summary><strong>What about environment parity? Docker ensures dev matches production.</strong></summary>

Fair point. AppNest runs your code on your native OS with your installed SDK versions, which may differ from production. If strict environment parity matters (e.g., matching a specific Linux distro, library versions, or system dependencies), Docker is the right choice. AppNest optimizes for **developer speed**, not environment parity.
</details>

<details>
<summary><strong>Is my app data safe? Where is everything stored?</strong></summary>

App configurations are stored in `%APPDATA%\AppNest\apps.json` and logs in `%APPDATA%\AppNest\logs\`. Nothing is sent to the cloud. Everything is local and plaintext — you can back it up, version-control it, or delete it freely.
</details>

<details>
<summary><strong>Can I share my AppNest configuration with my team?</strong></summary>

You can copy `apps.json` and `presets.json`, but paths are absolute to your machine, so teammates would need to adjust project directories. This is simpler than a Dockerfile but less portable. A future enhancement could support relative paths or workspace-level config files.
</details>

<details>
<summary><strong>What if I need to run a Linux-only tool on Windows?</strong></summary>

You can't. AppNest runs native Windows processes. If your project requires Linux-specific tooling (e.g., certain native modules, shell scripts with bash-isms), you need WSL2 or Docker. AppNest doesn't virtualize anything.
</details>

<details>
<summary><strong>Does it support custom presets beyond the built-in ones?</strong></summary>

Yes. Edit `presets.json` to add any project type with custom build commands, serve modes, ports, and environment variables. You can add presets for Python/Flask, Go, Java/Spring, or anything else.
</details>

<details>
<summary><strong>This is Windows-only and 2 MB. That feels too simple. What's the catch?</strong></summary>

No catch. Constraints are features: Windows-only means native APIs, no Electron overhead, and tight OS integration. 2 MB means no bundled runtime, no webview framework, no bloat. It uses your default browser for the dashboard and Rust for the backend. Simple is the point.
</details>

<details>
<summary><strong>Can I run Docker containers through AppNest?</strong></summary>

Yes, technically. Set the run command to `docker run ...` or `docker compose up`. AppNest will manage the process and capture the logs. But at that point, you're using Docker with AppNest as a log viewer — which is still useful if you want a unified dashboard for some native apps and some containerized services.
</details>

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup and guidelines.

## License

MIT License. See [LICENSE](LICENSE) for details.
