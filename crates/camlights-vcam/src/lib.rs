//! CamLights virtual camera: a Media Foundation custom media source loaded by the Windows
//! Camera Frame Server. It serves the frames the CamLights app publishes through
//! [`camlights_vcam_shared::FrameChannel`].

// MSVC suggests marking DllCanUnloadNow PRIVATE in the export table; COM loads it by name either way.
#![allow(linker_messages)]

mod activate;
mod log;
mod source;
mod stream;

use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};

use camlights_vcam_shared::{CLSID_STRING, CLSID_VIRTUAL_CAMERA};
use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_POINTER, ERROR_SUCCESS, HINSTANCE, HMODULE, S_FALSE, S_OK,
    WIN32_ERROR,
};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW,
    RegSetValueExW,
};
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::core::{BOOL, GUID, HRESULT, HSTRING, IUnknown, Interface, PCWSTR, Ref, implement};

static MODULE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

#[unsafe(no_mangle)]
extern "system" fn DllMain(instance: HINSTANCE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        MODULE.store(instance.0, Ordering::Relaxed);
    }
    true.into()
}

#[implement(IClassFactory)]
struct ClassFactory;

impl IClassFactory_Impl for ClassFactory_Impl {
    fn CreateInstance(&self, outer: Ref<IUnknown>, riid: *const GUID, object: *mut *mut c_void) -> windows::core::Result<()> {
        if object.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe { *object = std::ptr::null_mut() };
        if !outer.is_null() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let activate: IUnknown = activate::Activate::new()?.into();
        unsafe { activate.query(riid, object).ok() }
    }

    fn LockServer(&self, _lock: BOOL) -> windows::core::Result<()> {
        Ok(())
    }
}

#[unsafe(no_mangle)]
unsafe extern "system" fn DllGetClassObject(clsid: *const GUID, riid: *const GUID, object: *mut *mut c_void) -> HRESULT {
    if clsid.is_null() || riid.is_null() || object.is_null() {
        return E_POINTER;
    }
    unsafe {
        *object = std::ptr::null_mut();
        if *clsid != CLSID_VIRTUAL_CAMERA {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        let factory: IClassFactory = ClassFactory.into();
        factory.query(riid, object)
    }
}

#[unsafe(no_mangle)]
extern "system" fn DllCanUnloadNow() -> HRESULT {
    // The Frame Server keeps the source for the life of a session; staying loaded is simplest.
    S_FALSE
}

fn class_key() -> String {
    format!(r"Software\Classes\CLSID\{CLSID_STRING}")
}

fn module_path() -> windows::core::Result<String> {
    let mut buffer = [0u16; 1024];
    let module = HMODULE(MODULE.load(Ordering::Relaxed));
    let len = unsafe { GetModuleFileNameW(Some(module), &mut buffer) } as usize;
    if len == 0 || len == buffer.len() {
        return Err(windows::core::Error::from_thread());
    }
    Ok(String::from_utf16_lossy(&buffer[..len]))
}

fn set_string(key: HKEY, name: Option<&str>, value: &str) -> windows::core::Result<()> {
    let wide: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
    let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2) };
    let name = name.map(HSTRING::from);
    let name_ptr = name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr()));
    unsafe { RegSetValueExW(key, name_ptr, None, REG_SZ, Some(bytes)).ok() }
}

fn create_key(path: &str) -> windows::core::Result<HKEY> {
    let mut key = HKEY::default();
    unsafe {
        RegCreateKeyExW(HKEY_LOCAL_MACHINE, &HSTRING::from(path), None, None, REG_OPTION_NON_VOLATILE, KEY_WRITE, None, &mut key, None)
            .ok()?;
    }
    Ok(key)
}

fn register() -> windows::core::Result<()> {
    let path = module_path()?;
    let class = create_key(&class_key())?;
    let result = set_string(class, None, "CamLights Virtual Camera");
    unsafe {
        let _ = RegCloseKey(class);
    }
    result?;
    let server = create_key(&format!(r"{}\InprocServer32", class_key()))?;
    let result = set_string(server, None, &path).and_then(|_| set_string(server, Some("ThreadingModel"), "Both"));
    unsafe {
        let _ = RegCloseKey(server);
    }
    result
}

#[unsafe(no_mangle)]
extern "system" fn DllRegisterServer() -> HRESULT {
    match register() {
        Ok(()) => S_OK,
        Err(e) => e.code(),
    }
}

#[unsafe(no_mangle)]
extern "system" fn DllUnregisterServer() -> HRESULT {
    let status = unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, &HSTRING::from(class_key())) };
    if status == ERROR_SUCCESS || status == WIN32_ERROR(2) { S_OK } else { status.to_hresult() }
}
