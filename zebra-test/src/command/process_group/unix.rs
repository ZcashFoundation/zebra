//! Owned Unix process groups for test commands.
//!
//! The leader must not be reaped outside this module until the group is retired. In particular,
//! callers must not install a reaping SIGCHLD handler, SIG_IGN, or SA_NOCLDWAIT. Keeping an exited
//! leader waitable reserves its PID until cleanup, so its group ID cannot refer to another group.
//! Descendants that deliberately leave this group are outside its ownership boundary. Kernel signal
//! permissions still apply, including when descendants change credentials.

use std::{
    io,
    os::unix::process::CommandExt,
    process::{Child, Command, ExitStatus},
};

use nix::{
    errno::Errno,
    sys::signal::{kill, Signal},
    unistd::Pid,
};

#[cfg(any(target_os = "redox", target_os = "cygwin", target_os = "l4re"))]
compile_error!("owned test process groups require a native waitid API with WNOWAIT support");

/// A group whose leader's unreaped PID reserves the group ID until cleanup.
#[derive(Debug)]
pub(super) struct ProcessGroup {
    /// `None` after cleanup, even if the caller has not yet reaped the leader.
    leader: Option<Pid>,
}

/// Configure group ownership in the child before it executes the command.
pub(super) fn spawn(command: &mut Command) -> io::Result<(Child, ProcessGroup)> {
    let child = command.process_group(0).spawn()?;
    // Unix allocated a positive pid_t; Child::id converts that native value to u32.
    let leader = Pid::from_raw(child.id() as nix::libc::pid_t);
    Ok((
        child,
        ProcessGroup {
            leader: Some(leader),
        },
    ))
}

impl ProcessGroup {
    /// Kill the owned group without reaping the leader. Repeated cleanup cannot reuse its ID.
    pub(super) fn kill(&mut self, _child: &mut Child) -> io::Result<()> {
        let Some(leader) = self.leader else {
            return Ok(());
        };
        // Detect a lost ownership reservation before signalling any saved numeric ID.
        self.observe_exit(leader)?;
        self.terminate(leader)
    }

    /// Observe exit without reaping, clean up the group, then preserve the leader's actual status.
    pub(super) fn try_wait(&mut self, child: &mut Child) -> io::Result<Option<ExitStatus>> {
        let Some(leader) = self.leader else {
            // Cleanup has retired the numeric ID, so std may safely reap or return cached status.
            return child.try_wait();
        };
        if !self.observe_exit(leader)? {
            return Ok(None);
        }
        self.terminate(leader)?;
        child.wait().map(Some)
    }

    /// Preserve genuine observation failures; ECHILD also revokes permission to signal this ID.
    fn observe_exit(&mut self, leader: Pid) -> io::Result<bool> {
        match leader_exited(leader) {
            Ok(exited) => Ok(exited),
            Err(error) => {
                if error == Errno::ECHILD {
                    self.leader = None;
                }
                Err(error.into())
            }
        }
    }

    /// Signal only while the leader is still unreaped, then permanently retire the saved ID.
    fn terminate(&mut self, leader: Pid) -> io::Result<()> {
        // A spawned leader's PID is positive, so negating it selects exactly its process group.
        match kill(Pid::from_raw(-leader.as_raw()), Signal::SIGKILL) {
            Ok(()) | Err(Errno::ESRCH) => {
                // ESRCH means the owned group has no signalable members, not a cleanup failure.
                self.leader = None;
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }
}

/// Observe terminal presence without decoding real-time signals through nix's closed Signal enum.
#[allow(unsafe_code)]
fn leader_exited(leader: Pid) -> nix::Result<bool> {
    use nix::libc;

    let id = leader.as_raw().try_into().map_err(|_| Errno::EINVAL)?;
    // SAFETY: siginfo_t is a C data structure whose fields admit an all-zero representation.
    // Initialize it because older waitid implementations can leave it unchanged with WNOHANG.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: info is valid writable storage; P_PID selects only our unreaped child. WNOWAIT
    // preserves its PID reservation, and WEXITED requests only terminal status information.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            id,
            &mut info,
            libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
        )
    };
    Errno::result(result)?;
    // Successful exit observation sets si_signo to SIGCHLD; WNOHANG without exit leaves zero.
    Ok(info.si_signo != 0)
}
