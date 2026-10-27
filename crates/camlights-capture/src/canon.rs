//! Canon EOS live view through Canon's EDSDK, loaded at runtime from a user-provided copy.
//!
//! EDSDK is not redistributable, so CamLights never ships it: the user points CamLights at the
//! folder holding `EDSDK.dll` (and `EdsImage.dll` when their SDK version has one).

use std::ffi::{CStr, c_char, c_void};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use camlights_core::Frame;
use libloading::Library;
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

use crate::{Backend, DeviceInfo, FrameSource, VideoFormat};

type EdsError = u32;
type EdsRef = *mut c_void;

const ERR_OK: EdsError = 0x0000;
const ERR_DEVICE_BUSY: EdsError = 0x0081;
const ERR_OBJECT_NOTREADY: EdsError = 0xA102;

const PROP_EVF_OUTPUT_DEVICE: u32 = 0x0500;
const PROP_EVF_MODE: u32 = 0x0501;
const EVF_OUTPUT_DEVICE_PC: u32 = 2;

/// Live view frames arrive at up to ~30 fps; polling faster only returns duplicates.
const FRAME_INTERVAL: Duration = Duration::from_millis(33);
/// The camera needs a moment after enabling PC live view before the first frame.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);

const DLL_NAME: &str = "EDSDK.dll";

#[repr(C)]
struct EdsDeviceInfo {
    port_name: [c_char; 256],
    description: [c_char; 256],
    device_sub_type: u32,
    reserved: u32,
}

struct Api {
    initialize: unsafe extern "system" fn() -> EdsError,
    terminate: unsafe extern "system" fn() -> EdsError,
    get_camera_list: unsafe extern "system" fn(*mut EdsRef) -> EdsError,
    get_child_count: unsafe extern "system" fn(EdsRef, *mut u32) -> EdsError,
    get_child_at_index: unsafe extern "system" fn(EdsRef, i32, *mut EdsRef) -> EdsError,
    get_device_info: unsafe extern "system" fn(EdsRef, *mut EdsDeviceInfo) -> EdsError,
    retain: unsafe extern "system" fn(EdsRef) -> u32,
    release: unsafe extern "system" fn(EdsRef) -> u32,
    open_session: unsafe extern "system" fn(EdsRef) -> EdsError,
    close_session: unsafe extern "system" fn(EdsRef) -> EdsError,
    get_property_data: unsafe extern "system" fn(EdsRef, u32, i32, u32, *mut c_void) -> EdsError,
    set_property_data: unsafe extern "system" fn(EdsRef, u32, i32, u32, *const c_void) -> EdsError,
    create_memory_stream: unsafe extern "system" fn(u64, *mut EdsRef) -> EdsError,
    create_evf_image_ref: unsafe extern "system" fn(EdsRef, *mut EdsRef) -> EdsError,
    download_evf_image: unsafe extern "system" fn(EdsRef, EdsRef) -> EdsError,
    get_pointer: unsafe extern "system" fn(EdsRef, *mut *mut c_void) -> EdsError,
    get_length: unsafe extern "system" fn(EdsRef, *mut u64) -> EdsError,
    get_event: unsafe extern "system" fn() -> EdsError,
}

/// An initialised EDSDK. Must stay on the thread that loaded it; terminated on drop.
pub struct Sdk {
    api: Api,
    path: PathBuf,
    _library: Library,
}

fn check(code: EdsError, what: &str) -> Result<()> {
    if code == ERR_OK { Ok(()) } else { Err(anyhow!("{what} failed with EDSDK error 0x{code:08X}")) }
}

/// Candidate locations: the chosen folder, else an `EDSDK` folder or the DLL next to CamLights.
fn candidates(dir: Option<&Path>) -> Vec<PathBuf> {
    if let Some(dir) = dir {
        return vec![dir.join(DLL_NAME)];
    }
    let Ok(exe) = std::env::current_exe() else { return Vec::new() };
    let Some(exe_dir) = exe.parent() else { return Vec::new() };
    vec![exe_dir.join("EDSDK").join(DLL_NAME), exe_dir.join(DLL_NAME)]
}

