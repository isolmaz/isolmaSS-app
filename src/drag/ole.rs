//! The OLE side of a drag from the upload card: the data object that serves the payload to
//! the drop target and the drop source that ends the drag when the button is released or
//! Esc is pressed. `DoDragDrop` runs a modal loop driven by a real mouse, so only a desktop
//! session exercises it; what is offered is decided and tested in the parent module.
use super::Payload;
use std::mem::ManuallyDrop;
use windows::Win32::Foundation::{
    BOOL, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, DV_E_DVASPECT,
    DV_E_FORMATETC, DV_E_TYMED, E_INVALIDARG, E_NOTIMPL, GlobalFree, HGLOBAL,
    OLE_E_ADVISENOTSUPPORTED, S_OK,
};
use windows::Win32::System::Com::{
    DATADIR_GET, DVASPECT_CONTENT, FORMATETC, IAdviseSink, IDataObject, IDataObject_Impl,
    IEnumFORMATETC, IEnumSTATDATA, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{
    DROPEFFECT, DROPEFFECT_COPY, DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize,
    OleUninitialize,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Shell::SHCreateStdEnumFmtEtc;
use windows::core::{Error, HRESULT, Result, implement};

/// A new movable global-memory block holding `bytes`. The drop target owns and frees it.
fn global_copy(bytes: &[u8]) -> Result<HGLOBAL> {
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len())? };
    let pointer = unsafe { GlobalLock(memory) };
    if pointer.is_null() {
        let error = Error::from_win32();
        unsafe {
            let _ = GlobalFree(memory);
        }
        return Err(error);
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer.cast::<u8>(), bytes.len());
        let _ = GlobalUnlock(memory);
    }
    Ok(memory)
}

#[implement(IDataObject)]
struct DataObject {
    payload: Payload,
    formats: Vec<FORMATETC>,
}

impl DataObject {
    fn new(payload: Payload) -> Self {
        let formats = payload
            .formats()
            .into_iter()
            .map(|format| FORMATETC {
                cfFormat: format,
                ptd: std::ptr::null_mut(),
                dwAspect: DVASPECT_CONTENT.0,
                lindex: -1,
                tymed: TYMED_HGLOBAL.0 as u32,
            })
            .collect();
        Self { payload, formats }
    }

    /// The bytes `format` asks for, or the error code that tells why they are not served.
    fn content(&self, format: *const FORMATETC) -> std::result::Result<&[u8], HRESULT> {
        let format = unsafe { format.as_ref() }.ok_or(E_INVALIDARG)?;
        let bytes = self.payload.data(format.cfFormat).ok_or(DV_E_FORMATETC)?;
        if format.dwAspect != DVASPECT_CONTENT.0 {
            return Err(DV_E_DVASPECT);
        }
        if format.tymed & TYMED_HGLOBAL.0 as u32 == 0 {
            return Err(DV_E_TYMED);
        }
        Ok(bytes)
    }
}

impl IDataObject_Impl for DataObject_Impl {
    fn GetData(&self, pformatetcin: *const FORMATETC) -> Result<STGMEDIUM> {
        let bytes = self.content(pformatetcin).map_err(Error::from)?;
        Ok(STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 {
                hGlobal: global_copy(bytes)?,
            },
            pUnkForRelease: ManuallyDrop::new(None),
        })
    }

    fn GetDataHere(&self, _format: *const FORMATETC, _medium: *mut STGMEDIUM) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
        match self.content(pformatetc) {
            Ok(_) => S_OK,
            Err(code) => code,
        }
    }

    fn GetCanonicalFormatEtc(
        &self,
        _format: *const FORMATETC,
        _canonical: *mut FORMATETC,
    ) -> HRESULT {
        E_NOTIMPL
    }

    fn SetData(
        &self,
        _format: *const FORMATETC,
        _medium: *const STGMEDIUM,
        _release: BOOL,
    ) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn EnumFormatEtc(&self, dwdirection: u32) -> Result<IEnumFORMATETC> {
        if dwdirection == DATADIR_GET.0 as u32 {
            unsafe { SHCreateStdEnumFmtEtc(&self.formats) }
        } else {
            Err(E_NOTIMPL.into())
        }
    }

    fn DAdvise(
        &self,
        _format: *const FORMATETC,
        _flags: u32,
        _sink: Option<&IAdviseSink>,
    ) -> Result<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _connection: u32) -> Result<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> Result<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}

/// Ends the drag with a drop when the left button is released and cancels it on Esc.
#[implement(IDropSource)]
struct DropSource;

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, fescapepressed: BOOL, grfkeystate: MODIFIERKEYS_FLAGS) -> HRESULT {
        if fescapepressed.as_bool() {
            DRAGDROP_S_CANCEL
        } else if grfkeystate.0 & MK_LBUTTON.0 == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// Keeps OLE initialized on the calling thread for the length of a drag.
struct OleSession;

impl Drop for OleSession {
    fn drop(&mut self) {
        unsafe { OleUninitialize() };
    }
}

/// Drags `payload` out of the window the left button is held on and returns when the
/// button is released or Esc is pressed. True when a drop target accepted the copy.
pub fn run(payload: Payload) -> Result<bool> {
    unsafe { OleInitialize(None)? };
    let _session = OleSession;
    let data: IDataObject = DataObject::new(payload).into();
    let source: IDropSource = DropSource.into();
    let mut effect = DROPEFFECT(0);
    let outcome = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect) };
    outcome.ok()?;
    Ok(super::dropped(outcome, effect.0))
}
