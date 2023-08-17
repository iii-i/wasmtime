use crate::preview2::{
    bindings::io::streams::{self, InputStream, OutputStream},
    bindings::poll::poll::Pollable,
    filesystem::{FileInputStream, FileOutputStream},
    poll::PollableFuture,
    stream::{
        HostInputStream, HostOutputStream, InternalInputStream, InternalOutputStream,
        InternalTableStreamExt, StreamRuntimeError, StreamState,
    },
    HostPollable, TablePollableExt, WasiView,
};
use std::any::Any;

impl From<StreamState> for streams::StreamStatus {
    fn from(state: StreamState) -> Self {
        match state {
            StreamState::Open => Self::Open,
            StreamState::Closed => Self::Ended,
        }
    }
}

const ZEROS: &[u8] = &[0; 4 * 1024 * 1024];

#[async_trait::async_trait]
impl<T: WasiView> streams::Host for T {
    async fn drop_input_stream(&mut self, stream: InputStream) -> anyhow::Result<()> {
        self.table_mut().delete_internal_input_stream(stream)?;
        Ok(())
    }

    async fn drop_output_stream(&mut self, stream: OutputStream) -> anyhow::Result<()> {
        self.table_mut().delete_internal_output_stream(stream)?;
        Ok(())
    }

    async fn read(
        &mut self,
        stream: InputStream,
        len: u64,
    ) -> anyhow::Result<Result<(Vec<u8>, streams::StreamStatus), ()>> {
        match self.table_mut().get_internal_input_stream_mut(stream)? {
            InternalInputStream::Host(s) => {
                let (bytes, state) = match HostInputStream::read(s.as_mut(), len as usize) {
                    Ok(a) => a,
                    Err(e) => {
                        if let Some(e) = e.downcast_ref::<StreamRuntimeError>() {
                            tracing::debug!("stream runtime error: {e:?}");
                            return Ok(Err(()));
                        } else {
                            return Err(e);
                        }
                    }
                };
                debug_assert!(bytes.len() <= len as usize);

                Ok(Ok((bytes.into(), state.into())))
            }
            InternalInputStream::File(s) => {
                let (bytes, state) = match FileInputStream::read(s, len as usize).await {
                    Ok(a) => a,
                    Err(e) => {
                        if let Some(e) = e.downcast_ref::<StreamRuntimeError>() {
                            tracing::debug!("stream runtime error: {e:?}");
                            return Ok(Err(()));
                        } else {
                            return Err(e);
                        }
                    }
                };
                Ok(Ok((bytes.into(), state.into())))
            }
        }
    }

    async fn blocking_read(
        &mut self,
        stream: InputStream,
        len: u64,
    ) -> anyhow::Result<Result<(Vec<u8>, streams::StreamStatus), ()>> {
        match self.table_mut().get_internal_input_stream_mut(stream)? {
            InternalInputStream::Host(s) => {
                s.ready().await?;
                let (bytes, state) = match HostInputStream::read(s.as_mut(), len as usize) {
                    Ok(a) => a,
                    Err(e) => {
                        if let Some(e) = e.downcast_ref::<StreamRuntimeError>() {
                            tracing::debug!("stream runtime error: {e:?}");
                            return Ok(Err(()));
                        } else {
                            return Err(e);
                        }
                    }
                };
                debug_assert!(bytes.len() <= len as usize);
                Ok(Ok((bytes.into(), state.into())))
            }
            InternalInputStream::File(s) => {
                let (bytes, state) = match FileInputStream::read(s, len as usize).await {
                    Ok(a) => a,
                    Err(e) => {
                        if let Some(e) = e.downcast_ref::<StreamRuntimeError>() {
                            tracing::debug!("stream runtime error: {e:?}");
                            return Ok(Err(()));
                        } else {
                            return Err(e);
                        }
                    }
                };
                Ok(Ok((bytes.into(), state.into())))
            }
        }
    }

