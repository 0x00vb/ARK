//! Windows: an AppContainer. A process in one reaches only what grants access to the
//! container's SID (or to every app package: the system folders do), and without the
//! `internetClient` capabilities it has no network. ARK gives the container's SID access to the
//! project (read and write, except its kept folders) and the run's folders, then starts the
//! program inside the container.
//!
//! Starting a process in an AppContainer takes `CreateProcessW` with a security-capabilities
//! attribute, which `std::process::Command` cannot pass. So the run is started through this
//! program itself: [`wrap`] names the current program with [`FLAG`], and [`entry`] (called
//! first in `main`) sees the flag, starts the run in the container with the same standard
//! streams, and exits with its exit code. A job object stops the run when the launcher is
//! stopped (a timeout or a cancelled turn).
//!
//! Granting access writes an access entry for the container's SID on each folder, inherited
//! by what is inside. It stays: the next run in the same project finds it and skips the step.
//! A folder whose file system has no access lists (FAT, some network drives) cannot be granted
//! and the run fails with the reason.

use std::ffi::{c_void, OsStr, OsString};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::{
    CloseHandle, LocalFree, SetHandleInformation, ERROR_SUCCESS, HANDLE, HANDLE_FLAG_INHERIT,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    GetNamedSecurityInfoW, SetEntriesInAclW, SetNamedSecurityInfoW, DENY_ACCESS, EXPLICIT_ACCESS_W,
    GRANT_ACCESS, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN,
    TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows_sys::Win32::Security::{
    EqualSid, FreeSid, GetAce, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION,
    INHERITED_ACE, PSECURITY_DESCRIPTOR, PSID, SECURITY_CAPABILITIES,
    SUB_CONTAINERS_AND_OBJECTS_INHERIT,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_APPEND_DATA, FILE_DELETE_CHILD, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ,
    FILE_GENERIC_WRITE, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FILE_WRITE_EA, WRITE_DAC,
    WRITE_OWNER,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, ResumeThread, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, INFINITE, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
    STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

use super::{real, Bounds, KEPT};

/// The argument that makes this program the launcher of a contained run.
const FLAG: &str = "--ark-contained-run";
/// The AppContainer every run uses. Its SID follows from the name, so access granted once
/// holds for later runs.
const CONTAINER: &str = "ark.run";
/// Exit code of the launcher when it could not start the run.
const NOT_STARTED: i32 = 125;

/// Read, write and delete what is inside, but not remove a folder's entries by the folder's
/// right (`FILE_DELETE_CHILD`): a kept folder denies `DELETE` on itself, and that must hold.
const WRITE: u32 = FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE;
const READ: u32 = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE;
/// Only the rights that change something; denying the read-control or synchronize rights that
/// `FILE_GENERIC_WRITE` also holds would stop reads too.
const NO_WRITE: u32 = FILE_WRITE_DATA
    | FILE_APPEND_DATA
    | FILE_WRITE_EA
    | FILE_WRITE_ATTRIBUTES
    | DELETE
    | FILE_DELETE_CHILD
    | WRITE_DAC
    | WRITE_OWNER;

/// `ACE_HEADER::AceType` of an allowing and a denying entry (winnt.h).
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const ACCESS_DENIED_ACE_TYPE: u8 = 1;

/// Set by [`entry`] when this program runs normally: it can launch contained runs.
static LAUNCHER: AtomicBool = AtomicBool::new(false);

pub(super) fn entry() {
    let mut args = std::env::args_os();
    let _program = args.next();
    if args.next().as_deref() != Some(OsStr::new(FLAG)) {
        LAUNCHER.store(true, Ordering::SeqCst);
        return;
    }
    let rest: Vec<OsString> = args.collect();
    let code = match launch(&rest) {
        Ok(code) => code as i32,
        Err(e) => {
            eprintln!("ARK could not start the contained run: {e}");
            NOT_STARTED
        }
    };
    std::process::exit(code);
}

pub(super) fn find() -> Option<PathBuf> {
    if !LAUNCHER.load(Ordering::SeqCst) {
        tracing::info!("Run code is not offered: the program did not call ark_exec::entry()");
        return None;
    }
    std::env::current_exe().ok()
}

pub(super) fn wrap(
    exec: &Path,
    b: &Bounds,
    program: &Path,
    args: &[String],
) -> Result<(PathBuf, Vec<String>), String> {
    let sid = Sid::container()?;
    let project = real(&b.project);
    grant(&real(&b.scratch), &sid, WRITE, false)?;
    grant(&project, &sid, WRITE, false)?;
    for r in b.only_reads() {
        let r = real(r);
        if r.exists() {
            grant(&r, &sid, READ, false)?;
        }
    }
    for k in KEPT {
        let p = project.join(k);
        if p.exists() {
            grant(&p, &sid, NO_WRITE, true)?;
        }
    }
    let mut out = vec![
        FLAG.to_string(),
        project.display().to_string(),
        "--".into(),
        program.display().to_string(),
    ];
    out.extend(args.iter().cloned());
    Ok((exec.to_path_buf(), out))
}

/// The container's SID.
struct Sid(PSID);

impl Sid {
    fn container() -> Result<Sid, String> {
        let name = wide(OsStr::new(CONTAINER));
        let shown = wide(OsStr::new("ARK Run code"));
        let mut sid: PSID = null_mut();
        // SAFETY: the strings end with a nul; `sid` receives a SID freed with FreeSid.
        let made = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                shown.as_ptr(),
                shown.as_ptr(),
                null(),
                0,
                &mut sid,
            )
        };
        if made >= 0 {
            return Ok(Sid(sid));
        }
        // It exists already (made by an earlier run).
        // SAFETY: as above.
        let derived = unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) };
        if derived >= 0 {
            Ok(Sid(sid))
        } else {
            Err(format!(
                "no AppContainer profile (0x{:08x}, 0x{:08x})",
                made as u32, derived as u32
            ))
        }
    }
}