impl Sdk {
    pub fn load(dir: Option<&Path>) -> Result<Rc<Self>> {
        let candidates = candidates(dir);
        let Some(path) = candidates.iter().find(|p| p.is_file()).cloned() else {
            let looked: Vec<String> = candidates.iter().map(|p| p.display().to_string()).collect();
            bail!("{DLL_NAME} not found (looked in {})", looked.join(", "));
        };
        unsafe {
            let library = Library::new(&path).with_context(|| format!("loading {}", path.display()))?;
            macro_rules! sym {
                ($name:literal) => {
                    *library.get($name).with_context(|| format!("{} has no {}", path.display(), $name))?
                };
            }
            let api = Api {
                initialize: sym!("EdsInitializeSDK"),
                terminate: sym!("EdsTerminateSDK"),
                get_camera_list: sym!("EdsGetCameraList"),
                get_child_count: sym!("EdsGetChildCount"),
                get_child_at_index: sym!("EdsGetChildAtIndex"),
                get_device_info: sym!("EdsGetDeviceInfo"),
                retain: sym!("EdsRetain"),
                release: sym!("EdsRelease"),
                open_session: sym!("EdsOpenSession"),
                close_session: sym!("EdsCloseSession"),
                get_property_data: sym!("EdsGetPropertyData"),
                set_property_data: sym!("EdsSetPropertyData"),
                create_memory_stream: sym!("EdsCreateMemoryStream"),
                create_evf_image_ref: sym!("EdsCreateEvfImageRef"),
                download_evf_image: sym!("EdsDownloadEvfImage"),
                get_pointer: sym!("EdsGetPointer"),
                get_length: sym!("EdsGetLength"),
                get_event: sym!("EdsGetEvent"),
            };
            check((api.initialize)(), "EdsInitializeSDK")?;
            log::info!("loaded Canon EDSDK from {}", path.display());
            Ok(Rc::new(Self { api, path, _library: library }))
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Lets EDSDK deliver queued camera events; required on threads without a message loop.
    fn pump(&self) {
        unsafe { (self.api.get_event)() };
    }

    fn release(&self, object: EdsRef) {
        if !object.is_null() {
            unsafe { (self.api.release)(object) };
        }
    }

    /// Calls `visit` with each connected camera and its device info; stops at the first `Some`.
    fn find_camera<T>(&self, mut visit: impl FnMut(EdsRef, &EdsDeviceInfo) -> Option<T>) -> Result<Option<T>> {
        self.pump();
        let mut list = std::ptr::null_mut();
        unsafe { check((self.api.get_camera_list)(&mut list), "EdsGetCameraList")? };
        let mut count = 0;
        let result = (|| {
            unsafe { check((self.api.get_child_count)(list, &mut count), "EdsGetChildCount")? };
            for index in 0..count as i32 {
                let mut camera = std::ptr::null_mut();
                unsafe { check((self.api.get_child_at_index)(list, index, &mut camera), "EdsGetChildAtIndex")? };
                let mut info: EdsDeviceInfo = unsafe { std::mem::zeroed() };
                let found = unsafe { check((self.api.get_device_info)(camera, &mut info), "EdsGetDeviceInfo") }
                    .ok()
                    .and_then(|()| visit(camera, &info));
                self.release(camera);
                if found.is_some() {
                    return Ok(found);
                }
            }
            Ok(None)
        })();
        self.release(list);
        result
    }
}

impl Drop for Sdk {
    fn drop(&mut self) {
        unsafe { (self.api.terminate)() };
    }
}

fn text(chars: &[c_char; 256]) -> String {
    unsafe { CStr::from_ptr(chars.as_ptr()) }.to_string_lossy().into_owned()
}

fn live_view_format() -> VideoFormat {
    VideoFormat { width: 0, height: 0, fps_num: 30, fps_den: 1, encoding: "live view".to_owned() }
}

pub fn list_devices(sdk: &Sdk) -> Result<Vec<DeviceInfo>> {
    let mut devices = Vec::new();
    sdk.find_camera::<()>(|_, info| {
        devices.push(DeviceInfo { backend: Backend::Canon, name: text(&info.description), id: text(&info.port_name) });
        None
    })?;
    Ok(devices)
}

pub struct CanonSource {
    sdk: Rc<Sdk>,
    device: DeviceInfo,
    formats: Vec<VideoFormat>,
    camera: EdsRef,
    stream: EdsRef,
    evf_image: EdsRef,
    /// The live view output setting before we added the PC, restored on close.
    original_output: Option<u32>,
    last_frame: Option<Instant>,
}

impl CanonSource {
    pub fn open(sdk: Rc<Sdk>, device: &DeviceInfo) -> Result<Self> {
        let camera = sdk
            .find_camera(|camera, info| {
                (text(&info.port_name) == device.id).then(|| {
                    // Our own reference, balancing find_camera's release of the list entry.
                    unsafe { (sdk.api.retain)(camera) };
                    camera
                })
            })?
            .ok_or_else(|| anyhow!("{} is no longer connected", device.name))?;

        // Construct first so Drop cleans up whatever the remaining steps managed to set up.
        let mut source = Self {
            sdk,
            device: device.clone(),
            formats: vec![live_view_format()],
            camera,
            stream: std::ptr::null_mut(),
            evf_image: std::ptr::null_mut(),
            original_output: None,
            last_frame: None,
        };
        source.start_live_view().with_context(|| {
            format!(
                "starting live view on {} (only one program can control the camera: close EOS Utility, \
                 EOS Webcam Utility or SparkoCam if they are running)",
                device.name
            )
        })?;
        log::info!("opened {} through EDSDK", device.name);
        Ok(source)
    }

    fn start_live_view(&mut self) -> Result<()> {
        let api = &self.sdk.api;
        unsafe {
            check((api.open_session)(self.camera), "EdsOpenSession")?;
            // Some bodies need live view mode enabled explicitly; others reject the property.
            let on: u32 = 1;
            let _ = (api.set_property_data)(self.camera, PROP_EVF_MODE, 0, 4, (&raw const on).cast());

            let mut output = 0u32;
            check((api.get_property_data)(self.camera, PROP_EVF_OUTPUT_DEVICE, 0, 4, (&raw mut output).cast()), "reading live view output")?;
            self.original_output = Some(output);
            let with_pc = output | EVF_OUTPUT_DEVICE_PC;
            check((api.set_property_data)(self.camera, PROP_EVF_OUTPUT_DEVICE, 0, 4, (&raw const with_pc).cast()), "routing live view to the PC")?;

            check((api.create_memory_stream)(0, &mut self.stream), "EdsCreateMemoryStream")?;
            check((api.create_evf_image_ref)(self.stream, &mut self.evf_image), "EdsCreateEvfImageRef")?;
        }
        Ok(())
    }

    fn decode(&self) -> Result<Frame> {
        let api = &self.sdk.api;
        let mut data = std::ptr::null_mut();
        let mut len = 0u64;
        unsafe {
            check((api.get_pointer)(self.stream, &mut data), "EdsGetPointer")?;
            check((api.get_length)(self.stream, &mut len), "EdsGetLength")?;
        }
        if data.is_null() || len == 0 {
            bail!("live view returned an empty image");
        }
        let jpeg = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), len as usize) };
        let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
        let mut decoder = JpegDecoder::new_with_options(ZCursor::new(jpeg), options);
        let rgba = decoder.decode().map_err(|e| anyhow!("decoding live view JPEG: {e:?}"))?;
        let info = decoder.info().ok_or_else(|| anyhow!("live view JPEG has no header"))?;
        let (width, height) = (info.width as u32, info.height as u32);
        if rgba.len() != width as usize * height as usize * 4 {
            bail!("live view JPEG decoded to {} bytes for {width}×{height}", rgba.len());
        }
        Ok(Frame { width, height, data: rgba })
    }
}

