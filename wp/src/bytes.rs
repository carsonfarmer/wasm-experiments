//! Feeding bytes (stdin, a file) to the first tool as a `stream<u8>`.

use std::pin::Pin;
use std::task::{Context, Poll, ready};
use tokio::io::{AsyncRead, ReadBuf};
use wasmtime::component::{Destination, StreamAny, StreamProducer, StreamReader, StreamResult, VecBuffer};
use wasmtime::{AsContextMut, StoreContextMut};

struct Bytes<R>(R);

impl<D: 'static, R: AsyncRead + Send + Unpin + 'static> StreamProducer<D> for Bytes<R> {
    type Item = u8;
    type Buffer = VecBuffer<u8>;

    fn poll_produce<'a>(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut store: StoreContextMut<'a, D>,
        dst: Destination<'a, u8, VecBuffer<u8>>,
        finish: bool,
    ) -> Poll<wasmtime::Result<StreamResult>> {
        let Some(cap) = dst.remaining(store.as_context_mut()) else {
            return Poll::Ready(Err(wasmtime::format_err!("input bytes can only feed a component")));
        };
        if cap == 0 {
            return Poll::Ready(Ok(StreamResult::Completed));
        }
        // Read straight into the component's buffer.
        let mut dst = dst.as_direct(store, cap);
        let mut buf = ReadBuf::new(dst.remaining());
        let read = match Pin::new(&mut self.get_mut().0).poll_read(cx, &mut buf) {
            Poll::Pending if finish => return Poll::Ready(Ok(StreamResult::Cancelled)),
            poll => ready!(poll),
        };
        read?;
        let n = buf.filled().len();
        dst.mark_written(n);
        Poll::Ready(Ok(if n == 0 { StreamResult::Dropped } else { StreamResult::Completed }))
    }
}

/// A `stream<u8>` of everything `input` yields.
pub fn stream<D: 'static>(
    mut store: StoreContextMut<'_, D>,
    input: impl AsyncRead + Send + Unpin + 'static,
) -> wasmtime::Result<StreamAny> {
    let reader = StreamReader::new(&mut store, Bytes(input))?;
    StreamAny::try_from_stream_reader(&mut store, reader)
}
