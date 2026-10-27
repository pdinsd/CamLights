//! Webcams and capture cards through the Media Foundation source reader.

use anyhow::{Context, Result, bail, ensure};
use camlights_core::Frame;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::core::{GUID, HSTRING, Interface, PWSTR};

use crate::{Backend, DeviceInfo, FrameSource, VideoFormat};

/// COM (multithreaded apartment) and Media Foundation for the current thread, released on drop.
pub struct MfRuntime {
    _not_send: std::marker::PhantomData<*const ()>,
}

impl MfRuntime {
    pub fn start() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok().context("CoInitializeEx")?;
            if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
                CoUninitialize();
                return Err(e).context("MFStartup");
            }
        }
        Ok(Self { _not_send: std::marker::PhantomData })
    }
}

impl Drop for MfRuntime {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}

fn create_attributes(count: u32) -> Result<IMFAttributes> {
    let mut attributes = None;
    unsafe { MFCreateAttributes(&mut attributes, count)? };
    attributes.context("MFCreateAttributes returned no object")
}

fn allocated_string(attributes: &IMFAttributes, key: &GUID) -> Result<String> {
    let mut value = PWSTR::null();
    let mut len = 0;
    unsafe {
        attributes.GetAllocatedString(key, &mut value, &mut len)?;
        let text = value.to_string();
        CoTaskMemFree(Some(value.0 as *const _));
        Ok(text?)
    }
}

fn video_capture_attributes() -> Result<IMFAttributes> {
    let attributes = create_attributes(2)?;
    unsafe {
        attributes.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)?;
    }
    Ok(attributes)
}

pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    let attributes = video_capture_attributes()?;
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    unsafe { MFEnumDeviceSources(&attributes, &mut activates, &mut count).context("MFEnumDeviceSources")? };
    if activates.is_null() {
        return Ok(Vec::new());
    }

    let mut devices = Vec::with_capacity(count as usize);
    // Take every entry so each IMFActivate is released before the array itself is freed.
    let entries = unsafe { std::slice::from_raw_parts_mut(activates, count as usize) };
    for activate in entries.iter_mut().filter_map(Option::take) {
        let name = allocated_string(&activate, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME);
        let id = allocated_string(&activate, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK);
        match (name, id) {
            (Ok(name), Ok(id)) => devices.push(DeviceInfo { backend: Backend::MediaFoundation, name, id }),
            (name, id) => log::warn!("skipping capture device without name or link: {name:?} {id:?}"),
        }
    }
    unsafe { CoTaskMemFree(Some(activates as *const _)) };
    Ok(devices)
}

fn encoding_name(subtype: &GUID) -> String {
    let named = [(MFVideoFormat_RGB32, "RGB32"), (MFVideoFormat_RGB24, "RGB24"), (MFVideoFormat_ARGB32, "ARGB32")];
    if let Some((_, name)) = named.iter().find(|(guid, _)| guid == subtype) {
        return (*name).to_owned();
    }
    // Most video subtypes are FOURCC-based: the FOURCC is stored in data1.
    let bytes = subtype.data1.to_le_bytes();
    if bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        String::from_utf8_lossy(&bytes).trim().to_owned()
    } else {
        format!("{subtype:?}")
    }
}

fn split_u64(value: u64) -> (u32, u32) {
    ((value >> 32) as u32, value as u32)
}

fn describe(media_type: &IMFMediaType) -> Result<VideoFormat> {
    unsafe {
        let subtype = media_type.GetGUID(&MF_MT_SUBTYPE)?;
        let (width, height) = split_u64(media_type.GetUINT64(&MF_MT_FRAME_SIZE)?);
        let (fps_num, fps_den) = media_type.GetUINT64(&MF_MT_FRAME_RATE).map(split_u64).unwrap_or((0, 1));
        Ok(VideoFormat { width, height, fps_num, fps_den, encoding: encoding_name(&subtype) })
    }
}

/// Prefers full-rate (≥ 29 fps) modes up to 1080p, then the largest, then the fastest.
fn default_format_rank(format: &VideoFormat) -> (bool, bool, u32, u32) {
    let area = format.width * format.height;
    (format.fps() >= 29.0, area <= 1920 * 1080, area, (format.fps() * 100.0) as u32)
}

pub struct MfSource {
    device: DeviceInfo,
    reader: IMFSourceReader,
    formats: Vec<VideoFormat>,
    format: VideoFormat,
    /// Delivered RGB32 geometry: width, height, signed row stride.
    output: (u32, u32, i32),
    // Keep the device source alive for as long as the reader uses it.
    _media_source: IMFMediaSource,
}

const STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

