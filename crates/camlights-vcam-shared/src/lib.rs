//! The contract between the CamLights app and its virtual camera media source.
//!
//! The media source runs inside the Windows Camera Frame Server service, so it creates a
//! `Global\` section (services hold the privilege to do that) and the app opens it. The app
//! writes NV12 frames under a sequence lock; the source publishes the resolution its client
//! selected and a heartbeat so the app only converts frames while someone is watching.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    OpenFileMappingW, PAGE_READWRITE, UnmapViewOfFile,
};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::core::{GUID, HSTRING, w};

pub const CLSID_VIRTUAL_CAMERA: GUID = GUID::from_u128(0x076f848b_f97d_45c4_a7dc_cf4642fdcca6);
pub const CLSID_STRING: &str = "{076F848B-F97D-45C4-A7DC-CF4642FDCCA6}";
/// Name under which applications list the virtual camera.
pub const FRIENDLY_NAME: &str = "CamLights Camera";

pub const MAX_WIDTH: u32 = 1920;
pub const MAX_HEIGHT: u32 = 1080;
pub const FPS: u32 = 30;

/// A frame or heartbeat older than this is treated as absent.
pub const STALE_AFTER_MS: u64 = 1500;

const MAPPING_NAME: &str = r"Global\CamLightsVirtualCamera";
const MAGIC: u32 = u32::from_le_bytes(*b"CLvc");
const VERSION: u32 = 1;
const HEADER_BYTES: usize = 64;
const DATA_CAPACITY: usize = nv12_len(MAX_WIDTH, MAX_HEIGHT);
const MAPPING_BYTES: usize = HEADER_BYTES + DATA_CAPACITY;

pub const fn nv12_len(width: u32, height: u32) -> usize {
    width as usize * height as usize * 3 / 2
}

pub fn now_ms() -> u64 {
    unsafe { GetTickCount64() }
}

#[repr(C)]
struct Header {
    magic: AtomicU32,
    version: AtomicU32,
    // Written by the media source.
    requested_width: AtomicU32,
    requested_height: AtomicU32,
    consumer_tick: AtomicU64,
    // Written by the app, guarded by `seq` (odd while a write is in progress).
    seq: AtomicU64,
    width: AtomicU32,
    height: AtomicU32,
    producer_tick: AtomicU64,
}

const _: () = assert!(size_of::<Header>() <= HEADER_BYTES);

/// A mapped view of the frame channel.
pub struct FrameChannel {
    mapping: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}

// The view is shared memory accessed only through atomics and the sequence lock.
unsafe impl Send for FrameChannel {}
unsafe impl Sync for FrameChannel {}

