//! Hand clip files to other apps: OLE drag-and-drop and "copy as file" (CF_HDROP via
//! the shell's own data object), so Discord, Explorer and chat apps receive real files.
use std::path::Path;

use windows::{
    Win32::{
        Foundation::{DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, HWND, S_OK},
        System::{
            Com::{IBindCtx, IDataObject},
            Ole::{
                DROPEFFECT, DROPEFFECT_COPY, DoDragDrop, IDropSource, IDropSource_Impl, OleFlushClipboard, OleInitialize, OleSetClipboard,
                RevokeDragDrop,
            },
            SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS},
        },
        UI::Shell::{BHID_DataObject, Common::ITEMIDLIST, ILCreateFromPathW, ILFree, IShellItemArray, SHCreateShellItemArrayFromIDLists},
    },
    core::{BOOL, HRESULT, HSTRING, implement},
};

/// OLE must be initialised on the UI thread before drag/drop or the OLE clipboard.
pub fn init_ole() {
    // S_FALSE (already initialised) is fine; a failure surfaces later as an error.
    let _ = unsafe { OleInitialize(None) };
}

/// Recall's window takes no file drops. GPUI 0.2.2's drop target dereferences the CF_HDROP
/// HGLOBAL; a drag that stays in-process (a clip dragged over Recall itself) hands it the
/// shell data object's fixed block, so DragQueryFileW read 0x14 and Recall crashed
/// (2026-10-30). Revoked, Recall's window is a plain "no drop" area during a drag.
pub fn refuse_drops(hwnd: isize) {
    // DRAGDROP_E_NOTREGISTERED (nothing to revoke) is fine.
    let _ = unsafe { RevokeDragDrop(HWND(hwnd as *mut _)) };
}

fn data_object(paths: &[String]) -> Result<IDataObject, String> {
    if paths.is_empty() {
        return Err("no clips selected".into());
    }
    let mut pidls: Vec<*mut ITEMIDLIST> = Vec::with_capacity(paths.len());
    for path in paths {
        if !Path::new(path).is_file() {
            free_all(&pidls);
            return Err(format!("missing file: {path}"));
        }
        let pidl = unsafe { ILCreateFromPathW(&HSTRING::from(path.as_str())) };
        if pidl.is_null() {
            free_all(&pidls);
            return Err(format!("could not reference {path}"));
        }
        pidls.push(pidl);
    }
    let constant: Vec<*const ITEMIDLIST> = pidls.iter().map(|p| *p as *const _).collect();
    let result = unsafe { SHCreateShellItemArrayFromIDLists(&constant) }
        .and_then(|items: IShellItemArray| unsafe { items.BindToHandler::<_, IDataObject>(None::<&IBindCtx>, &BHID_DataObject) });
    free_all(&pidls);
    result.map_err(|error| format!("could not prepare files: {error}"))
}

fn free_all(pidls: &[*mut ITEMIDLIST]) {
    for pidl in pidls {
        unsafe { ILFree(Some(*pidl as *const _)) };
    }
}

/// Puts the files on the clipboard as files (paste into Discord, Explorer, chats).
pub fn copy_files(paths: &[String]) -> Result<(), String> {
    let data = data_object(paths)?;
    unsafe { OleSetClipboard(&data) }.map_err(|error| format!("clipboard busy: {error}"))?;
    // Keep the data available after Recall exits.
    let _ = unsafe { OleFlushClipboard() };
    Ok(())
}

#[implement(IDropSource)]
struct DropSource;

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        if escape.as_bool() {
            DRAGDROP_S_CANCEL
        } else if keys.0 & MK_LBUTTON.0 == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// Runs a modal OLE drag of the files. Returns true when another app accepted the drop.
/// Call it while no GPUI state is borrowed (from a spawned task, outside `update`).
pub fn drag_files(paths: &[String]) -> Result<bool, String> {
    let data = data_object(paths)?;
    let source: IDropSource = DropSource.into();
    let mut effect = DROPEFFECT(0);
    let hr = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect) };
    Ok(hr == DRAGDROP_S_DROP && effect.0 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::{
        System::Com::{DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL},
        System::Ole::{CF_HDROP, ReleaseStgMedium},
        UI::Shell::{DragQueryFileW, HDROP},
    };

    #[test]
    fn data_object_carries_the_files_as_hdrop() {
        init_ole();
        let dir = std::env::temp_dir().join(format!("recall-shell-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("clip a.mp4");
        let b = dir.join("clip-b.mp4");
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&b, b"y").unwrap();
        let paths = vec![a.to_string_lossy().into_owned(), b.to_string_lossy().into_owned()];
        let data = data_object(&paths).expect("data object");
        let format = FORMATETC { cfFormat: CF_HDROP.0, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
        let mut medium = unsafe { data.GetData(&format) }.expect("CF_HDROP");
        let drop = HDROP(unsafe { medium.u.hGlobal.0 });
        let count = unsafe { DragQueryFileW(drop, u32::MAX, None) };
        let mut names = Vec::new();
        for i in 0..count {
            let mut buf = [0u16; 512];
            let n = unsafe { DragQueryFileW(drop, i, Some(&mut buf)) } as usize;
            names.push(String::from_utf16_lossy(&buf[..n]));
        }
        unsafe { ReleaseStgMedium(&mut medium) };
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(count, 2);
        assert!(names.iter().any(|n| n.ends_with("clip a.mp4")), "{names:?}");
        assert!(data_object(&[r"Z:\definitely\missing.mp4".into()]).is_err());
    }
}
