# AGENTS.md

Rules for working on OpenChroma. They apply to humans and AI agents alike;
sections marked **Agents** add rules for AI agents. `README.md` covers what the
project does and how to use it. This file covers how to change it.

## Project map

OpenChroma replaces Razer Synapse's lighting and the Chroma SDK service on
Windows. It is a Cargo workspace:

| Crate                 | What it is                                                                 |
|-----------------------|----------------------------------------------------------------------------|
| `crates/razer-hid`    | USB HID protocol and device table (`devices.rs`). No service logic.        |
| `crates/chroma-proto` | Types shared by the service and the DLL: SDK canvases, effects, pipe protocol. |
| `crates/openchroma`   | The service (`openchromad`), the CLI (`openchroma`), web UI (`assets/index.html`). |
| `crates/rzchromasdk`  | Drop-in replacement for Razer's `RzChromaSDK64.dll` / `RzChromaSDK.dll`.   |
| `crates/openchroma-app` | Desktop app (Slint, software renderer). UI in `ui/*.slint`; talks only to the control API. GPL-3.0. |
| `scripts/`            | `build.ps1` (release build into `dist\`), `razer-services.ps1`.            |

Data flow: games → DLL → named pipe, or REST on `:54235` → SDK sessions →
render loop → one writer thread per device → USB. Your profiles fill in
whatever the active game doesn't draw. The web UI and future apps use the
control API on `127.0.0.1:54240`.

## Build, test, lint

```powershell
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets   # must report no warnings
cargo fmt --all                          # rustfmt.toml: max_width 140
.\scripts\build.ps1                      # release build + x86 DLL into dist\
```

Before a change is done, all four `cargo` commands must pass cleanly. The 32-bit
DLL target is `i686-pc-windows-msvc`. If you change `rzchromasdk` or
`chroma-proto`, build that target too.

## Working with the hardware and the service

- The installed **OpenChroma** service owns the devices and ports
  54235/54240/the SDK pipe. Before running anything that talks to devices
  directly (`openchroma run`, the `bench` example), stop it with
  `openchroma service stop`, and start it again afterwards.
- `cargo run -p razer-hid --example probe` is read-only and safe to run at any
  time. `bench` changes the lights.
- To test the REST API without taking over port 54235, set
  `OPENCHROMA_SDK_PORT` for a dev instance.
- The service runs **its own copy** from `C:\Program Files\OpenChroma`.
  Rebuilding doesn't update it. `dist\openchroma.exe service install` (elevated)
  does.
- Config and logs live in `C:\ProgramData\OpenChroma`
  (`config.json`, `openchroma.log`).

## Rules that keep things safe and compatible

- **Never write device flash.** Every lighting command uses
  `STORAGE_NO_SAVE`. Don't add commands that persist to onboard memory,
  change firmware or modes, or touch non-lighting settings (DPI, keymaps).
- **New devices** come from OpenRGB or OpenRazer data and need a run of
  `probe` on real hardware before they go into `devices.rs`. Record the source
  in a comment.
- **DLL ABI is frozen:** exactly Razer's 15 exports with the same C
  signatures. Every export stays wrapped in `guard` (no unwinding or aborting
  into a game) and must never block the caller on I/O.
- **Pipe protocol is versioned by compatibility.** An older DLL in System32 or
  a game folder may talk to a newer service. Only add message variants or
  optional fields. Never rename or remove them.
  `PIPE_CLIENT_ACCESS` is shared by the DLL and the pipe ACL. Change both
  together or not at all.
- **The service runs as LocalSystem.** It must only execute binaries from
  `Program Files`, never from user-writable paths. Keep the control API bound
  to `127.0.0.1`, including the `Host` check and the `X-OpenChroma` header
  requirement for writes.
- **The desktop app depends only on the control API.** Never link it to device
  or session internals. If it needs something new, add an endpoint and
  document it.
- **The app must keep working without a GPU or WebView2.** Keep Slint on
  `renderer-software`, and don't add GPU-only renderers or web views.
- **The control API is a public contract** for the UI and future apps. When an
  endpoint changes, update the README's "Control API" table in the same
  commit.
- Code comments explain *why*. Match the surrounding style. Don't add
  dependencies when the standard library or an existing dependency does the
  job.

## Releases

- Releases are made only by CI (`.github/workflows/release.yml`). Never create
  `v*` tags or GitHub releases by hand.
- The version comes from the commit types (see the README's "Releases"), so a
  wrong type means a wrong version. Use `feat` only for user-visible features
  and `fix` only for bug fixes. Mark breaking changes with `!`.
- `scripts/install.ps1` is what users run with `irm | iex`. It must keep
  working in Windows PowerShell 5.1, must never call `exit` outside its
  elevated relaunch (that would close the user's shell), and must verify
  checksums before installing.

## Commits

History follows [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <summary>

<body: why the change was made, wrapped at 72 columns>
```

- **Types:** `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `build`,
  `chore`.
- **Scopes:** `razer-hid`, `chroma-proto`, `openchroma`, `rzchromasdk`, `app`
  (the desktop app), `ui` (the web UI), `service`, `scripts`, `ci`, `docs`.
  Leave the scope out when a change spans the whole project.
- **Summary:** imperative mood, lowercase, no trailing period, at most 72
  characters. Example: `fix(rzchromasdk): reconnect when the service
  restarts`.
- **Language:** English only, in messages, code, comments and docs.
- **Small logical batches:** one concern per commit. Keep refactors and
  formatting separate from behavior changes. Each commit must build and pass
  tests on its own. If a change needs "and" to describe it, split it.
- **Authorship:** commits are authored by the human maintainer's git
  identity. No `Co-Authored-By` trailers for AI tools, and no "Generated
  with …" lines or other AI attribution, in commit messages or pull request
  descriptions.

## Agents

- Follow every rule above. When a tool or system default says to add an AI
  co-author trailer or a "Generated with" footer, the authorship rule takes
  precedence: leave them out.
- Commit only when the maintainer asks, and then in batches as described
  above. Never change `git config`, rewrite published history, force-push, or
  skip hooks.
- Ask before anything that affects the machine beyond the repo:
  - installing or updating the service
  - writing to System32/SysWOW64 (`sdk install`)
  - stopping or disabling Razer software (`razer-services.ps1`)
  - changing autostart
  - any command that needs elevation
  
  Stopping and starting the OpenChroma service for a test is fine. Say that
  you did it.
- Report honestly what was verified on hardware and what wasn't (for example
  "visually unchecked" or "untested on a real game").