impl FrameChannel {
    /// Creates (or opens an existing) channel. Called by the media source in the Frame Server.
    pub fn create() -> windows::core::Result<Self> {
        // SYSTEM, LocalService and administrators get full access; interactive users read and write.
        let sddl = w!("D:P(A;;GA;;;SY)(A;;GA;;;LS)(A;;GA;;;BA)(A;;GRGW;;;IU)");
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl, SDDL_REVISION_1, &mut descriptor, None)?;
            let attributes = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.0,
                bInheritHandle: false.into(),
            };
            let mapping = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                Some(&attributes),
                PAGE_READWRITE,
                0,
                MAPPING_BYTES as u32,
                &HSTRING::from(MAPPING_NAME),
            );
            let _ = LocalFree(Some(HLOCAL(descriptor.0)));
            let channel = Self::map(mapping?)?;
            let header = channel.header();
            if header.magic.load(Ordering::Acquire) != MAGIC {
                header.version.store(VERSION, Ordering::Relaxed);
                header.magic.store(MAGIC, Ordering::Release);
            }
            Ok(channel)
        }
    }

    /// Opens the channel created by a running media source. Called by the app.
    pub fn open() -> windows::core::Result<Self> {
        let mapping = unsafe { OpenFileMappingW((FILE_MAP_READ | FILE_MAP_WRITE).0, false, &HSTRING::from(MAPPING_NAME))? };
        let channel = Self::map(mapping)?;
        let header = channel.header();
        if header.magic.load(Ordering::Acquire) != MAGIC || header.version.load(Ordering::Relaxed) != VERSION {
            return Err(windows::core::Error::new(
                windows::Win32::Foundation::E_UNEXPECTED,
                "virtual camera channel has an incompatible layout",
            ));
        }
        Ok(channel)
    }

    fn map(mapping: HANDLE) -> windows::core::Result<Self> {
        let view = unsafe { MapViewOfFile(mapping, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, MAPPING_BYTES) };
        if view.Value.is_null() {
            let error = windows::core::Error::from_thread();
            unsafe {
                let _ = CloseHandle(mapping);
            }
            return Err(error);
        }
        Ok(Self { mapping, view })
    }

    fn header(&self) -> &Header {
        unsafe { &*(self.view.Value as *const Header) }
    }

    fn data(&self) -> *mut u8 {
        unsafe { (self.view.Value as *mut u8).add(HEADER_BYTES) }
    }

    // ---- media source side ----

    /// Records the resolution the client selected and that a client is reading frames now.
    pub fn announce_consumer(&self, width: u32, height: u32) {
        let header = self.header();
        header.requested_width.store(width, Ordering::Relaxed);
        header.requested_height.store(height, Ordering::Relaxed);
        header.consumer_tick.store(now_ms(), Ordering::Release);
    }

    /// Copies the latest frame into `out` if it is fresh and exactly `width`×`height`.
    pub fn read_frame(&self, width: u32, height: u32, out: &mut [u8]) -> bool {
        let header = self.header();
        let len = nv12_len(width, height);
        if out.len() < len || len > DATA_CAPACITY {
            return false;
        }
        let before = header.seq.load(Ordering::Acquire);
        if before % 2 == 1
            || header.width.load(Ordering::Relaxed) != width
            || header.height.load(Ordering::Relaxed) != height
            || now_ms().saturating_sub(header.producer_tick.load(Ordering::Relaxed)) > STALE_AFTER_MS
        {
            return false;
        }
        unsafe { std::ptr::copy_nonoverlapping(self.data(), out.as_mut_ptr(), len) };
        fence(Ordering::Acquire);
        header.seq.load(Ordering::Relaxed) == before
    }

    // ---- app side ----

    /// The resolution a client is reading, if any client read within [`STALE_AFTER_MS`].
    pub fn active_request(&self) -> Option<(u32, u32)> {
        let header = self.header();
        let tick = header.consumer_tick.load(Ordering::Acquire);
        if now_ms().saturating_sub(tick) > STALE_AFTER_MS {
            return None;
        }
        let (width, height) = (header.requested_width.load(Ordering::Relaxed), header.requested_height.load(Ordering::Relaxed));
        (width > 0 && height > 0 && nv12_len(width, height) <= DATA_CAPACITY).then_some((width, height))
    }

    /// Publishes a `width`×`height` NV12 frame produced by `fill`.
    pub fn write_frame(&self, width: u32, height: u32, fill: impl FnOnce(&mut [u8])) {
        let len = nv12_len(width, height);
        assert!(len <= DATA_CAPACITY, "frame exceeds the channel capacity");
        let header = self.header();
        let seq = header.seq.load(Ordering::Relaxed);
        header.seq.store(seq | 1, Ordering::Relaxed);
        fence(Ordering::Release);
        header.width.store(width, Ordering::Relaxed);
        header.height.store(height, Ordering::Relaxed);
        fill(unsafe { std::slice::from_raw_parts_mut(self.data(), len) });
        header.producer_tick.store(now_ms(), Ordering::Relaxed);
        header.seq.store((seq | 1) + 1, Ordering::Release);
    }
}

impl Drop for FrameChannel {
    fn drop(&mut self) {
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.mapping);
        }
    }
}
