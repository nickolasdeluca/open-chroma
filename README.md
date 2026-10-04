# OpenChroma

An open replacement for Razer Synapse's lighting and the Razer Chroma SDK
service on Windows. It talks to Razer devices directly over USB HID, runs your
own lighting profiles when you're not gaming, and lets Chroma-enabled games take
over the lights. ASUS Aura motherboard lighting is driven the same way, with no
Armoury Crate or OpenRGB needed. Games can connect through either the native SDK DLL or the
REST API.

```
 games (native)                    games / apps (REST)
 RzChromaSDK64.dll / RzChromaSDK.dll   http://localhost:54235/razer/chromasdk
        │  (OpenChroma's drop-in DLL)          │
        └──── named pipe ────┐   ┌─────────────┘
                             ▼   ▼
                     openchromad (service)
       profiles · SDK sessions · compositor · web UI (127.0.0.1:54240)
                             │
          one writer thread per device, USB HID feature reports
                             ▼
   keyboard · mouse · mousepad · case · ARGB controller · motherboard
```

## Install

From PowerShell:

```powershell
irm https://github.com/nickolasdeluca/open-chroma/releases/latest/download/install.ps1 | iex
```

The same command updates an existing install. The installer asks for
administrator rights, downloads the latest release and checks its SHA-256. If
Razer's Chroma SDK services are running, it offers to stop them. It then
installs the service, the desktop app and the SDK DLLs (see below).

For a specific version, or to uninstall:

```powershell
& ([scriptblock]::Create((irm https://github.com/nickolasdeluca/open-chroma/releases/latest/download/install.ps1))) -Version 0.2.0
& ([scriptblock]::Create((irm https://github.com/nickolasdeluca/open-chroma/releases/latest/download/install.ps1))) -Uninstall
```

Your profiles and settings in `%ProgramData%\OpenChroma` are kept on update and
uninstall.

## Supported devices

Verified on real hardware:

| PID    | Device                                  | LEDs                        |
|--------|-----------------------------------------|-----------------------------|
| 0x0221 | Razer BlackWidow Chroma V2              | 6 × 22 matrix               |
| 0x0099 | Razer Basilisk V3                       | logo, scroll wheel, 9 underglow |
| 0x0C02 | Razer Goliathus Chroma Extended         | 1 zone                      |
| 0x0F13 | Lian Li O11 Dynamic Razer Edition       | 4 strips × 16               |
| 0x0F1F | Razer Chroma Addressable RGB Controller | 6 channels × up to 80       |

ASUS motherboards with the Aura USB controller (vendor 0x0B05):

| PID    | Board                   | LEDs                                          |
|--------|-------------------------|-----------------------------------------------|
| 0x18F3 | ROG Crosshair VIII Hero | 8 onboard (incl. 2 × 12 V), 2 ARGB × up to 120 |

The board reports how many LEDs and headers it has, but not what is plugged
into the ARGB headers. Set each header's LED count in the web UI. Boards that
drive their LEDs over SMBus (most from before 2018) aren't supported. Armoury
Crate's lighting service can still take the board back; if the lights stop
following OpenChroma, stop it (OpenChroma never does that for you).

