//! The Windows calls installing needs: the per-user uninstall entry in the registry, the Start
//! menu shortcut, and message boxes for the uninstaller (which runs with no window of its own).
//! Other platforms get stubs that fail, so the rest of the app still builds there.

use std::path::Path;

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub enum Value<'a> {
    Text(&'a str),
    Number(u32),
}

/// Creates `HKCU\<key>` and sets `values` on it.
#[cfg(windows)]
pub fn reg_write(key: &str, values: &[(&str, Value)]) -> Result<(), String> {
    use windows_sys::Win32::System::Registry::*;
    let mut hkey: HKEY = std::ptr::null_mut();
    let key_w = wide(key);
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key_w.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            std::ptr::null(),
            &mut hkey,
            std::ptr::null_mut(),
        )
    };
    if rc != 0 {
        return Err(format!("couldn't write the uninstall entry ({})", std::io::Error::from_raw_os_error(rc as i32)));
    }
    let mut result = Ok(());
    for (name, value) in values {
        let name_w = wide(name);
        let rc = unsafe {
            match value {
                Value::Text(t) => {
                    let data = wide(t);
                    RegSetValueExW(hkey, name_w.as_ptr(), 0, REG_SZ, data.as_ptr() as *const u8, (data.len() * 2) as u32)
                }
                Value::Number(n) => RegSetValueExW(hkey, name_w.as_ptr(), 0, REG_DWORD, n as *const u32 as *const u8, 4),
            }
        };
        if rc != 0 && result.is_ok() {
            result = Err(format!("couldn't write {name} ({})", std::io::Error::from_raw_os_error(rc as i32)));
        }
    }
    unsafe { RegCloseKey(hkey) };
    result
}

/// A text value of `HKCU\<key>`.
#[cfg(windows)]
pub fn reg_read(key: &str, name: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::*;
    let (key_w, name_w) = (wide(key), wide(name));
    let mut buf = vec![0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key_w.as_ptr(),
            name_w.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr() as *mut core::ffi::c_void,
            &mut len,
        )
    };
    if rc != 0 {
        return None;
    }
    let chars = (len as usize / 2).min(buf.len());
    let s = String::from_utf16_lossy(&buf[..chars]);
    Some(s.trim_end_matches('\0').to_string())
}

#[cfg(windows)]
pub fn reg_delete(key: &str) {
    use windows_sys::Win32::System::Registry::*;
    let key_w = wide(key);
    unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, key_w.as_ptr()) };
    // RegDeleteTreeW empties the key; the key itself goes with a second call.
    unsafe { windows_sys::Win32::System::Registry::RegDeleteKeyW(HKEY_CURRENT_USER, key_w.as_ptr()) };
}

/// Writes a `.lnk` at `lnk` that starts `exe` in its own folder, with the exe's icon.
#[cfg(windows)]
pub fn shortcut(exe: &Path, lnk: &Path) -> Result<(), String> {
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    use windows::core::{Interface, HSTRING};
    let dir = exe.parent().ok_or("the exe has no folder")?;
    if let Some(parent) = lnk.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("couldn't create the Start menu folder ({e})"))?;
    }
    unsafe {
        // S_FALSE (already initialised on this thread) still needs the matching uninit.
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let made = (|| -> windows::core::Result<()> {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            link.SetPath(&HSTRING::from(exe.as_os_str()))?;
            link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
            link.SetDescription(&HSTRING::from("Projects"))?;
            link.SetIconLocation(&HSTRING::from(exe.as_os_str()), 0)?;
            link.cast::<IPersistFile>()?.Save(&HSTRING::from(lnk.as_os_str()), true)
        })();
        if init.is_ok() {
            CoUninitialize();
        }
        made.map_err(|e| format!("couldn't create the Start menu shortcut ({})", e.message()))
    }
}

/// A Yes/No question; true for Yes.
#[cfg(windows)]
pub fn ask(title: &str, text: &str) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    let (t, b) = (wide(title), wide(text));
    unsafe { MessageBoxW(std::ptr::null_mut(), b.as_ptr(), t.as_ptr(), MB_YESNO | MB_ICONQUESTION | MB_TOPMOST) == IDYES }
}

#[cfg(windows)]
pub fn tell(title: &str, text: &str, error: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    let (t, b) = (wide(title), wide(text));
    let icon = if error { MB_ICONERROR } else { MB_ICONINFORMATION };
    unsafe { MessageBoxW(std::ptr::null_mut(), b.as_ptr(), t.as_ptr(), MB_OK | icon | MB_TOPMOST) };
}

/// Release builds are GUI programs with no console; a headless run started from a terminal
/// (`--snap`, `--tree`) gets that terminal back for its output.
#[cfg(windows)]
pub fn attach_parent_console() {
    unsafe { windows_sys::Win32::System::Console::AttachConsole(windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS) };
}

/// Waits up to `ms` for process `pid` to exit; true once it has. An unknown pid counts as exited.
#[cfg(windows)]
pub fn wait_exit(pid: u32, ms: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
    unsafe {
        let h = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if h.is_null() {
            return true;
        }
        let done = WaitForSingleObject(h, ms) == WAIT_OBJECT_0;
        CloseHandle(h);
        done
    }
}

#[cfg(not(windows))]
mod stubs {
    use super::*;
    pub fn reg_write(_: &str, _: &[(&str, Value)]) -> Result<(), String> {
        Err("installing is Windows-only".into())
    }
    pub fn reg_read(_: &str, _: &str) -> Option<String> {
        None
    }
    pub fn reg_delete(_: &str) {}
    pub fn shortcut(_: &Path, _: &Path) -> Result<(), String> {
        Err("installing is Windows-only".into())
    }
    pub fn ask(_: &str, _: &str) -> bool {
        false
    }
    pub fn tell(_: &str, _: &str, _: bool) {}
    pub fn attach_parent_console() {}
    pub fn wait_exit(_: u32, _: u32) -> bool {
        true
    }
}
#[cfg(not(windows))]
pub use stubs::*;
