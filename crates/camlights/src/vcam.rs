//! Registers the CamLights virtual camera with Windows and feeds it frames.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
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
const CHANNEL_RETRY_MS: u64 = 1000;

/// The DLL shipped next to the executable.
pub fn bundled_dll() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    Ok(exe.with_file_name(DLL_NAME))
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

/// Runs `regsvr32` elevated (one UAC prompt) to register or unregister the bundled DLL.
pub fn run_regsvr32(unregister: bool) -> Result<()> {
    let dll = bundled_dll()?;
    if !unregister && !dll.exists() {
        bail!("{} is missing; build the whole workspace", dll.display());
    }
    let parameters = HSTRING::from(format!("/s {}\"{}\"", if unregister { "/u " } else { "" }, dll.display()));
    unsafe {
        // ShellExecuteEx expects a single-threaded apartment.
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: w!("runas"),
            lpFile: w!("regsvr32.exe"),
            lpParameters: PCWSTR(parameters.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        let launched = ShellExecuteExW(&mut info);
        if com {
            CoUninitialize();
        }
        launched.context("starting regsvr32 (was the administrator prompt declined?)")?;
        let process = info.hProcess;
        let waited = WaitForSingleObject(process, INFINITE);
        let mut code = 1u32;
        let _ = GetExitCodeProcess(process, &mut code);
        let _ = CloseHandle(process);
        if waited != WAIT_OBJECT_0 || code != 0 {
            bail!("regsvr32 failed with exit code {code}");
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