impl Drop for Sid {
    fn drop(&mut self) {
        // SAFETY: the SID came from the AppContainer functions, which say to free it so.
        unsafe {
            FreeSid(self.0);
        }
    }
}

/// Frees memory Windows allocated for a result (`LocalFree`).
struct Local(*mut c_void);

impl Drop for Local {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from a function documented to need LocalFree.
            unsafe {
                LocalFree(self.0);
            }
        }
    }
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

/// Add an entry for `sid` to `path`'s access list (allow, or `deny`, `mask`), inherited by
/// everything inside. Nothing is written when the entry is there already.
fn grant(path: &Path, sid: &Sid, mask: u32, deny: bool) -> Result<(), String> {
    let name = wide(path.as_os_str());
    let mut dacl: *mut ACL = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `name` ends with a nul; the security descriptor is freed by `Local`, and `dacl`
    // points into it.
    let e = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut sd,
        )
    };
    let _sd = Local(sd);
    if e != ERROR_SUCCESS {
        return Err(format!(
            "reading the access list of {} failed (error {e})",
            path.display()
        ));
    }
    // SAFETY: `dacl` is the list just read, or null.
    if unsafe { has(dacl, sid.0, mask, deny) } {
        return Ok(());
    }
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: mask,
        grfAccessMode: if deny { DENY_ACCESS } else { GRANT_ACCESS },
        grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid.0 as *mut u16,
        },
    };
    let mut merged: *mut ACL = null_mut();
    // SAFETY: one entry, the old list, and an out pointer freed by `Local`.
    let e = unsafe { SetEntriesInAclW(1, &entry, dacl, &mut merged) };
    let _merged = Local(merged as *mut c_void);
    if e != ERROR_SUCCESS {
        return Err(format!(
            "making the access list of {} failed (error {e})",
            path.display()
        ));
    }
    // SAFETY: `name` ends with a nul and `merged` is a valid list. Windows passes the
    // inherited entry on to what is inside the folder.
    let e = unsafe {
        SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            merged,
            null(),
        )
    };
    if e != ERROR_SUCCESS {
        return Err(format!(
            "giving the run access to {} failed (error {e}); its drive may not keep access lists",
            path.display()
        ));
    }
    Ok(())
}