    async fn write(
        &mut self,
        stream: OutputStream,
        bytes: Vec<u8>,
    ) -> anyhow::Result<Result<(u64, streams::StreamStatus), ()>> {
        match self.table_mut().get_internal_output_stream_mut(stream)? {
            InternalOutputStream::Host(s) => {
                let (bytes_written, status) =
                    match HostOutputStream::write(s.as_mut(), bytes.into()) {
                        Ok(a) => a,
                        Err(e) => {
                            if let Some(e) = e.downcast_ref::<StreamRuntimeError>() {
                                tracing::debug!("stream runtime error: {e:?}");
                                return Ok(Err(()));
                            } else {
                                return Err(e);
                            }
                        }
                    };
                Ok(Ok((u64::try_from(bytes_written).unwrap(), status.into())))
            }
            InternalOutputStream::File(s) => {
                let (nwritten, state) = FileOutputStream::write(s, bytes.into()).await?;
                Ok(Ok((nwritten as u64, state.into())))
            }
        }
    }

    async fn blocking_write(
        &mut self,
        stream: OutputStream,
        bytes: Vec<u8>,
    ) -> anyhow::Result<Result<(u64, streams::StreamStatus), ()>> {
        match self.table_mut().get_internal_output_stream_mut(stream)? {
            InternalOutputStream::Host(s) => {
                let mut bytes = bytes::Bytes::from(bytes);
                let mut nwritten: usize = 0;
                loop {
                    s.ready().await?;
                    let (written, state) = match HostOutputStream::write(s.as_mut(), bytes.clone())
                    {
                        Ok(a) => a,
                        Err(e) => {
                            if let Some(e) = e.downcast_ref::<StreamRuntimeError>() {
                                tracing::debug!("stream runtime error: {e:?}");
                                return Ok(Err(()));
                            } else {
                                return Err(e);
                            }
                        }
                    };
                    let _ = bytes.split_to(written);
                    nwritten += written;
                    if bytes.is_empty() || state == StreamState::Closed {
                        return Ok(Ok((nwritten as u64, state.into())));
                    }
                }
            }
            InternalOutputStream::File(s) => {
                let (written, state) = FileOutputStream::write(s, bytes.into()).await?;
                Ok(Ok((written as u64, state.into())))
            }
        }
    }

    async fn skip(
        &mut self,
        stream: InputStream,
        len: u64,
    ) -> anyhow::Result<Result<(u64, streams::StreamStatus), ()>> {
        match self.table_mut().get_internal_input_stream_mut(stream)? {
            InternalInputStream::Host(s) => {
                // TODO: the cast to usize should be fallible, use `.try_into()?`
                let (bytes_skipped, state) = match HostInputStream::skip(s.as_mut(), len as usize) {
                    Ok(a) => a,
                    Err(e) => {
                        if let Some(e) = e.downcast_ref::<StreamRuntimeError>() {
                            tracing::debug!("stream runtime error: {e:?}");
                            return Ok(Err(()));
                        } else {
                            return Err(e);
                        }
                    }
                };

                Ok(Ok((bytes_skipped as u64, state.into())))
            }
            InternalInputStream::File(s) => {
                let (bytes_skipped, state) = match FileInputStream::skip(s, len as usize).await {
                    Ok(a) => a,
                    Err(e) => {
                        if let Some(e) = e.downcast_ref::<StreamRuntimeError>() {
                            tracing::debug!("stream runtime error: {e:?}");
                            return Ok(Err(()));
                        } else {
                            return Err(e);
                        }
                    }
                };
                Ok(Ok((bytes_skipped as u64, state.into())))
            }
        }
    }

    async fn blocking_skip(
        &mut self,
        stream: InputStream,
        len: u64,
    ) -> anyhow::Result<Result<(u64, streams::StreamStatus), ()>> {
        match self.table_mut().get_internal_input_stream_mut(stream)? {
            InternalInputStream::Host(s) => {
                s.ready().await?;
                // TODO: the cast to usize should be fallible, use `.try_into()?`
                let (bytes_skipped, state) = match HostInputStream::skip(s.as_mut(), len as usize) {
                    Ok(a) => a,
                    Err(e) => {
                        if let Some(e) = e.downcast_ref::<StreamRuntimeError>() {
                            tracing::debug!("stream runtime error: {e:?}");
                            return Ok(Err(()));
                        } else {
                            return Err(e);
                        }
                    }
                };

                Ok(Ok((bytes_skipped as u64, state.into())))
            }
            InternalInputStream::File(s) => {
                let (bytes_skipped, state) = match FileInputStream::skip(s, len as usize).await {
                    Ok(a) => a,
                    Err(e) => {
                        if let Some(e) = e.downcast_ref::<StreamRuntimeError>() {
                            tracing::debug!("stream runtime error: {e:?}");
                            return Ok(Err(()));
                        } else {
                            return Err(e);
                        }
                    }
                };
                Ok(Ok((bytes_skipped as u64, state.into())))
            }
        }
    }

