//! The activation object the Frame Server creates from our CLSID; it hands out the media source.

use std::ffi::c_void;
use std::sync::Mutex;

use windows::Win32::Foundation::E_POINTER;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::core::{BOOL, GUID, Interface, PCWSTR, PWSTR, Ref, implement};

use crate::log::vlog;
use crate::source::MediaSource;

#[implement(IMFActivate)]
pub struct Activate {
    attributes: IMFAttributes,
    source: Mutex<Option<IMFMediaSource>>,
}

impl Activate {
    pub fn new() -> windows::core::Result<IMFActivate> {
        let mut attributes = None;
        unsafe { MFCreateAttributes(&mut attributes, 1)? };
        let attributes = attributes.ok_or_else(|| windows::core::Error::from(E_POINTER))?;
        Ok(Activate { attributes, source: Mutex::new(None) }.into())
    }
}

impl IMFActivate_Impl for Activate_Impl {
    fn ActivateObject(&self, riid: *const GUID, object: *mut *mut c_void) -> windows::core::Result<()> {
        if riid.is_null() || object.is_null() {
            return Err(E_POINTER.into());
        }
        let mut source = self.source.lock().unwrap_or_else(|e| e.into_inner());
        if source.is_none() {
            vlog!("activating media source");
            *source = Some(MediaSource::new(&self.attributes).inspect_err(|e| vlog!("creating media source failed: {e}"))?);
        }
        let source = source.as_ref().expect("source was just created");
        unsafe { source.query(riid, object).ok() }
    }

    fn ShutdownObject(&self) -> windows::core::Result<()> {
        if let Some(source) = self.source.lock().unwrap_or_else(|e| e.into_inner()).take() {
            unsafe { source.Shutdown()? };
        }
        Ok(())
    }

    fn DetachObject(&self) -> windows::core::Result<()> {
        self.source.lock().unwrap_or_else(|e| e.into_inner()).take();
        Ok(())
    }
}