/// Whether `acl` has an entry of its own (not inherited) for `sid`, of the kind and with at
/// least `mask`.
///
/// # Safety
/// `acl` is null or a valid access list.
unsafe fn has(acl: *const ACL, sid: PSID, mask: u32, deny: bool) -> bool {
    if acl.is_null() {
        return false;
    }
    let want = if deny {
        ACCESS_DENIED_ACE_TYPE
    } else {
        ACCESS_ALLOWED_ACE_TYPE
    };
    for i in 0..u32::from((*acl).AceCount) {
        let mut ace: *mut c_void = null_mut();
        if GetAce(acl, i, &mut ace) == 0 || ace.is_null() {
            continue;
        }
        let header = &*(ace as *const ACE_HEADER);
        if header.AceType != want || header.AceFlags & INHERITED_ACE as u8 != 0 {
            continue;
        }
        // Allowed and denied entries have the same layout.
        let entry = &*(ace as *const ACCESS_ALLOWED_ACE);
        let entry_sid = &entry.SidStart as *const u32 as PSID;
        if EqualSid(entry_sid, sid) != 0 && entry.Mask & mask == mask {
            return true;
        }
    }
    false
}

/// In the launcher: start `cwd -- program args` in the container with this process's standard
/// streams, wait, and return its exit code.
fn launch(rest: &[OsString]) -> Result<u32, String> {
    let (cwd, command) = match rest {
        [cwd, sep, command @ ..] if sep == "--" && !command.is_empty() => (cwd, command),
        _ => return Err("expected: <folder> -- <program> [args]".into()),
    };
    let sid = Sid::container()?;
    let mut caps = SECURITY_CAPABILITIES {
        AppContainerSid: sid.0,
        Capabilities: null_mut(),
        CapabilityCount: 0,
        Reserved: 0,
    };

    // The run gets this process's standard streams and no other handle.
    let mut handles: Vec<HANDLE> = Vec::new();
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: GetStdHandle only reads the process's standard handles.
        let h = unsafe { GetStdHandle(which) };
        if !h.is_null() && h != INVALID_HANDLE_VALUE && !handles.contains(&h) {
            // SAFETY: `h` is a handle of this process.
            unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
            handles.push(h);
        }
    }

    let mut size = 0usize;
    // SAFETY: the first call only reports the size needed.
    unsafe { InitializeProcThreadAttributeList(null_mut(), 2, 0, &mut size) };
    let mut buf = vec![0u8; size];
    let list = buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
    // SAFETY: `buf` has the size the first call asked for and outlives the list's use.
    if unsafe { InitializeProcThreadAttributeList(list, 2, 0, &mut size) } == 0 {
        return Err(last_error("InitializeProcThreadAttributeList"));
    }
    let attributes = Attributes(list);
    // SAFETY: `caps` and `handles` outlive CreateProcessW below.
    unsafe {
        if UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
            &mut caps as *mut _ as *const c_void,
            std::mem::size_of::<SECURITY_CAPABILITIES>(),
            null_mut(),
            null(),
        ) == 0
        {
            return Err(last_error("the AppContainer attribute"));
        }
        if !handles.is_empty()
            && UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr() as *const c_void,
                handles.len() * std::mem::size_of::<HANDLE>(),
                null_mut(),
                null(),
            ) == 0
        {
            return Err(last_error("the handle list attribute"));
        }
    }

    let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    // SAFETY: as above.
    unsafe {
        si.StartupInfo.hStdInput = GetStdHandle(STD_INPUT_HANDLE);
        si.StartupInfo.hStdOutput = GetStdHandle(STD_OUTPUT_HANDLE);
        si.StartupInfo.hStdError = GetStdHandle(STD_ERROR_HANDLE);
    }
    si.lpAttributeList = list;

    let mut line = command_line(command);
    let dir = wide(cwd);
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: every pointer is valid for the call; `line` is mutable as CreateProcessW wants.
    // The environment is this process's, which ARK set for the run.
    let ok = unsafe {
        CreateProcessW(
            null(),
            line.as_mut_ptr(),
            null(),
            null(),
            1,
            EXTENDED_STARTUPINFO_PRESENT
                | CREATE_SUSPENDED
                | CREATE_NO_WINDOW
                | CREATE_UNICODE_ENVIRONMENT,
            null(),
            dir.as_ptr(),
            &si.StartupInfo,
            &mut pi,
        )
    };
    drop(attributes);
    if ok == 0 {
        return Err(last_error("CreateProcessW"));
    }
    let process = Handle(pi.hProcess);
    let thread = Handle(pi.hThread);

    // The run and everything it starts end when this launcher ends: the job closes with it.
    // SAFETY: plain calls on handles this function owns.
    let job = Handle(unsafe { CreateJobObjectW(null(), null()) });
    if job.0.is_null() {
        let error = last_error("CreateJobObjectW");
        unsafe {
            TerminateProcess(process.0, 1);
            WaitForSingleObject(process.0, INFINITE);
        }
        return Err(error);
    }
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    unsafe {
        if SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) == 0
            || AssignProcessToJobObject(job.0, process.0) == 0
        {
            let error = last_error("the job object");
            TerminateProcess(process.0, 1);
            WaitForSingleObject(process.0, INFINITE);
            return Err(error);
        }
        if ResumeThread(thread.0) == u32::MAX {
            let error = last_error("ResumeThread");
            TerminateProcess(process.0, 1);
            WaitForSingleObject(process.0, INFINITE);
            return Err(error);
        }
        WaitForSingleObject(process.0, INFINITE);
        let mut code = 0u32;
        GetExitCodeProcess(process.0, &mut code);
        Ok(code)
    }
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: a handle this module opened.
            unsafe { CloseHandle(self.0) };
        }
    }
}