    async fn write_zeroes(
        &mut self,
        stream: OutputStream,
        len: u64,
    ) -> anyhow::Result<Result<(u64, streams::StreamStatus), ()>> {
        let s = self.table_mut().get_internal_output_stream_mut(stream)?;
        let mut bytes = bytes::Bytes::from_static(ZEROS);
        bytes.truncate((len as usize).min(bytes.len()));
        let (written, state) = match s {
            InternalOutputStream::Host(s) => match HostOutputStream::write(s.as_mut(), bytes) {
                Ok(a) => a,
                Err(e) => {
                    if let Some(e) = e.downcast_ref::<StreamRuntimeError>() {
                        tracing::debug!("stream runtime error: {e:?}");
                        return Ok(Err(()));
                    } else {
                        return Err(e);
                    }
                }
            },
            InternalOutputStream::File(s) => match FileOutputStream::write(s, bytes).await {
                Ok(a) => a,
                Err(e) => {
                    if let Some(e) = e.downcast_ref::<StreamRuntimeError>() {
                        tracing::debug!("stream runtime error: {e:?}");
                        return Ok(Err(()));
                    } else {
                        return Err(e);
                    }
                }
            },
        };
        Ok(Ok((written as u64, state.into())))
    }

    async fn blocking_write_zeroes(
        &mut self,
        stream: OutputStream,
        len: u64,
    ) -> anyhow::Result<Result<(u64, streams::StreamStatus), ()>> {
        let mut remaining = len as usize;
        let s = self.table_mut().get_internal_output_stream_mut(stream)?;
        loop {
            if let InternalOutputStream::Host(s) = s {
                HostOutputStream::ready(s.as_mut()).await?;
            }
            let mut bytes = bytes::Bytes::from_static(ZEROS);
            bytes.truncate(remaining.min(bytes.len()));
            let (written, state) = match s {
                InternalOutputStream::Host(s) => match HostOutputStream::write(s.as_mut(), bytes) {
                    Ok(a) => a,
                    Err(e) => {
                        if let Some(e) = e.downcast_ref::<StreamRuntimeError>() {
                            tracing::debug!("stream runtime error: {e:?}");
                            return Ok(Err(()));
                        } else {
                            return Err(e);
                        }
                    }
                },
                InternalOutputStream::File(s) => match FileOutputStream::write(s, bytes).await {
                    Ok(a) => a,
                    Err(e) => {
                        if let Some(e) = e.downcast_ref::<StreamRuntimeError>() {
                            tracing::debug!("stream runtime error: {e:?}");
                            return Ok(Err(()));
                        } else {
                            return Err(e);
                        }
                    }
                },
            };
            remaining -= written;
            if remaining == 0 || state == StreamState::Closed {
                return Ok(Ok((len - remaining as u64, state.into())));
            }
        }
    }

    async fn splice(
        &mut self,
        _src: InputStream,
        _dst: OutputStream,
        _len: u64,
    ) -> anyhow::Result<Result<(u64, streams::StreamStatus), ()>> {
        // TODO: We can't get two streams at the same time because they both
        // carry the exclusive lifetime of `ctx`. When [`get_many_mut`] is
        // stabilized, that could allow us to add a `get_many_stream_mut` or
        // so which lets us do this.
        //
        // [`get_many_mut`]: https://doc.rust-lang.org/stable/std/collections/hash_map/struct.HashMap.html#method.get_many_mut
        /*
        let s: &mut Box<dyn crate::InputStream> = ctx
            .table_mut()
            .get_input_stream_mut(src)
            ?;
        let d: &mut Box<dyn crate::OutputStream> = ctx
            .table_mut()
            .get_output_stream_mut(dst)
            ?;

        let bytes_spliced: u64 = s.splice(&mut **d, len).await?;

        Ok(bytes_spliced)
        */
        todo!("stream splice is not implemented")
    }

