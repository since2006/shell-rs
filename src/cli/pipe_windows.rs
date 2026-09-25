//! The Win32 calls behind the CLI's named pipe. The pipe's name belongs to
//! the whole machine rather than to a folder of the user's, so both ends
//! make sure of each other: the app's pipe lets only the same user in, and
//! the command talks only to a pipe that user (or an administrator) owns.

use std::{
    ffi::OsStr,
    fs::{File, OpenOptions},
    io,
    os::windows::{
        ffi::OsStrExt as _,
        fs::OpenOptionsExt as _,
        io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle},
    },
    path::Path,
    ptr::null_mut,
};

use windows_sys::Win32::{
    Foundation::{
        ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_SUCCESS,
        INVALID_HANDLE_VALUE, LocalFree,
    },
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SDDL_REVISION_1, SE_KERNEL_OBJECT,
        },
        EqualSid, GetTokenInformation, IsWellKnownSid, OWNER_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
        WinBuiltinAdministratorsSid,
    },
    Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX, SECURITY_IDENTIFICATION,
    },
    System::{
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, WaitNamedPipeW,
        },
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
};

/// Each direction's buffer. Output larger than this simply waits for the
/// command to read it.
const BUFFER: u32 = 64 * 1024;
/// How many times the command waits for a free instance before giving up.
const BUSY_RETRIES: u32 = 5;
/// How long each of those waits may take, in milliseconds.
const BUSY_WAIT: u32 = 1000;

/// A new instance of the app's pipe, waiting for a caller. The first one
/// fails if the name is already taken, by another ShellRS or anyone else.
pub(super) fn create_instance(name: &Path, first: bool) -> io::Result<OwnedHandle> {
    let descriptor = SecurityDescriptor::current_user_only()?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let name = wide(name.as_os_str());
    let mut open_mode = PIPE_ACCESS_DUPLEX;
    if first {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    // SAFETY: `name` is NUL-terminated and `attributes` points at a
    // descriptor that outlives the call.
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            BUFFER,
            BUFFER,
            0,
            &attributes,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a valid handle nobody else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

/// Whether creating the first instance failed because the name is taken.
pub(super) fn is_taken(error: &io::Error) -> bool {
    error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32)
}

/// Block until a caller connects to `pipe`. An error means the caller
/// came and went already; the instance is spent either way.
pub(super) fn wait_for_client(pipe: &OwnedHandle) -> io::Result<()> {
    // SAFETY: a valid pipe handle, opened for synchronous use.
    if unsafe { ConnectNamedPipe(pipe.as_raw_handle(), null_mut()) } != 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    // Connected between creating the instance and waiting on it.
    if error.raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32) {
        return Ok(());
    }
    Err(error)
}

/// The command's end: open the app's pipe, waiting briefly while every
/// instance is busy. Identification only, so a program that took the name
/// cannot act as the caller.
pub(super) fn open(name: &Path) -> io::Result<File> {
    let mut waits = 0;
    loop {
        match OpenOptions::new()
            .read(true)
            .write(true)
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(name)
        {
            Err(error)
                if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) && waits < BUSY_RETRIES =>
            {
                waits += 1;
                // SAFETY: a NUL-terminated name.
                unsafe { WaitNamedPipeW(wide(name.as_os_str()).as_ptr(), BUSY_WAIT) };
            }
            result => return result,
        }
    }
}

/// Whether the pipe at the other end belongs to the current user, or to
/// the administrators (whom an elevated ShellRS creates it as). Nobody
/// else can make either of them a pipe's owner.
pub(super) fn owned_by_current_user(pipe: &File) -> io::Result<bool> {
    let mut owner: PSID = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `owner` points into `descriptor`, which is freed after the
    // last use of `owner` below.
    let status = unsafe {
        GetSecurityInfo(
            pipe.as_raw_handle(),
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let descriptor = SecurityDescriptor(descriptor);
    let user = CurrentUser::read()?;
    // SAFETY: both SIDs are valid for as long as `descriptor` and `user`.
    let owned = unsafe {
        EqualSid(owner, user.sid()) != 0 || IsWellKnownSid(owner, WinBuiltinAdministratorsSid) != 0
    };
    drop(descriptor);
    Ok(owned)
}

/// The user this process runs as.
struct CurrentUser {
    /// A `TOKEN_USER` and the SID it points into; `usize`s so the struct is
    /// aligned.
    buffer: Vec<usize>,
}

impl CurrentUser {
    fn read() -> io::Result<Self> {
        let mut token = null_mut();
        // SAFETY: the current process's pseudo handle needs no closing; the
        // token handle is owned right away.
        let token = unsafe {
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(io::Error::last_os_error());
            }
            OwnedHandle::from_raw_handle(token)
        };
        let mut length = 0;
        // SAFETY: asking for the size only; this call fails by design.
        unsafe {
            GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut length)
        };
        let mut buffer = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
        // SAFETY: `buffer` holds at least `length` bytes.
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                buffer.as_mut_ptr().cast(),
                length,
                &mut length,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { buffer })
    }

    fn sid(&self) -> PSID {
        // SAFETY: `buffer` holds the `TOKEN_USER` that `read` asked for.
        unsafe { (*self.buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }

    /// The SID as text, such as `S-1-5-21-…`.
    fn sid_string(&self) -> io::Result<String> {
        let mut text = null_mut();
        // SAFETY: a valid SID; the string is freed with LocalFree.
        unsafe {
            if ConvertSidToStringSidW(self.sid(), &mut text) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut length = 0;
            while *text.add(length) != 0 {
                length += 1;
            }
            let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
            LocalFree(text.cast());
            Ok(sid)
        }
    }
}

/// A security descriptor Windows allocated, freed when dropped.
struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl SecurityDescriptor {
    /// Full access for the current user and nobody else, not inherited
    /// from anywhere.
    fn current_user_only() -> io::Result<Self> {
        let sddl = format!("D:P(A;;GA;;;{})", CurrentUser::read()?.sid_string()?);
        let mut descriptor = null_mut();
        // SAFETY: a NUL-terminated string; the result is freed on drop.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide(OsStr::new(&sddl)).as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by Windows with LocalAlloc.
        unsafe { LocalFree(self.0) };
    }
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(std::iter::once(0)).collect()
}
