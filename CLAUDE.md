# CamLights

Original Rust webcam app for Windows 11: camera input → image controls → green screen → overlays → preview and the **CamLights Camera** virtual camera. Written from scratch on public Windows/camera APIs; it is not a copy of any other product. Repository: https://github.com/pdinsd/CamLights (Apache-2.0).

## Toolchain

- Rust is installed at `%USERPROFILE%\.cargo\bin`, which may not be on `PATH` in agent shells: prefix `$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"`.
- MSVC from Visual Studio 2022. The C runtime is linked statically (`.cargo/config.toml`).
- Inno Setup 6.0 is at `C:\Program Files (x86)\Inno Setup 6\ISCC.exe`. Keep `installer/CamLights.iss` compatible with 6.0 (for example `x64`, not `x64compatible`).

## Commands

- `cargo build` builds the app and the virtual camera DLL; `cargo test` runs the tests (image processing lives in `camlights-core`).
- `.\scripts\package.ps1` builds a release and writes `dist\CamLights-<version>.zip` and `dist\CamLights-Setup-<version>.exe`.
- CI (`.github/workflows/ci.yml`) builds and tests on `windows-latest`.

## Architecture

| Crate | Role |
|---|---|
| `crates/camlights` | egui app (`app.rs`), capture thread (`engine.rs`), virtual camera install and frame sending (`vcam.rs`). |
| `crates/camlights-core` | `Frame` (RGBA), image controls, chroma key (`background.rs`), overlays, NV12 conversion. Optimised even in dev builds. |
| `crates/camlights-capture` | Media Foundation cameras (`mf.rs`) and Canon EDSDK loaded at runtime (`canon.rs`). |
| `crates/camlights-vcam` | COM DLL: Media Foundation media source run by the Windows Camera Frame Server (LocalService). |
| `crates/camlights-vcam-shared` | `Global\` shared-memory frame channel (seqlock) and the CLSID `{076F848B-F97D-45C4-A7DC-CF4642FDCCA6}`. |

## Things that are easy to get wrong

- The Frame Server keeps `camlights_vcam.dll` loaded while any app uses the camera. The DLL is therefore installed as a content-hashed copy in `%ProgramFiles%\CamLights\VirtualCamera` by the elevated `CamLights.exe --install-virtual-camera`; the UI offers **Update…** when that copy differs from the build next to the exe.
- The DLL logs to `%ProgramData%\CamLights\vcam-<account>.log` (one file per account, because LocalService cannot append to a file a user created).
- Only one CamLights may run (mutex `Local\CamLights.SingleInstance`); a second copy would fight over the camera.
- Canon EDSDK is not redistributable and is never committed; users point CamLights at their own `EDSDK.dll`.
- Do not commit the stray IDA files `SparkoCam.exe.*` in the repository root.

## Status (2026-10-27)

- Verified by the user: webcam capture, virtual camera in a web softphone, Canon EOS M50 through EOS Webcam Utility, release build, the Inno Setup installer.
- Built, not yet confirmed by the user: overlays with drag, green screen.
- Untested on hardware: direct Canon EDSDK live view (marked experimental in the README).
- Roadmap: AI background blur/replace, face-tracking props, Nikon support, DirectShow-only sources, app icon and code signing.

## Conventions

Conventional Commit messages. Commit and push only when asked. Ask before changing anything public on GitHub (releases, repository settings).
