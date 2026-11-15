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
cargo test --locked --manifest-path src-tauri/Cargo.toml
git diff --exit-code -- src/bindings
```

### Windows desktop smoke test

Install [tauri-driver 2.1](https://v2.tauri.app/develop/tests/webdriver/) and
Microsoft Edge WebDriver for the installed WebView2 version. Put both drivers on
`PATH`, or provide their absolute paths in `OMNIAGENT_TAURI_DRIVER` and
`OMNIAGENT_EDGE_DRIVER`.

```powershell
pnpm tauri build --debug --no-bundle --config tests/desktop/tauri.conf.json
pnpm test:desktop
```

The test build has a separate application identifier and uses disposable Git
worktrees.