struct Attributes(LPPROC_THREAD_ATTRIBUTE_LIST);

impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: an initialized list whose memory is still alive.
        unsafe { DeleteProcThreadAttributeList(self.0) };
    }
}

fn last_error(what: &str) -> String {
    format!("{what} failed: {}", std::io::Error::last_os_error())
}

/// A command line as `CommandLineToArgvW` and the C runtime split it, ending with a nul.
fn command_line(args: &[OsString]) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::new();
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            out.push(b' ' as u16);
        }
        quote(a, &mut out);
    }
    out.push(0);
    out
}

fn quote(arg: &OsStr, out: &mut Vec<u16>) {
    let a: Vec<u16> = arg.encode_wide().collect();
    let plain = !a.is_empty()
        && !a
            .iter()
            .any(|&c| c == b' ' as u16 || c == b'\t' as u16 || c == b'"' as u16);
    if plain {
        out.extend(a);
        return;
    }
    out.push(b'"' as u16);
    let mut slashes = 0usize;
    for c in a {
        if c == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        let n = if c == b'"' as u16 {
            slashes * 2 + 1
        } else {
            slashes
        };
        out.extend(std::iter::repeat_n(b'\\' as u16, n));
        out.push(c);
        slashes = 0;
    }
    out.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    out.push(b'"' as u16);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(args: &[&str]) -> String {
        let a: Vec<OsString> = args.iter().map(OsString::from).collect();
        let mut w = command_line(&a);
        w.pop();
        String::from_utf16(&w).unwrap()
    }

    #[test]
    fn arguments_are_quoted_the_way_windows_splits_them() {
        assert_eq!(line(&["C:\\py\\python.exe", "-B"]), "C:\\py\\python.exe -B");
        assert_eq!(
            line(&["C:\\Program Files\\x.exe", "a b", ""]),
            "\"C:\\Program Files\\x.exe\" \"a b\" \"\""
        );
        assert_eq!(line(&["say \"hi\""]), "\"say \\\"hi\\\"\"");
        assert_eq!(
            line(&["C:\\dir with space\\"]),
            "\"C:\\dir with space\\\\\""
        );
    }
}
