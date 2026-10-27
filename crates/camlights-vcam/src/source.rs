//! The live media source: one NV12 video stream at a few fixed resolutions.

use std::sync::Mutex;

use windows::Win32::Foundation::{E_INVALIDARG, ERROR_SET_NOT_FOUND, S_OK};
use windows::Win32::Media::KernelStreaming::{IKsControl, IKsControl_Impl, KSIDENTIFIER, PINNAME_VIDEO_CAPTURE};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::core::{BOOL, ComObject, GUID, IUnknown, Interface, Ref, implement};

use crate::log::vlog;
use crate::stream::MediaStream;

const STREAM_ID: u32 = 0;
/// Offered resolutions; the first is the default.
const SIZES: [(u32, u32); 3] = [(1280, 720), (1920, 1080), (640, 480)];

fn pack(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | low as u64
}

pub fn frame_size(media_type: &IMFMediaType) -> windows::core::Result<(u32, u32)> {
    let packed = unsafe { media_type.GetUINT64(&MF_MT_FRAME_SIZE)? };
    Ok(((packed >> 32) as u32, packed as u32))
}

fn nv12_type(width: u32, height: u32) -> windows::core::Result<IMFMediaType> {
    unsafe {
        let media_type = MFCreateMediaType()?;
        media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        media_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        media_type.SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height))?;
        media_type.SetUINT64(&MF_MT_FRAME_RATE, pack(camlights_vcam_shared::FPS, 1))?;
        media_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        media_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        media_type.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
        media_type.SetUINT32(&MF_MT_FIXED_SIZE_SAMPLES, 1)?;
        media_type.SetUINT32(&MF_MT_SAMPLE_SIZE, camlights_vcam_shared::nv12_len(width, height) as u32)?;
        media_type.SetUINT32(&MF_MT_DEFAULT_STRIDE, width)?;
        media_type.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        media_type.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
        Ok(media_type)
    }
}

#[implement(IMFMediaSourceEx, IMFGetService, IKsControl)]
pub struct MediaSource {
    state: Mutex<State>,
}

struct State {
    /// `None` once shut down.
    queue: Option<IMFMediaEventQueue>,
    presentation: IMFPresentationDescriptor,
    stream_descriptor: IMFStreamDescriptor,
    attributes: IMFAttributes,
    stream: Option<ComObject<MediaStream>>,
    stream_announced: bool,
}

impl MediaSource {
    pub fn new(activate_attributes: &IMFAttributes) -> windows::core::Result<IMFMediaSource> {
        unsafe {
            let types = SIZES.iter().map(|&(w, h)| nv12_type(w, h).map(Some)).collect::<windows::core::Result<Vec<_>>>()?;
            let stream_descriptor = MFCreateStreamDescriptor(STREAM_ID, &types)?;
            stream_descriptor.SetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY, &PINNAME_VIDEO_CAPTURE)?;
            stream_descriptor.SetUINT32(&MF_DEVICESTREAM_STREAM_ID, STREAM_ID)?;
            stream_descriptor.SetUINT32(&MF_DEVICESTREAM_FRAMESERVER_SHARED, 1)?;
            stream_descriptor.SetUINT32(&MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES, MFFrameSourceTypes_Color.0 as u32)?;
            stream_descriptor.GetMediaTypeHandler()?.SetCurrentMediaType(types[0].as_ref())?;

            let presentation = MFCreatePresentationDescriptor(Some(&[Some(stream_descriptor.clone())]))?;
            presentation.SelectStream(0)?;

            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 1)?;
            let attributes = attributes.ok_or_else(|| windows::core::Error::from(E_INVALIDARG))?;
            activate_attributes.CopyAllItems(&attributes)?;

            let source = ComObject::new(MediaSource {
                state: Mutex::new(State {
                    queue: Some(MFCreateEventQueue()?),
                    presentation,
                    stream_descriptor: stream_descriptor.clone(),
                    attributes,
                    stream: None,
                    stream_announced: false,
                }),
            });
            let interface: IMFMediaSource = source.to_interface::<IMFMediaSourceEx>().cast()?;
            // The stream refers back to the source until Shutdown breaks the cycle.
            let stream = MediaStream::new(interface.clone(), stream_descriptor)?;
            source.lock().stream = Some(stream);
            Ok(interface)
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn queue(&self) -> windows::core::Result<IMFMediaEventQueue> {
        self.lock().queue.clone().ok_or_else(|| MF_E_SHUTDOWN.into())
    }
}

impl IMFMediaEventGenerator_Impl for MediaSource_Impl {
    fn GetEvent(&self, flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> windows::core::Result<IMFMediaEvent> {
        // Never hold the state lock here: a blocking GetEvent would stall every other call.
        let queue = self.queue()?;
        unsafe { queue.GetEvent(flags.0 as u32) }
    }
    fn BeginGetEvent(&self, callback: Ref<IMFAsyncCallback>, state: Ref<IUnknown>) -> windows::core::Result<()> {
        unsafe { self.queue()?.BeginGetEvent(callback.as_ref(), state.as_ref()) }
    }
    fn EndGetEvent(&self, result: Ref<IMFAsyncResult>) -> windows::core::Result<IMFMediaEvent> {
        unsafe { self.queue()?.EndGetEvent(result.as_ref()) }
    }
    fn QueueEvent(&self, kind: u32, extended: *const GUID, status: windows::core::HRESULT, value: *const PROPVARIANT) -> windows::core::Result<()> {
        unsafe { self.queue()?.QueueEventParamVar(kind, extended, status, value) }
    }
}

impl IMFMediaSource_Impl for MediaSource_Impl {
    fn GetCharacteristics(&self) -> windows::core::Result<u32> {
        self.queue()?;
        Ok(MFMEDIASOURCE_IS_LIVE.0 as u32)
    }

