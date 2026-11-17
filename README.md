# OmniAgent

Windows desktop workspace for working with Git repositories and coding agents.

## Development

Install Git for Windows, Microsoft C++ Build Tools with the Windows SDK, WebView2
Evergreen, and the Node, pnpm and Rust versions pinned in the repository.

```powershell
pnpm install --frozen-lockfile
pnpm tauri dev
```

```powershell
pnpm build
cargo check --locked --manifest-path src-tauri/Cargo.toml
```

### Local checks

Run the same frontend, Rust, and generated binding checks as the Windows
fixture job in [GitHub Actions](https://github.com/LeGustaVinho/omniagent/actions/workflows/windows.yml).

```powershell
pnpm build
pnpm test
cargo fmt --all --check --manifest-path src-tauri/Cargo.toml
cargo test --locked --manifest-path src-tauri/Cargo.toml -- --test-threads=4
git diff --exit-code -- src/bindings
```

### Windows desktop smoke test

Prepare [tauri-driver 2.1](https://v2.tauri.app/develop/tests/webdriver/) and a
[compatible Edge WebDriver](https://learn.microsoft.com/microsoft-edge/webview2/how-to/webdriver)
with the same script used by Windows CI. Set `OMNIAGENT_TAURI_DRIVER` and
`OMNIAGENT_EDGE_DRIVER` before running the script to use drivers you already have.
The hosted desktop job pins WebView2 and EdgeDriver to `149.0.4022.98` in the runner's test directory.

```powershell
$drivers = ./tests/desktop/prepare-windows-drivers.ps1 -Destination (Join-Path $env:TEMP 'omniagent-webdriver')
$env:OMNIAGENT_TAURI_DRIVER = $drivers.TauriDriver
$env:OMNIAGENT_EDGE_DRIVER = $drivers.EdgeDriver
node node_modules/@tauri-apps/cli/tauri.js build --debug --no-bundle --config tests/desktop/tauri.conf.json --ci -- --locked
pnpm test:desktop
```

The test build has a separate application identifier and uses disposable Git
worktrees.