impl FrameSource for CanonSource {
    fn device(&self) -> &DeviceInfo {
        &self.device
    }

    fn formats(&self) -> &[VideoFormat] {
        &self.formats
    }

    fn format(&self) -> &VideoFormat {
        &self.formats[0]
    }

    fn next_frame(&mut self) -> Result<Frame> {
        if let Some(last) = self.last_frame {
            let elapsed = last.elapsed();
            if elapsed < FRAME_INTERVAL {
                std::thread::sleep(FRAME_INTERVAL - elapsed);
            }
        }
        let started = Instant::now();
        loop {
            self.sdk.pump();
            let code = unsafe { (self.sdk.api.download_evf_image)(self.camera, self.evf_image) };
            match code {
                ERR_OK => break,
                ERR_OBJECT_NOTREADY | ERR_DEVICE_BUSY if started.elapsed() < FIRST_FRAME_TIMEOUT => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => check(code, "EdsDownloadEvfImage")?,
            }
        }
        self.last_frame = Some(Instant::now());
        self.decode()
    }
}

impl Drop for CanonSource {
    fn drop(&mut self) {
        let api = &self.sdk.api;
        unsafe {
            if let Some(output) = self.original_output {
                // Give live view back to the camera's own screen.
                let _ = (api.set_property_data)(self.camera, PROP_EVF_OUTPUT_DEVICE, 0, 4, (&raw const output).cast());
            }
            self.sdk.release(self.evf_image);
            self.sdk.release(self.stream);
            let _ = (api.close_session)(self.camera);
        }
        self.sdk.release(self.camera);
    }
}