impl MfSource {
    pub fn open(device: &DeviceInfo, requested: Option<&VideoFormat>) -> Result<Self> {
        let attributes = video_capture_attributes()?;
        let media_source = unsafe {
            attributes.SetString(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, &HSTRING::from(&device.id))?;
            MFCreateDeviceSource(&attributes).with_context(|| format!("opening {}", device.name))?
        };

        let reader_attributes = create_attributes(1)?;
        let reader = unsafe {
            // Lets the reader decode and colour-convert any native format to RGB32.
            reader_attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
            MFCreateSourceReaderFromMediaSource(&media_source, &reader_attributes)?
        };

        let mut native = Vec::new();
        for index in 0.. {
            match unsafe { reader.GetNativeMediaType(STREAM, index) } {
                Ok(media_type) => match describe(&media_type) {
                    Ok(format) => native.push((format, media_type)),
                    Err(e) => log::debug!("skipping native type {index}: {e}"),
                },
                Err(e) if e.code() == MF_E_NO_MORE_TYPES => break,
                Err(e) => return Err(e).context("GetNativeMediaType"),
            }
        }
        ensure!(!native.is_empty(), "{} offers no video formats", device.name);

        let mut formats: Vec<VideoFormat> = Vec::new();
        for (format, _) in &native {
            if !formats.contains(format) {
                formats.push(format.clone());
            }
        }

        let chosen = requested
            .and_then(|want| native.iter().find(|(f, _)| f == want))
            .or_else(|| native.iter().max_by_key(|(f, _)| default_format_rank(f)))
            .expect("native formats are non-empty");
        let format = chosen.0.clone();

        unsafe {
            reader.SetCurrentMediaType(STREAM, None, &chosen.1).with_context(|| format!("selecting {}", format.label()))?;
            let output_type = MFCreateMediaType()?;
            output_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            output_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
            reader.SetCurrentMediaType(STREAM, None, &output_type).context("requesting RGB32 output")?;
        }

        let mut source = Self { device: device.clone(), reader, formats, format, output: (0, 0, 0), _media_source: media_source };
        source.refresh_output()?;
        log::info!("opened {} as {}", source.device.name, source.format.label());
        Ok(source)
    }

    fn refresh_output(&mut self) -> Result<()> {
        let current = unsafe { self.reader.GetCurrentMediaType(STREAM)? };
        let (width, height) = split_u64(unsafe { current.GetUINT64(&MF_MT_FRAME_SIZE)? });
        ensure!(width > 0 && height > 0, "reader reported an empty frame size");
        let stride = unsafe { current.GetUINT32(&MF_MT_DEFAULT_STRIDE) }.map(|s| s as i32).unwrap_or(width as i32 * 4);
        self.output = (width, height, stride);
        Ok(())
    }

    fn convert(&self, sample: &IMFSample) -> Result<Frame> {
        let (width, height, default_stride) = self.output;
        let row_bytes = width as usize * 4;
        let buffer = unsafe { sample.ConvertToContiguousBuffer()? };

        if let Ok(buffer_2d) = buffer.cast::<IMF2DBuffer>() {
            let mut scanline0 = std::ptr::null_mut();
            let mut pitch = 0i32;
            unsafe { buffer_2d.Lock2D(&mut scanline0, &mut pitch)? };
            let rows = (0..height as isize)
                .map(|y| unsafe { std::slice::from_raw_parts(scanline0.offset(y * pitch as isize), row_bytes) });
            let frame = Frame::from_bgrx_rows(width, height, rows);
            unsafe { buffer_2d.Unlock2D()? };
            return Ok(frame);
        }

        let mut data = std::ptr::null_mut();
        let mut len = 0u32;
        unsafe { buffer.Lock(&mut data, None, Some(&mut len))? };
        let abs_stride = default_stride.unsigned_abs() as usize;
        let needed = abs_stride * (height as usize - 1) + row_bytes;
        let frame = if (len as usize) < needed || abs_stride < row_bytes {
            Err(anyhow::anyhow!("sample holds {len} bytes, expected at least {needed}"))
        } else {
            // A negative stride means bottom-up rows: the first visible row is last in memory.
            let first = if default_stride < 0 { abs_stride * (height as usize - 1) } else { 0 };
            let rows = (0..height as isize).map(|y| unsafe {
                std::slice::from_raw_parts(data.add(first).offset(y * default_stride as isize), row_bytes)
            });
            Ok(Frame::from_bgrx_rows(width, height, rows))
        };
        unsafe { buffer.Unlock()? };
        frame
    }
}

impl FrameSource for MfSource {
    fn device(&self) -> &DeviceInfo {
        &self.device
    }

    fn formats(&self) -> &[VideoFormat] {
        &self.formats
    }

    fn format(&self) -> &VideoFormat {
        &self.format
    }

    fn next_frame(&mut self) -> Result<Frame> {
        loop {
            let mut flags = 0u32;
            let mut sample = None;
            unsafe { self.reader.ReadSample(STREAM, 0, None, Some(&mut flags), None, Some(&mut sample))? };
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                bail!("{} stopped streaming", self.device.name);
            }
            if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
                self.refresh_output()?;
            }
            if let Some(sample) = sample {
                return self.convert(&sample);
            }
        }
    }
}