Device protocol details (matrix sizes, transaction ids, which HID interface takes
commands) come from [OpenRGB](https://gitlab.com/CalcProgrammer1/OpenRGB) and
[OpenRazer](https://github.com/openrazer/openrazer). To add a device, append an
entry to `crates/razer-hid/src/devices.rs` (or `crates/asus-aura/src/lib.rs`)
and a layout in `crates/openchroma/src/layout.rs`.

## Releases

`.github/workflows/release.yml` builds, lints and tests every push and pull
request. When a push to `main` contains releasable commits, it also tags
`vX.Y.Z` and publishes a GitHub release. The release contains:

- `OpenChroma-X.Y.Z-windows-x64.zip`: the binaries, `razer-services.ps1` and
  the license texts
- `install.ps1`
- `SHA256SUMS.txt`

The version comes from the Conventional Commits since the last tag
(`scripts/release-version.sh`):

| Commits since the last release | Next version |
|---|---|
| a breaking change (`feat!:` or a `BREAKING CHANGE:` footer) | major; minor while below 1.0 |
| at least one `feat` | minor |
| `fix` or `perf` | patch |
| only `docs`, `chore`, `ci`, `refactor`, `test`, `build` | no release |

The first release uses the version in `Cargo.toml`. To jump to a specific
version, raise it in `Cargo.toml`; a higher manual version always wins. CI
stamps the release version into the binaries, and `Cargo.toml` itself isn't
changed by releases.

## Building

You need Rust (MSVC toolchain). From PowerShell:

```powershell
.\scripts\build.ps1
```

This puts `openchroma.exe`, `openchromad.exe`, `openchroma-app.exe`,
`RzChromaSDK64.dll` and `RzChromaSDK.dll` in `dist\`. Keep them together; the
CLI looks for the other files next to itself.

## Switching from Synapse

Synapse and OpenChroma both write to the same devices, and Razer's SDK service
holds port 54235, so Razer's lighting stack needs to be stopped.

1. Check that OpenChroma sees everything. This is read-only:
   ```
   dist\openchroma devices
   ```
2. Stop Razer's lighting stack from an **elevated** PowerShell. `stop` lasts until
   reboot. `disable` keeps it off. `restore` undoes either one.
   ```powershell
   .\scripts\razer-services.ps1 stop
   ```
3. Install the OpenChroma service from an **elevated** terminal:
   ```
   dist\openchroma service install
   ```
4. Open **OpenChroma** from the Start menu.

The service is named **OpenChroma** ("OpenChroma Lighting Service" in
`services.msc`). The installer does the following:

- copies the binaries to `C:\Program Files\OpenChroma`
- registers the service to start at boot as LocalSystem, and to restart
  automatically if it fails
- lets signed-in users start and stop it without admin rights
  (`openchroma service start|stop`)
- puts OpenChroma's SDK DLLs in System32/SysWOW64, unless Razer's DLLs are
  still there
- adds an **OpenChroma** shortcut for the desktop app to the Start menu

Run `service install` again after rebuilding to update the service. It stops
the service, replaces the files and starts it again. `openchroma service
uninstall` removes the service and keeps your config.

On first start, OpenChroma imports the ARGB controller's channel layout (LED
counts, fan groups, names) from Synapse's logs, because the controller can't
report it.

You can also run OpenChroma without the service. `openchroma run` runs it in a
console, and `openchroma autostart on` starts it when you sign in.

## The desktop app

`openchroma-app.exe` is the main way to manage OpenChroma. It has four pages:

- **Lighting:** a live preview of every LED, plus your profiles, brightness, and
  whether games can take over.
- **Profile editor:** choose the effect, its colors, speed and direction, and
  give any device or ARGB channel its own effect.
- **ARGB channels:** set the LED count or the fan layout for each port, choose
  the Chroma Link LED it shows in games, and use Identify to flash a port white.
- **Games:** see the game in control, choose which game canvas each device shows,
  check the SDK DLL and REST API status, and allow or block each app that has
  used Chroma.

The app is a client of the service's control API, so the service keeps working
when the app is closed. If the service isn't running, the app offers to start
it. The UI is built with [Slint](https://slint.dev) and its software renderer,
so the app runs without WebView2 and without a GPU driver. That includes old
Windows builds, VMs and Remote Desktop.

The service also still serves a basic web UI at `http://127.0.0.1:54240/`
(`openchroma ui`).

## Letting games in

**REST API games** (including many Unity, web, and newer titles) work as soon as
OpenChroma owns port 54235. You don't need to install anything else.

**Native SDK games** load `RzChromaSDK64.dll` (64-bit) or `RzChromaSDK.dll`
(32-bit) by name. Install OpenChroma's DLLs in one of these places:

```
# system-wide (elevated; Razer's originals are backed up to %ProgramData%\OpenChroma\backup)
dist\openchroma sdk install
dist\openchroma sdk uninstall      # restores Razer's DLLs

# a single game (Windows loads DLLs from the game folder first)
dist\openchroma sdk install "D:\Games\SomeGame"
dist\openchroma sdk uninstall "D:\Games\SomeGame"

dist\openchroma sdk status
```

Razer installers and updates may put their DLL back, so check `sdk status` after
updating Synapse.

The DLL never blocks the game. Effects go to a background thread. If the
service restarts, the DLL reconnects within about 2 seconds and replays what
the game was showing. If a game crashes, its session ends right away.

### How game lighting maps to your devices

Chroma SDK apps draw on per-category canvases. OpenChroma maps them like this:

| Canvas                | Your hardware                                                   |
|-----------------------|-----------------------------------------------------------------|
| Keyboard 6×22         | BlackWidow Chroma V2, 1:1                                       |
| Mouse 9×7             | Basilisk V3: logo (7,3), wheel (2,3), underglow around the edge |
| Mousepad 20 (or 15)   | Goliathus: average of the pad's LEDs                            |
| Chroma Link 5         | O11 strips and ARGB channels, spread over Link LEDs 1-4         |
|                       | Motherboard: onboard LEDs on Link LED 0, headers over 1-4       |

Any category a game doesn't draw on keeps showing your profile. For example,
if a game only lights the keyboard, the fans keep their profile colors. On the
app's Games page you can choose a different canvas for any device, or
"Nothing" to keep that device on your profile during games. These choices are
stored as `game_mapping` in the config. You can also pin each ARGB channel or
motherboard header to a specific Chroma Link LED.

The newest SDK session from an allowed app controls the lights. When it ends,
the previous one takes over again, and when none are left, your profile comes
back. A blocked app keeps running normally, but OpenChroma ignores its
lighting. Apps are remembered in `%ProgramData%\OpenChroma\apps.json`.
`openchroma sdk off` (or the app's toggle) blocks all games.

## Profiles

Edit profiles in the UI or in `%ProgramData%\OpenChroma\config.json`. The available
effects are `off`, `static`, `breathing`, `spectrum`, `wave` (rainbow),
`gradient`, `color_wave`, and `starlight`. A profile can override the effect for
a device or zone:

```json
{
  "name": "Desk",
  "effect": { "type": "wave", "period": 4.0, "repeat": 1.0, "reverse": false },
  "overrides": {
    "keyboard": { "type": "static", "color": "#ffffff" },
    "argb:6":   { "type": "breathing", "colors": ["#ff2000"], "period": 5.0 }
  }
}
```

Override keys are `keyboard`, `mouse`, `mousepad`, `case`, `argb`, `motherboard`,
`case:1`-`case:4`, `argb:1`-`argb:6`, `motherboard:0` (onboard LEDs) and
`motherboard:1` onwards (ARGB headers). All effects run in software on every device, so the
devices stay in sync. Nothing is written to device flash, so unplugging a device
returns it to its own default.

CLI shortcuts while the service runs: `openchroma status`,
`openchroma profile "Ocean"`, and `openchroma brightness 40`.

## Reliability

These are the problems this project was built to fix:

- Every device has its own writer thread, so one stalled device doesn't freeze
  the others. Each device can take full frames at 55 fps or more.
- Devices are rescanned every 2 s. If a write fails, that device is dropped and
  reopened, which covers unplugging, USB resets, and sleep and resume.
- The full frame is re-sent every 5 s even if nothing changed. This restores
  lighting after a device resets itself.
- If another OpenChroma instance is already running, the new one refuses to
  start, so two instances never fight over the devices.
- Logs go to `%ProgramData%\OpenChroma\openchroma.log`.

## Control API

The desktop app and the web UI are both clients of a small local HTTP API. It listens on `http://127.0.0.1:54240` only and answers only
requests whose `Host` is `127.0.0.1` or `localhost`. Requests that change
anything must send an `X-OpenChroma` header with any value. A web page on
another site can't add that header without a CORS preflight, and the API never
approves one.

| Method & path        | Body / result                                                                 |
|----------------------|-------------------------------------------------------------------------------|
| `GET /api/status`    | devices (connected, firmware, live LED colors), SDK sessions, active profile, brightness |
| `GET /api/config`    | full config (profiles, ARGB channels, motherboard headers, ...)               |
| `PUT /api/config`    | replace the full config; it is validated, saved, and applied immediately     |
| `POST /api/settings` | any of `{"active_profile": "...", "brightness": 0-100, "sdk_enabled": bool}` |
| `POST /api/identify` | `{"target": "argb:4"}` flashes a device or zone white for up to 15 s; `{"target": null}` stops |
| `POST /api/apps`     | `{"title": "...", "allowed": bool}` allows or blocks an app that has used the SDK |

`GET /api/status` also reports each device's game canvas (`game`), the apps that
have used the SDK (`apps`), who provides the system SDK DLLs (`sdk_dlls`) and the
current Identify target. It is cheap enough to poll several times a second for
live previews, which is what the app does.

## Known limitations

- **DLL signature checks.** Some games built on Razer's newer C++ SDK wrapper
  (`CChromaEditorLibrary`) check that `RzChromaSDK64.dll` is signed by Razer
  before loading it. They return `RZRESULT_DLL_INVALID_SIGNATURE`. Those games
  will reject OpenChroma's DLL, and no third-party DLL can get around that.
  Games that use the REST API or load the DLL directly aren't affected.
- Reactive effects (keys lighting up when pressed) are accepted but shown as
  "off". The service doesn't see your key presses.
- Only the HTTP REST endpoint is implemented. The HTTPS endpoint on
  `chromasdk.io:54236` isn't.
- Basilisk V3 underglow LED order and the Chroma Link zone spread are best
  guesses. If something lights up in the wrong place, adjust the tables in
  `crates/openchroma/src/layout.rs`.

## Development

```
cargo test --workspace
cargo run -p razer-hid --example probe          # read-only firmware/serial query
cargo run -p razer-hid --example bench          # frame throughput (changes lights)
cargo run -p rzchromasdk --example smoke -- <path to DLL>   # LoadLibrary test against a running service
```

To test the REST API while Razer's service still owns 54235, set
`OPENCHROMA_SDK_PORT` to another port before starting the service.

For screenshots of the app, `OPENCHROMA_APP_PAGE` (0-3) opens a page and
`OPENCHROMA_APP_SIZE` (for example `1280x820`) sets the window size.

## License

GPL-2.0-or-later ([`LICENSE`](LICENSE)). The device tables are derived from
OpenRGB, which is GPL-2.0-or-later.

The desktop app (`crates/openchroma-app`) is GPL-3.0-or-later
([`crates/openchroma-app/LICENSE`](crates/openchroma-app/LICENSE)), because it
uses Slint under the GPLv3. It embeds the IBM Plex fonts, which are under the
SIL Open Font License (`crates/openchroma-app/ui/fonts/LICENSE.txt`).

OpenChroma is provided as is, without warranty of any kind, and you use it at
your own risk. It sends commands directly to your devices over USB; see the
licenses for the full disclaimer of warranty and limitation of liability.
OpenChroma is not affiliated with or endorsed by Razer or ASUS. Razer, Chroma
and ASUS Aura are trademarks of their respective owners.
