# Contributing to AppNest

Thanks for your interest in contributing! Here's how to get set up.

## Development Setup

### 1. Install prerequisites

- **Rust** (stable toolchain)
  ```
  winget install Rustlang.Rustup
  rustup default stable-x86_64-pc-windows-gnu
  ```

- **MSYS2 + MinGW** (provides the GNU linker)
  ```
  winget install MSYS2.MSYS2
  ```
  Open MSYS2 terminal and run:
  ```
  pacman -S mingw-w64-x86_64-gcc
  ```

- **Git**
  ```
  winget install Git.Git
  ```

### 2. Clone and build

```powershell
git clone https://github.com/BipulRaman/AppNest.git
cd AppNest/app

# Ensure MinGW is in PATH
$env:PATH = "C:\msys64\mingw64\bin;$env:USERPROFILE\.cargo\bin;$env:PATH"

# Build
cargo build --release
```

### 3. Run

```powershell
.\target\release\appnest.exe
```

Dashboard opens at `http://localhost:1234`. The app runs in the system tray.

### 4. Develop

Edit files in `app/src/` (Rust backend) or `app/public/` (frontend). Then rebuild:

```powershell
cargo build --release
```

> **Note:** The frontend (HTML/CSS/JS) is embedded at compile time. You must rebuild after frontend changes.

## Project Layout

```
app/src/main.rs      → Entry point, system tray, event loop
app/src/manager.rs   → App lifecycle, process spawning, logging, log broadcast
app/src/server.rs    → Axum HTTP routes, SSE log streaming, embedded assets, native dialogs
app/public/          → Frontend (compiled into the binary)
.github/workflows/   → Release pipeline (stamps Cargo.toml version from tag, publishes exe)
```

## Guidelines

- Keep it simple. This is a lightweight tool, not a framework.
- No external runtime dependencies — the exe must be self-contained.
- Test your changes with at least one .NET and one Node.js project.
- Keep the frontend vanilla (no frameworks, no build step for the UI itself).

## Submitting Changes

1. Fork the repo and create a branch from `main`.
2. Make your changes.
3. Test locally.
4. Open a pull request with a clear description of what you changed and why.

## Reporting Issues

Open a GitHub issue with:
- What you expected to happen
- What actually happened
- Your OS version and any relevant logs (export from the Logs panel)
