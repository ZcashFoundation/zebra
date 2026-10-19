//! Owns a Windows job for each test child and its normally created descendants.
//!
//! The job is unnamed and non-inheritable, with neither breakaway limit enabled. It is attached
//! while the initial thread is suspended, before any child code runs. This is process cleanup,
//! not a security boundary against external processes injecting threads or duplicating handles,
//! or children asking an unrelated broker/service to launch processes outside the job.

use std::{
    io,
    mem::{offset_of, size_of},
    os::windows::{
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    process::{Child, Command, ExitStatus},
};

use windows_sys::Win32::{
    Foundation::{ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_TIMEOUT},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        },
        Threading::{
            GetProcessIdOfThread, OpenThread, ResumeThread, WaitForSingleObject, CREATE_SUSPENDED,
            THREAD_QUERY_LIMITED_INFORMATION, THREAD_SUSPEND_RESUME,
        },
    },
};

/// Bounds our scan of the system-wide snapshot; larger snapshots fail closed before resumption.
const MAX_SNAPSHOT_THREADS: usize = 65_536;

/// A stable job handle, retained until cleanup finishes; closing it also kills remaining members.
#[derive(Debug)]
pub(super) struct ProcessGroup {
    job: OwnedHandle,
    terminated: bool,
}

impl ProcessGroup {
    /// Terminates the job without waiting on or changing the leader's cached exit status.
    pub(super) fn kill(&mut self, _child: &mut Child) -> io::Result<()> {
        if self.terminated {
            return Ok(());
        }

        // SAFETY: this live owned handle came from CreateJobObjectW with JOB_OBJECT_ALL_ACCESS.
        // It cannot be closed during this exclusive borrow and always refers to our original job.
        if unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        self.terminated = true;
        Ok(())
    }

    /// Preserves a natural leader exit, terminating any remaining members before reporting it.
    pub(super) fn try_wait(&mut self, child: &mut Child) -> io::Result<Option<ExitStatus>> {
        let status = child.try_wait()?;
        if status.is_some() {
            self.kill(child)?;
        }
        Ok(status)
    }
}

/// Spawns through std to retain argument, environment, directory, and stdio semantics.
///
/// Callers must leave Windows creation flags at their default (zero). Stable Command exposes no
/// getter for existing flags: we replace them with CREATE_SUSPENDED and restore zero after spawn.
/// Job assignment restrictions imposed by an enclosing job cause a spawn error, not an unowned
/// running child. All native handles except the process handle remain private to this backend.
pub(super) fn spawn(command: &mut Command) -> io::Result<(Child, ProcessGroup)> {
    // SAFETY: both optional pointers are null, requesting a fresh unnamed job with a
    // non-inheritable handle and the default security descriptor. No caller memory is accessed.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateJobObjectW returned a valid, uniquely owned handle closed by OwnedHandle.
    let job = unsafe { OwnedHandle::from_raw_handle(job) };

    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    // In particular, do not set BREAKAWAY_OK or SILENT_BREAKAWAY_OK.
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // The fixed Windows job-limit structure is smaller than a DWORD's 4 GiB size range.
    let limits_size = size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
    // SAFETY: job is live with SET_ATTRIBUTES access. The pointer is correctly aligned for the
    // selected information class, covers the full structure with initialized fields, and lives
    // for the entire call. SetInformationJobObject does not retain the pointer.
    if unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&limits).cast(),
            limits_size,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let group = ProcessGroup {
        job,
        terminated: false,
    };

    let child = command.creation_flags(CREATE_SUSPENDED).spawn();
    command.creation_flags(0);
    let mut child = child?;

    if let Err(error) = assign_and_resume(&child, &group) {
        // Close the sole job handle first, covering even a failure after successful resumption.
        // If assignment failed, killing the process directly handles the still-suspended leader.
        drop(group);
        let kill_result = child.kill();
        if let Err(wait_error) = child.wait() {
            return Err(io::Error::new(
                error.kind(),
                format!(
                    "{error}; suspended child cleanup failed: kill={kill_result:?}, \
                     wait={wait_error}"
                ),
            ));
        }
        return Err(error);
    }

    Ok((child, group))
}

