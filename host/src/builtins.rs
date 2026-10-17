//! `cat FILE`: a host built-in that streams a file as `stream<u8>`, reading
//! straight into the consumer's read buffer (no intermediate copy).

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::task::{Context, Poll};
use wasmtime::component::{Destination, StreamAny, StreamProducer, StreamReader, StreamResult, VecBuffer};
use wasmtime::{AsContextMut, StoreContextMut};

struct Cat {
    file: File,
    bytes: Arc<AtomicU64>,
}

impl<D: 'static> StreamProducer<D> for Cat {
    type Item = u8;
    type Buffer = VecBuffer<u8>;

    fn poll_produce<'a>(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        mut store: StoreContextMut<'a, D>,
        mut dst: Destination<'a, u8, VecBuffer<u8>>,
        _: bool,
    ) -> Poll<wasmtime::Result<StreamResult>> {
        let me = self.get_mut();
        let n = match dst.remaining(store.as_context_mut()) {
            Some(0) => return Poll::Ready(Ok(StreamResult::Completed)),
            // A guest reader: read the file directly into its buffer.
            Some(cap) => {
                let mut direct = dst.as_direct(store, cap);
                let n = me.file.read(direct.remaining())?;
                direct.mark_written(n);
                n
            }
            // A host reader: hand it a chunk.
            None => {
                let mut buf = vec![0; 1 << 16];
                let n = me.file.read(&mut buf)?;
                buf.truncate(n);
                dst.set_buffer(buf.into());
                n
            }
        };
        me.bytes.fetch_add(n as u64, Relaxed);
        Poll::Ready(Ok(if n == 0 { StreamResult::Dropped } else { StreamResult::Completed }))
    }
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