    fn CreatePresentationDescriptor(&self) -> windows::core::Result<IMFPresentationDescriptor> {
        let state = self.lock();
        if state.queue.is_none() {
            return Err(MF_E_SHUTDOWN.into());
        }
        unsafe { state.presentation.Clone() }
    }

    fn Start(&self, presentation: Ref<IMFPresentationDescriptor>, time_format: *const GUID, start: *const PROPVARIANT) -> windows::core::Result<()> {
        let presentation = presentation.ok()?;
        if !time_format.is_null() && unsafe { *time_format } != GUID::zeroed() {
            return Err(MF_E_UNSUPPORTED_TIME_FORMAT.into());
        }
        let mut state = self.lock();
        let queue = state.queue.clone().ok_or_else(|| windows::core::Error::from(MF_E_SHUTDOWN))?;
        let stream = state.stream.clone().ok_or_else(|| windows::core::Error::from(MF_E_SHUTDOWN))?;

        let mut selected = BOOL(0);
        let mut descriptor = None;
        unsafe { presentation.GetStreamDescriptorByIndex(0, &mut selected, &mut descriptor)? };
        let descriptor = descriptor.ok_or_else(|| windows::core::Error::from(E_INVALIDARG))?;
        if selected.as_bool() {
            let media_type = unsafe { descriptor.GetMediaTypeHandler()?.GetCurrentMediaType()? };
            let (width, height) = frame_size(&media_type)?;
            vlog!("start {width}x{height}");
            let event = if state.stream_announced { MEUpdatedStream } else { MENewStream };
            state.stream_announced = true;
            let unknown: IUnknown = stream.to_interface();
            unsafe { queue.QueueEventParamUnk(event.0 as u32, &GUID::zeroed(), S_OK, &unknown)? };
            stream.start(width, height)?;
        }
        unsafe { queue.QueueEventParamVar(MESourceStarted.0 as u32, &GUID::zeroed(), S_OK, start) }
    }

    fn Stop(&self) -> windows::core::Result<()> {
        let state = self.lock();
        let queue = state.queue.clone().ok_or_else(|| windows::core::Error::from(MF_E_SHUTDOWN))?;
        vlog!("stop");
        if let Some(stream) = &state.stream {
            stream.stop()?;
        }
        unsafe { queue.QueueEventParamVar(MESourceStopped.0 as u32, &GUID::zeroed(), S_OK, &PROPVARIANT::default()) }
    }

    fn Pause(&self) -> windows::core::Result<()> {
        Err(MF_E_INVALID_STATE_TRANSITION.into())
    }

    fn Shutdown(&self) -> windows::core::Result<()> {
        let mut state = self.lock();
        vlog!("shutdown");
        if let Some(stream) = state.stream.take() {
            stream.shutdown();
        }
        match state.queue.take() {
            Some(queue) => unsafe { queue.Shutdown() },
            None => Err(MF_E_SHUTDOWN.into()),
        }
    }
}

impl IMFMediaSourceEx_Impl for MediaSource_Impl {
    fn GetSourceAttributes(&self) -> windows::core::Result<IMFAttributes> {
        Ok(self.lock().attributes.clone())
    }

    fn GetStreamAttributes(&self, stream: u32) -> windows::core::Result<IMFAttributes> {
        if stream != STREAM_ID {
            return Err(E_INVALIDARG.into());
        }
        self.lock().stream_descriptor.cast()
    }

    fn SetD3DManager(&self, _manager: Ref<IUnknown>) -> windows::core::Result<()> {
        // Samples are system-memory buffers, so a D3D manager is not needed.
        Ok(())
    }
}

impl IMFGetService_Impl for MediaSource_Impl {
    fn GetService(&self, _service: *const GUID, _riid: *const GUID, _object: *mut *mut core::ffi::c_void) -> windows::core::Result<()> {
        Err(MF_E_UNSUPPORTED_SERVICE.into())
    }
}

// The Frame Server probes camera controls through IKsControl; this camera exposes none.
impl IKsControl_Impl for MediaSource_Impl {
    fn KsProperty(&self, _: *const KSIDENTIFIER, _: u32, _: *mut core::ffi::c_void, _: u32, _: *mut u32) -> windows::core::Result<()> {
        Err(ERROR_SET_NOT_FOUND.to_hresult().into())
    }
    fn KsMethod(&self, _: *const KSIDENTIFIER, _: u32, _: *mut core::ffi::c_void, _: u32, _: *mut u32) -> windows::core::Result<()> {
        Err(ERROR_SET_NOT_FOUND.to_hresult().into())
    }
    fn KsEvent(&self, _: *const KSIDENTIFIER, _: u32, _: *mut core::ffi::c_void, _: u32, _: *mut u32) -> windows::core::Result<()> {
        Err(ERROR_SET_NOT_FOUND.to_hresult().into())
    }
}