    async fn blocking_splice(
        &mut self,
        _src: InputStream,
        _dst: OutputStream,
        _len: u64,
    ) -> anyhow::Result<Result<(u64, streams::StreamStatus), ()>> {
        // TODO: once splice is implemented, figure out what the blocking semantics are for waiting
        // on src and dest here.
        todo!("stream splice is not implemented")
    }

    async fn forward(
        &mut self,
        _src: InputStream,
        _dst: OutputStream,
    ) -> anyhow::Result<Result<(u64, streams::StreamStatus), ()>> {
        // TODO: We can't get two streams at the same time because they both
        // carry the exclusive lifetime of `ctx`. When [`get_many_mut`] is
        // stabilized, that could allow us to add a `get_many_stream_mut` or
        // so which lets us do this.
        //
        // [`get_many_mut`]: https://doc.rust-lang.org/stable/std/collections/hash_map/struct.HashMap.html#method.get_many_mut
        /*
        let s: &mut Box<dyn crate::InputStream> = ctx
            .table_mut()
            .get_input_stream_mut(src)
            ?;
        let d: &mut Box<dyn crate::OutputStream> = ctx
            .table_mut()
            .get_output_stream_mut(dst)
            ?;

        let bytes_spliced: u64 = s.splice(&mut **d, len).await?;

        Ok(bytes_spliced)
        */

        todo!("stream forward is not implemented")
    }

    async fn subscribe_to_input_stream(&mut self, stream: InputStream) -> anyhow::Result<Pollable> {
        // Ensure that table element is an input-stream:
        let pollable = match self.table_mut().get_internal_input_stream_mut(stream)? {
            InternalInputStream::Host(_) => {
                fn input_stream_ready<'a>(stream: &'a mut dyn Any) -> PollableFuture<'a> {
                    let stream = stream
                        .downcast_mut::<InternalInputStream>()
                        .expect("downcast to InternalInputStream failed");
                    match *stream {
                        InternalInputStream::Host(ref mut hs) => hs.ready(),
                        _ => unreachable!(),
                    }
                }

                HostPollable::TableEntry {
                    index: stream,
                    make_future: input_stream_ready,
                }
            }
            // Files are always "ready" immediately (because we have no way to actually wait on
            // readiness in epoll)
            InternalInputStream::File(_) => {
                HostPollable::Closure(Box::new(|| Box::pin(futures::future::ready(Ok(())))))
            }
        };
        Ok(self.table_mut().push_host_pollable(pollable)?)
    }

    async fn subscribe_to_output_stream(
        &mut self,
        stream: OutputStream,
    ) -> anyhow::Result<Pollable> {
        // Ensure that table element is an output-stream:
        let pollable = match self.table_mut().get_internal_output_stream_mut(stream)? {
            InternalOutputStream::Host(_) => {
                fn output_stream_ready<'a>(stream: &'a mut dyn Any) -> PollableFuture<'a> {
                    let stream = stream
                        .downcast_mut::<InternalOutputStream>()
                        .expect("downcast to HostOutputStream failed");
                    match *stream {
                        InternalOutputStream::Host(ref mut hs) => hs.ready(),
                        _ => unreachable!(),
                    }
                }

                HostPollable::TableEntry {
                    index: stream,
                    make_future: output_stream_ready,
                }
            }
            InternalOutputStream::File(_) => {
                HostPollable::Closure(Box::new(|| Box::pin(futures::future::ready(Ok(())))))
            }
        };

        Ok(self.table_mut().push_host_pollable(pollable)?)
    }
}

pub mod sync {
    use crate::preview2::{
        bindings::io::streams::{Host as AsyncHost, StreamStatus as AsyncStreamStatus},
        bindings::sync_io::io::streams::{self, InputStream, OutputStream},
        bindings::sync_io::poll::poll::Pollable,
        in_tokio, WasiView,
    };

    // same boilerplate everywhere, converting between two identical types with different
    // definition sites. one day wasmtime-wit-bindgen will make all this unnecessary
    fn xform<A>(r: Result<(A, AsyncStreamStatus), ()>) -> Result<(A, streams::StreamStatus), ()> {
        r.map(|(a, b)| (a, b.into()))
    }

