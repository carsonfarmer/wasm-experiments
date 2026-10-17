//! Host built-in stages that aren't components.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::task::{Context, Poll};
use wasmtime::component::{Destination, StreamAny, StreamProducer, StreamReader, StreamResult, VecBuffer};
use wasmtime::{AsContextMut, StoreContextMut};

/// `cat PATH`: stream a file's bytes as a `stream<u8>`, reading straight into
/// the consumer's read buffer (no intermediate copy).
pub struct Cat {
    file: File,
    bytes: Arc<AtomicU64>,
}

impl<D: 'static> StreamProducer<D> for Cat {
    type Item = u8;
    type Buffer = VecBuffer<u8>;

    fn poll_produce<'a>(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        mut store: StoreContextMut<'a, D>,
        dst: Destination<'a, u8, VecBuffer<u8>>,
        _finish: bool,
    ) -> Poll<wasmtime::Result<StreamResult>> {
        let me = self.get_mut();
        let n = match dst.remaining(store.as_context_mut()) {
            Some(0) => return Poll::Ready(Ok(StreamResult::Completed)),
            Some(cap) => {
                let mut direct = dst.as_direct(store, cap);
                let n = read_full(&mut me.file, direct.remaining())?;
                direct.mark_written(n);
                n
            }
            None => {
                let mut buf = vec![0u8; 1 << 16];
                let n = read_full(&mut me.file, &mut buf)?;
                buf.truncate(n);
                let mut dst = dst;
                dst.set_buffer(VecBuffer::from(buf));
                n
            }
        };
        me.bytes.fetch_add(n as u64, Relaxed);
        Poll::Ready(Ok(if n == 0 { StreamResult::Dropped } else { StreamResult::Completed }))
    }
}

fn read_full(file: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match file.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

pub fn cat<D: 'static>(
    mut store: StoreContextMut<'_, D>,
    path: &Path,
    bytes: Arc<AtomicU64>,
) -> anyhow::Result<StreamAny> {
    let file = File::open(path).map_err(|e| anyhow::anyhow!("cat {}: {e}", path.display()))?;
    let reader = StreamReader::new(&mut store, Cat { file, bytes })?;
    Ok(StreamAny::try_from_stream_reader(&mut store, reader)?)
}