/// Attaches the suspended child, finds its sole initial thread, and resumes that owned thread.
fn assign_and_resume(child: &Child, group: &ProcessGroup) -> io::Result<()> {
    // SAFETY: the live job handle has ASSIGN_PROCESS access; std's CreateProcess result owns a
    // live process handle with SET_QUOTA and TERMINATE access. Both borrows span this call.
    if unsafe { AssignProcessToJobObject(group.job.as_raw_handle(), child.as_raw_handle()) } == 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: these flags request only a system thread snapshot, without inheritance or any
    // heaps/modules from the suspended process. There are no pointer arguments.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: snapshot is the successful, uniquely owned ToolHelp handle, closed by OwnedHandle.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    // The fixed Windows thread-entry structure is smaller than a DWORD's 4 GiB size range.
    let entry_size = size_of::<THREADENTRY32>() as u32;
    let mut entry = THREADENTRY32 {
        dwSize: entry_size,
        ..THREADENTRY32::default()
    };
    // SAFETY: snapshot stays open; entry is an initialized, aligned, exclusively borrowed
    // THREADENTRY32 with dwSize set to its full allocation size. No pointer is retained.
    let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
    let mut initial_thread = None;
    let mut seen = 0;

    loop {
        if found == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != i32::try_from(ERROR_NO_MORE_FILES).ok() {
                return Err(error);
            }
            break;
        }
        if seen == MAX_SNAPSHOT_THREADS {
            return Err(io::Error::other(
                "Windows thread snapshot exceeds the scan limit",
            ));
        }
        seen += 1;
        // ToolHelp may return a shorter structure; never trust an omitted ownership field.
        let owner_end = offset_of!(THREADENTRY32, th32OwnerProcessID) + size_of::<u32>();
        // Windows usize is at least 32 bits, so every DWORD value fits without truncation.
        if (entry.dwSize as usize) < owner_end {
            return Err(io::Error::other(
                "Windows thread snapshot omitted thread ownership",
            ));
        }
        if entry.th32OwnerProcessID == child.id() {
            if initial_thread.is_some() {
                return Err(io::Error::other(
                    "suspended child has more than one initial thread",
                ));
            }
            // SAFETY: OpenThread takes scalar values, not pointers. The returned handle is
            // non-inheritable; its actual owner is checked below before any resumption.
            let thread = unsafe {
                OpenThread(
                    THREAD_SUSPEND_RESUME | THREAD_QUERY_LIMITED_INFORMATION,
                    0,
                    entry.th32ThreadID,
                )
            };
            if thread.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: OpenThread returned a valid, uniquely owned handle closed by OwnedHandle.
            initial_thread = Some(unsafe { OwnedHandle::from_raw_handle(thread) });
        }

        entry.dwSize = entry_size;
        // SAFETY: snapshot remains owned and open; entry is the same initialized, aligned,
        // exclusively borrowed full-size buffer used by Thread32First. No pointer is retained.
        found = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
    }

    let thread = initial_thread
        .ok_or_else(|| io::Error::other("suspended child's initial thread was not found"))?;
    // SAFETY: thread remains owned and open with QUERY_LIMITED_INFORMATION access.
    let owner = unsafe { GetProcessIdOfThread(thread.as_raw_handle()) };
    if owner == 0 {
        return Err(io::Error::last_os_error());
    }
    if owner != child.id() {
        return Err(io::Error::other(
            "snapshot thread no longer belongs to the child",
        ));
    }
    // Checking liveness AFTER ownership prevents an exited leader's recycled PID from validating
    // an unrelated thread. Once validated, the owned thread handle cannot change its referent.
    // SAFETY: std owns the live process handle with SYNCHRONIZE access throughout this call.
    // The zero timeout never blocks, and no concurrent close is possible through this borrow.
    match unsafe { WaitForSingleObject(child.as_raw_handle(), 0) } {
        WAIT_TIMEOUT => {}
        WAIT_FAILED => return Err(io::Error::last_os_error()),
        _ => {
            return Err(io::Error::other(
                "suspended child exited before thread resumption",
            ))
        }
    }

    // SAFETY: thread is the ownership-checked stable handle with SUSPEND_RESUME access; the child
    // has already been assigned to our job. The handle is kept open throughout resumption.
    match unsafe { ResumeThread(thread.as_raw_handle()) } {
        1 => Ok(()),
        u32::MAX => Err(io::Error::last_os_error()),
        _ => Err(io::Error::other(
            "initial thread had an unexpected suspend count",
        )),
    }
}