    impl From<AsyncStreamStatus> for streams::StreamStatus {
        fn from(other: AsyncStreamStatus) -> Self {
            match other {
                AsyncStreamStatus::Open => Self::Open,
                AsyncStreamStatus::Ended => Self::Ended,
            }
        }
    }

    impl<T: WasiView> streams::Host for T {
        fn drop_input_stream(&mut self, stream: InputStream) -> anyhow::Result<()> {
            in_tokio(async { AsyncHost::drop_input_stream(self, stream).await })
        }

        fn drop_output_stream(&mut self, stream: OutputStream) -> anyhow::Result<()> {
            in_tokio(async { AsyncHost::drop_output_stream(self, stream).await })
        }

        fn read(
            &mut self,
            stream: InputStream,
            len: u64,
        ) -> anyhow::Result<Result<(Vec<u8>, streams::StreamStatus), ()>> {
            in_tokio(async { AsyncHost::read(self, stream, len).await }).map(xform)
        }

        fn blocking_read(
            &mut self,
            stream: InputStream,
            len: u64,
        ) -> anyhow::Result<Result<(Vec<u8>, streams::StreamStatus), ()>> {
            in_tokio(async { AsyncHost::blocking_read(self, stream, len).await }).map(xform)
        }

        fn write(
            &mut self,
            stream: OutputStream,
            bytes: Vec<u8>,
        ) -> anyhow::Result<Result<(u64, streams::StreamStatus), ()>> {
            in_tokio(async { AsyncHost::write(self, stream, bytes).await }).map(xform)
        }

        fn blocking_write(
            &mut self,
            stream: OutputStream,
            bytes: Vec<u8>,
        ) -> anyhow::Result<Result<(u64, streams::StreamStatus), ()>> {
            in_tokio(async { AsyncHost::blocking_write(self, stream, bytes).await }).map(xform)
        }

        fn skip(
            &mut self,
            stream: InputStream,
            len: u64,
        ) -> anyhow::Result<Result<(u64, streams::StreamStatus), ()>> {
            in_tokio(async { AsyncHost::skip(self, stream, len).await }).map(xform)
        }

        fn blocking_skip(
            &mut self,
            stream: InputStream,
            len: u64,
        ) -> anyhow::Result<Result<(u64, streams::StreamStatus), ()>> {
            in_tokio(async { AsyncHost::blocking_skip(self, stream, len).await }).map(xform)
        }

        fn write_zeroes(
            &mut self,
            stream: OutputStream,
            len: u64,
        ) -> anyhow::Result<Result<(u64, streams::StreamStatus), ()>> {
            in_tokio(async { AsyncHost::write_zeroes(self, stream, len).await }).map(xform)
        }

        fn blocking_write_zeroes(
            &mut self,
            stream: OutputStream,
            len: u64,
        ) -> anyhow::Result<Result<(u64, streams::StreamStatus), ()>> {
            in_tokio(async { AsyncHost::blocking_write_zeroes(self, stream, len).await }).map(xform)
        }

        fn splice(
            &mut self,
            src: InputStream,
            dst: OutputStream,
            len: u64,
        ) -> anyhow::Result<Result<(u64, streams::StreamStatus), ()>> {
            in_tokio(async { AsyncHost::splice(self, src, dst, len).await }).map(xform)
        }

        fn blocking_splice(
            &mut self,
            src: InputStream,
            dst: OutputStream,
            len: u64,
        ) -> anyhow::Result<Result<(u64, streams::StreamStatus), ()>> {
            in_tokio(async { AsyncHost::blocking_splice(self, src, dst, len).await }).map(xform)
        }

        fn forward(
            &mut self,
            src: InputStream,
            dst: OutputStream,
        ) -> anyhow::Result<Result<(u64, streams::StreamStatus), ()>> {
            in_tokio(async { AsyncHost::forward(self, src, dst).await }).map(xform)
        }

        fn subscribe_to_input_stream(&mut self, stream: InputStream) -> anyhow::Result<Pollable> {
            in_tokio(async { AsyncHost::subscribe_to_input_stream(self, stream).await })
        }

        fn subscribe_to_output_stream(&mut self, stream: OutputStream) -> anyhow::Result<Pollable> {
            in_tokio(async { AsyncHost::subscribe_to_output_stream(self, stream).await })
        }
    }
}
