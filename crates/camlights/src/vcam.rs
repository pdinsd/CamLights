//! Installs the CamLights virtual camera, registers it with Windows and feeds it frames.
//!
//! The Frame Server keeps the media source DLL loaded while any app uses the camera, so the
//! installed copy can never be overwritten in place. Each build is therefore installed under a
//! content-hashed name in Program Files, and copies that are no longer loaded are removed.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use camlights_core::{Frame, fit, rgba_to_nv12};
use camlights_vcam_shared::{CLSID_STRING, FRIENDLY_NAME, FrameChannel, now_ms};
use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows::Win32::Media::MediaFoundation::{
    IMFVirtualCamera, MFCreateVirtualCamera, MFVirtualCameraAccess_CurrentUser, MFVirtualCameraLifetime_Session,
    MFVirtualCameraType_SoftwareCameraSource,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{HSTRING, PCWSTR, w};

const DLL_NAME: &str = "camlights_vcam.dll";
const INSTALLED_PREFIX: &str = "camlights_vcam-";
const CHANNEL_RETRY_MS: u64 = 1000;

const INSTALL_ARG: &str = "--install-virtual-camera";
const UNINSTALL_ARG: &str = "--uninstall-virtual-camera";

/// The DLL shipped next to the executable.
pub fn bundled_dll() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    Ok(exe.with_file_name(DLL_NAME))
}

/// FNV-1a: a stable content fingerprint for naming installed copies.
fn fingerprint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| (hash ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3))
}

fn installed_name(bytes: &[u8]) -> String {
    format!("{INSTALLED_PREFIX}{:016x}.dll", fingerprint(bytes))
}

fn install_dir() -> Result<PathBuf> {
    let program_files = std::env::var_os("ProgramFiles").context("ProgramFiles is not set")?;
    Ok(Path::new(&program_files).join("CamLights").join("VirtualCamera"))
}

/// Whether the registered copy differs from the DLL built alongside this executable.
pub fn registered_is_outdated(registered: &Path) -> bool {
    let Ok(bytes) = bundled_dll().and_then(|p| Ok(std::fs::read(p)?)) else { return false };
    registered.file_name().and_then(|n| n.to_str()) != Some(installed_name(&bytes).as_str())
}

fn regsvr32(dll: &Path, unregister: bool) -> Result<()> {
    let system = std::env::var_os("SystemRoot").context("SystemRoot is not set")?;
    let mut command = Command::new(Path::new(&system).join("System32").join("regsvr32.exe"));
    command.arg("/s");
    if unregister {
        command.arg("/u");
    }
    let status = command.arg(dll).status().context("running regsvr32")?;
    ensure!(status.success(), "regsvr32 {} failed with {status}", dll.display());
    Ok(())
}

/// Deletes installed copies other than `keep`; copies the Frame Server still has loaded stay
/// until a later install or uninstall.
fn remove_stale_copies(dir: &Path, keep: Option<&Path>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let ours = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(INSTALLED_PREFIX) && n.ends_with(".dll"));
        if ours && Some(path.as_path()) != keep {
            let _ = std::fs::remove_file(&path);
        }
    }
}

fn install() -> Result<()> {
    let source = bundled_dll()?;
    let bytes = std::fs::read(&source).with_context(|| format!("reading {}", source.display()))?;
    let dir = install_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let target = dir.join(installed_name(&bytes));
    if !target.exists() {
        std::fs::write(&target, &bytes).with_context(|| format!("writing {}", target.display()))?;
    }
    regsvr32(&target, false)?;
    remove_stale_copies(&dir, Some(&target));
    Ok(())
}

fn uninstall() -> Result<()> {
    if let Some(registered) = registered_dll() {
        regsvr32(&registered, true)?;
    }
    if let Ok(dir) = install_dir() {
        remove_stale_copies(&dir, None);
    }
    Ok(())
}

/// Handles the elevated install/uninstall invocations; returns the exit code when one was requested.
pub fn run_command_line() -> Option<i32> {
    let outcome = match std::env::args().nth(1).as_deref() {
        Some(INSTALL_ARG) => install(),
        Some(UNINSTALL_ARG) => uninstall(),
        _ => return None,
    };
    Some(match outcome {
        Ok(()) => 0,
        Err(e) => {
            log::error!("{e:#}");
            1
        }
    })
}