// IMFActivate extends IMFAttributes; every attribute call goes to the backing store.
impl IMFAttributes_Impl for Activate_Impl {
    fn GetItem(&self, key: *const GUID, value: *mut PROPVARIANT) -> windows::core::Result<()> {
        unsafe { self.attributes.GetItem(key, (!value.is_null()).then_some(value)) }
    }
    fn GetItemType(&self, key: *const GUID) -> windows::core::Result<MF_ATTRIBUTE_TYPE> {
        unsafe { self.attributes.GetItemType(key) }
    }
    fn CompareItem(&self, key: *const GUID, value: *const PROPVARIANT) -> windows::core::Result<BOOL> {
        unsafe { self.attributes.CompareItem(key, value) }
    }
    fn Compare(&self, theirs: Ref<IMFAttributes>, match_type: MF_ATTRIBUTES_MATCH_TYPE) -> windows::core::Result<BOOL> {
        unsafe { self.attributes.Compare(theirs.as_ref(), match_type) }
    }
    fn GetUINT32(&self, key: *const GUID) -> windows::core::Result<u32> {
        unsafe { self.attributes.GetUINT32(key) }
    }
    fn GetUINT64(&self, key: *const GUID) -> windows::core::Result<u64> {
        unsafe { self.attributes.GetUINT64(key) }
    }
    fn GetDouble(&self, key: *const GUID) -> windows::core::Result<f64> {
        unsafe { self.attributes.GetDouble(key) }
    }
    fn GetGUID(&self, key: *const GUID) -> windows::core::Result<GUID> {
        unsafe { self.attributes.GetGUID(key) }
    }
    fn GetStringLength(&self, key: *const GUID) -> windows::core::Result<u32> {
        unsafe { self.attributes.GetStringLength(key) }
    }
    fn GetString(&self, key: *const GUID, value: PWSTR, capacity: u32, length: *mut u32) -> windows::core::Result<()> {
        let buffer = unsafe { std::slice::from_raw_parts_mut(value.0, capacity as usize) };
        unsafe { self.attributes.GetString(key, buffer, (!length.is_null()).then_some(length)) }
    }
    fn GetAllocatedString(&self, key: *const GUID, value: *mut PWSTR, length: *mut u32) -> windows::core::Result<()> {
        unsafe { self.attributes.GetAllocatedString(key, value, length) }
    }
    fn GetBlobSize(&self, key: *const GUID) -> windows::core::Result<u32> {
        unsafe { self.attributes.GetBlobSize(key) }
    }
    fn GetBlob(&self, key: *const GUID, buffer: *mut u8, capacity: u32, size: *mut u32) -> windows::core::Result<()> {
        let buffer = unsafe { std::slice::from_raw_parts_mut(buffer, capacity as usize) };
        unsafe { self.attributes.GetBlob(key, buffer, (!size.is_null()).then_some(size)) }
    }
    fn GetAllocatedBlob(&self, key: *const GUID, buffer: *mut *mut u8, size: *mut u32) -> windows::core::Result<()> {
        unsafe { self.attributes.GetAllocatedBlob(key, buffer, size) }
    }
    fn GetUnknown(&self, key: *const GUID, riid: *const GUID, object: *mut *mut c_void) -> windows::core::Result<()> {
        unsafe { (Interface::vtable(&self.attributes).GetUnknown)(self.attributes.as_raw(), key, riid, object).ok() }
    }
    fn SetItem(&self, key: *const GUID, value: *const PROPVARIANT) -> windows::core::Result<()> {
        unsafe { self.attributes.SetItem(key, value) }
    }
    fn DeleteItem(&self, key: *const GUID) -> windows::core::Result<()> {
        unsafe { self.attributes.DeleteItem(key) }
    }
    fn DeleteAllItems(&self) -> windows::core::Result<()> {
        unsafe { self.attributes.DeleteAllItems() }
    }
    fn SetUINT32(&self, key: *const GUID, value: u32) -> windows::core::Result<()> {
        unsafe { self.attributes.SetUINT32(key, value) }
    }
    fn SetUINT64(&self, key: *const GUID, value: u64) -> windows::core::Result<()> {
        unsafe { self.attributes.SetUINT64(key, value) }
    }
    fn SetDouble(&self, key: *const GUID, value: f64) -> windows::core::Result<()> {
        unsafe { self.attributes.SetDouble(key, value) }
    }
    fn SetGUID(&self, key: *const GUID, value: *const GUID) -> windows::core::Result<()> {
        unsafe { self.attributes.SetGUID(key, value) }
    }
    fn SetString(&self, key: *const GUID, value: &PCWSTR) -> windows::core::Result<()> {
        unsafe { self.attributes.SetString(key, *value) }
    }
    fn SetBlob(&self, key: *const GUID, buffer: *const u8, size: u32) -> windows::core::Result<()> {
        let buffer = if buffer.is_null() { &[][..] } else { unsafe { std::slice::from_raw_parts(buffer, size as usize) } };
        unsafe { self.attributes.SetBlob(key, buffer) }
    }
    fn SetUnknown(&self, key: *const GUID, value: Ref<windows::core::IUnknown>) -> windows::core::Result<()> {
        unsafe { self.attributes.SetUnknown(key, value.as_ref()) }
    }
    fn LockStore(&self) -> windows::core::Result<()> {
        unsafe { self.attributes.LockStore() }
    }
    fn UnlockStore(&self) -> windows::core::Result<()> {
        unsafe { self.attributes.UnlockStore() }
    }
    fn GetCount(&self) -> windows::core::Result<u32> {
        unsafe { self.attributes.GetCount() }
    }
    fn GetItemByIndex(&self, index: u32, key: *mut GUID, value: *mut PROPVARIANT) -> windows::core::Result<()> {
        unsafe { self.attributes.GetItemByIndex(index, key, (!value.is_null()).then_some(value)) }
    }
    fn CopyAllItems(&self, destination: Ref<IMFAttributes>) -> windows::core::Result<()> {
        unsafe { self.attributes.CopyAllItems(destination.as_ref()) }
    }
}
