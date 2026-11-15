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
