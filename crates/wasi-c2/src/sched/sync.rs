#[cfg(unix)]
pub use unix::*;

#[cfg(unix)]
mod unix {
    use crate::file::WasiFile;
    use crate::sched::subscription::{RwEventFlags, Subscription};
    use crate::sched::{Poll, WasiSched};
    use crate::Error;
    use cap_std::time::Duration;
    use std::convert::TryInto;
    use std::ops::Deref;
    use std::os::unix::io::{AsRawFd, RawFd};
    use yanix::poll::{PollFd, PollFlags};

    #[derive(Default)]
    pub struct SyncSched {}

    impl WasiSched for SyncSched {
        fn poll_oneoff<'a>(&self, poll: &'a Poll<'a>) -> Result<(), Error> {
            if poll.is_empty() {
                return Ok(());
            }
            let mut pollfds = Vec::new();
            let timeout = poll.earliest_system_timer();
            for s in poll.rw_subscriptions() {
                match s {
                    Subscription::Read(f) => {
                        let raw_fd = wasi_file_raw_fd(f.file.deref()).ok_or(Error::Inval)?;
                        pollfds.push(unsafe { PollFd::new(raw_fd, PollFlags::POLLIN) });
                    }

                    Subscription::Write(f) => {
                        let raw_fd = wasi_file_raw_fd(f.file.deref()).ok_or(Error::Inval)?;
                        pollfds.push(unsafe { PollFd::new(raw_fd, PollFlags::POLLOUT) });
                    }
                    Subscription::SystemTimer { .. } => unreachable!(),
                }
            }

            let ready = loop {
                let poll_timeout = if let Some(t) = timeout {
                    let duration = t
                        .deadline
                        .duration_since(t.clock.now(t.precision))
                        .unwrap_or(Duration::from_secs(0));
                    (duration.as_millis() + 1) // XXX try always rounding up?
                        .try_into()
                        .map_err(|_| Error::Overflow)?
                } else {
                    libc::c_int::max_value()
                };
                tracing::debug!(
                    poll_timeout = tracing::field::debug(poll_timeout),
                    poll_fds = tracing::field::debug(&pollfds),
                    "poll"
                );
                match yanix::poll::poll(&mut pollfds, poll_timeout) {
                    Ok(ready) => break ready,
                    Err(_) => {
                        let last_err = std::io::Error::last_os_error();
                        if last_err.raw_os_error().unwrap() == libc::EINTR {
                            continue;
                        } else {
                            return Err(last_err.into());
                        }
                    }
                }
            };
            if ready > 0 {
                for (rwsub, pollfd) in poll.rw_subscriptions().zip(pollfds.into_iter()) {
                    if let Some(revents) = pollfd.revents() {
                        let (nbytes, rwsub) = match rwsub {
                            Subscription::Read(sub) => {
                                (1, sub)
                                // XXX FIXME: query_nbytes in wasi-common/src/sys/poll.rs
                                // uses metadata.len - tell to calculate for regular files,
                                // ioctl(fd, FIONREAD) for large files
                            }
                            Subscription::Write(sub) => (0, sub),
                            _ => unreachable!(),
                        };
                        if revents.contains(PollFlags::POLLNVAL) {
                            rwsub.error(Error::Badf);
                        } else if revents.contains(PollFlags::POLLERR) {
                            rwsub.error(Error::Io);
                        } else if revents.contains(PollFlags::POLLHUP) {
                            rwsub.complete(nbytes, RwEventFlags::HANGUP);
                        } else {
                            rwsub.complete(nbytes, RwEventFlags::empty());
                        };
                    }
                }
            } else {
                timeout
                    .expect("timed out")
                    .result()
                    .expect("timer deadline is past")
                    .unwrap()
            }
            Ok(())
        }
        fn sched_yield(&self) -> Result<(), Error> {
            std::thread::yield_now();
            Ok(())
        }
    }

    fn wasi_file_raw_fd(f: &dyn WasiFile) -> Option<RawFd> {
        let a = f.as_any();
        if a.is::<cap_std::fs::File>() {
            Some(a.downcast_ref::<cap_std::fs::File>().unwrap().as_raw_fd())
        } else if a.is::<crate::stdio::Stdin>() {
            Some(a.downcast_ref::<crate::stdio::Stdin>().unwrap().as_raw_fd())
        } else if a.is::<crate::stdio::Stdout>() {
            Some(
                a.downcast_ref::<crate::stdio::Stdout>()
                    .unwrap()
                    .as_raw_fd(),
            )
        } else if a.is::<crate::stdio::Stderr>() {
            Some(
                a.downcast_ref::<crate::stdio::Stderr>()
                    .unwrap()
                    .as_raw_fd(),
            )
        } else {
            None
        }
    }
}