/// The DLL path COM has registered for our CLSID, if any.
pub fn registered_dll() -> Option<PathBuf> {
    let subkey = HSTRING::from(format!(r"Software\Classes\CLSID\{CLSID_STRING}\InprocServer32"));
    let mut buffer = [0u16; 1024];
    let mut bytes = (buffer.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(HKEY_LOCAL_MACHINE, &subkey, PCWSTR::null(), RRF_RT_REG_SZ, None, Some(buffer.as_mut_ptr().cast()), Some(&mut bytes))
    };
    if status.is_err() {
        return None;
    }
    let len = (bytes as usize / 2).saturating_sub(1);
    Some(PathBuf::from(String::from_utf16_lossy(&buffer[..len])))
}

/// Re-runs CamLights elevated (one UAC prompt) to install or uninstall the virtual camera.
pub fn change_installation(uninstall: bool) -> Result<()> {
    if !uninstall {
        let dll = bundled_dll()?;
        ensure!(dll.exists(), "{} is missing; build the whole workspace", dll.display());
    }
    let exe = HSTRING::from(std::env::current_exe()?.as_os_str());
    let parameters = HSTRING::from(if uninstall { UNINSTALL_ARG } else { INSTALL_ARG });
    unsafe {
        // ShellExecuteEx expects a single-threaded apartment.
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: w!("runas"),
            lpFile: PCWSTR(exe.as_ptr()),
            lpParameters: PCWSTR(parameters.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        let launched = ShellExecuteExW(&mut info);
        if com {
            CoUninitialize();
        }
        launched.context("starting the installer (was the administrator prompt declined?)")?;
        let process = info.hProcess;
        let waited = WaitForSingleObject(process, INFINITE);
        let mut code = 1u32;
        let _ = GetExitCodeProcess(process, &mut code);
        let _ = CloseHandle(process);
        if waited != WAIT_OBJECT_0 || code != 0 {
            bail!("the virtual camera {} failed (exit code {code})", if uninstall { "uninstall" } else { "install" });
        }
    }
    Ok(())
}

/// A registered virtual camera for this login session, removed on drop.
pub struct VirtualCamera {
    camera: IMFVirtualCamera,
    channel: Option<FrameChannel>,
    channel_attempt_ms: Option<u64>,
    nv12: Vec<u8>,
}

impl VirtualCamera {
    /// Must be called on a thread with Media Foundation started.
    pub fn start() -> Result<Self> {
        let camera = unsafe {
            MFCreateVirtualCamera(
                MFVirtualCameraType_SoftwareCameraSource,
                MFVirtualCameraLifetime_Session,
                MFVirtualCameraAccess_CurrentUser,
                &HSTRING::from(FRIENDLY_NAME),
                &HSTRING::from(CLSID_STRING),
                None,
            )
            .context("MFCreateVirtualCamera")?
        };
        unsafe { camera.Start(None).context("starting the virtual camera")? };
        log::info!("virtual camera {FRIENDLY_NAME} registered");
        Ok(Self { camera, channel: None, channel_attempt_ms: None, nv12: Vec::new() })
    }

    /// The resolution a client is reading right now, if any.
    pub fn client(&mut self) -> Option<(u32, u32)> {
        if self.channel.is_none() {
            let now = now_ms();
            if self.channel_attempt_ms.is_some_and(|t| now.saturating_sub(t) < CHANNEL_RETRY_MS) {
                return None;
            }
            self.channel_attempt_ms = Some(now);
            // The media source creates the channel when a client first starts it.
            self.channel = FrameChannel::open().ok();
        }
        self.channel.as_ref()?.active_request()
    }

    /// Sends `frame` to the client, scaled to the resolution it selected, and returns that resolution.
    /// Does nothing without a client.
    pub fn send(&mut self, frame: &Frame) -> Option<(u32, u32)> {
        let (width, height) = self.client()?;
        let fitted;
        let frame = if (frame.width, frame.height) == (width, height) {
            frame
        } else {
            fitted = fit(frame, width, height);
            &fitted
        };
        self.nv12.resize(camlights_vcam_shared::nv12_len(width, height), 0);
        rgba_to_nv12(frame, &mut self.nv12);
        if let Some(channel) = &self.channel {
            channel.write_frame(width, height, |out| out.copy_from_slice(&self.nv12));
        }
        Some((width, height))
    }
}

impl Drop for VirtualCamera {
    fn drop(&mut self) {
        unsafe {
            let _ = self.camera.Remove();
            let _ = self.camera.Shutdown();
        }
    }
}
