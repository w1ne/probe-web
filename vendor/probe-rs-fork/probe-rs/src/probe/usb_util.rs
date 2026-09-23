use futures_lite::FutureExt;
use nusb::{
    Interface,
    transfer::{Bulk, In, Out},
};
use std::{io, time::Duration};

// WebUSB cannot cancel a transfer, so after a timeout the endpoint still has one
// queued and later reads are out of step: on wasm a timeout is terminal for the
// probe (close and reopen it), never something to retry.
#[cfg(not(target_family = "wasm"))]
const TIMEOUT_WRITE_MSG: &str = "bulk write timed out";
#[cfg(not(target_family = "wasm"))]
const TIMEOUT_READ_MSG: &str = "bulk read timed out";
#[cfg(target_family = "wasm")]
const TIMEOUT_WRITE_MSG: &str =
    "bulk write timed out; WebUSB cannot cancel the transfer, so the probe must be reopened";
#[cfg(target_family = "wasm")]
const TIMEOUT_READ_MSG: &str =
    "bulk read timed out; WebUSB cannot cancel the transfer, so the probe must be reopened";

pub trait InterfaceExt {
    async fn read_bulk(&self, endpoint: u8, buf: &mut [u8], timeout: Duration)
    -> io::Result<usize>;
    async fn write_bulk(&self, endpoint: u8, buf: &[u8], timeout: Duration) -> io::Result<usize>;
}

/// Await `fut`, giving up after `timeout`.
///
/// Returns `None` on timeout. The caller is responsible for cancelling and
/// draining any transfer it had submitted: a pending transfer that is merely
/// dropped stays queued on the endpoint and its data will be delivered to the
/// *next* read, desynchronising the protocol. That cannot be papered over on
/// wasm, where WebUSB gives no way to abort an in-flight transfer.
async fn with_timeout<T>(fut: impl Future<Output = T>, timeout: Duration) -> Option<T> {
    async { Some(fut.await) }
        .or(async {
            wait(timeout).await;
            None
        })
        .await
}

/// Cancel any outstanding transfer on `ep` and drain its completion, so the
/// endpoint is left with nothing queued.
///
/// On WebUSB this is a no-op: the spec exposes no transfer-cancellation API
/// (<https://github.com/WICG/webusb/issues/25>), so `nusb` compiles
/// `Endpoint::cancel_all` only for non-wasm targets. A timed-out transfer
/// therefore stays queued in the browser and its data will be delivered to the
/// *next* read on that endpoint, desynchronising the protocol. Code that treats
/// a timeout as ordinary control flow rather than an error is unsafe to run
/// against WebUSB for that reason.
#[cfg(not(target_arch = "wasm32"))]
async fn cancel_and_drain<EpType, Dir>(ep: &mut nusb::Endpoint<EpType, Dir>)
where
    EpType: nusb::transfer::BulkOrInterrupt,
    Dir: nusb::transfer::EndpointDirection,
{
    ep.cancel_all();
    // Whether what comes back is the original transfer, the cancellation, or
    // nothing at all, drop it: the caller is reporting a timeout either way.
    let _ = with_timeout(ep.next_complete(), Duration::from_millis(100)).await;
}

#[cfg(target_arch = "wasm32")]
async fn cancel_and_drain<EpType, Dir>(_ep: &mut nusb::Endpoint<EpType, Dir>)
where
    EpType: nusb::transfer::BulkOrInterrupt,
    Dir: nusb::transfer::EndpointDirection,
{
    // Nothing we can do; see the doc comment above.
}

/// Spike D instrumentation: per-transfer cost breakdown on wasm.
#[cfg(target_family = "wasm")]
pub mod stats {
    use std::cell::Cell;
    thread_local! {
        pub static WRITES: Cell<u64> = const { Cell::new(0) };
        pub static READS: Cell<u64> = const { Cell::new(0) };
        pub static EP_NS: Cell<u64> = const { Cell::new(0) };
        pub static XFER_NS: Cell<u64> = const { Cell::new(0) };
        pub static BYTES: Cell<u64> = const { Cell::new(0) };
    }
    pub fn add(c: &'static std::thread::LocalKey<Cell<u64>>, v: u64) {
        c.with(|x| x.set(x.get() + v));
    }
    pub fn report() -> String {
        let (w, r, ep, xf, b) = (
            WRITES.with(|x| x.get()),
            READS.with(|x| x.get()),
            EP_NS.with(|x| x.get()),
            XFER_NS.with(|x| x.get()),
            BYTES.with(|x| x.get()),
        );
        let n = (w + r).max(1);
        format!(
            "usb: {w} writes, {r} reads, {b} bytes; endpoint-create {:.1} ms total ({:.0} us/xfer); submit→complete {:.1} ms total ({:.0} us/xfer)",
            ep as f64 / 1e6,
            ep as f64 / 1e3 / n as f64,
            xf as f64 / 1e6,
            xf as f64 / 1e3 / n as f64
        )
    }
    pub fn reset() {
        for c in [&WRITES, &READS, &EP_NS, &XFER_NS, &BYTES] {
            c.with(|x| x.set(0));
        }
    }
}

#[cfg(target_family = "wasm")]
macro_rules! timed {
    ($counter:expr, $e:expr) => {{
        let t = web_time::Instant::now();
        let r = $e;
        stats::add(&$counter, t.elapsed().as_nanos() as u64);
        r
    }};
}
#[cfg(not(target_family = "wasm"))]
macro_rules! timed {
    ($counter:expr, $e:expr) => {
        $e
    };
}

impl InterfaceExt for Interface {
    async fn write_bulk(&self, endpoint: u8, buf: &[u8], timeout: Duration) -> io::Result<usize> {
        #[cfg(target_family = "wasm")]
        {
            stats::add(&stats::WRITES, 1);
            stats::add(&stats::BYTES, buf.len() as u64);
        }
        let mut ep_out = timed!(stats::EP_NS, self
            .endpoint::<Bulk, Out>(endpoint)
            .map_err(io::Error::from))?;

        let mut transfer = ep_out.allocate(buf.len());
        transfer.extend_from_slice(buf);
        ep_out.submit(transfer);

        let Some(comp) = timed!(stats::XFER_NS, with_timeout(ep_out.next_complete(), timeout).await) else {
            cancel_and_drain(&mut ep_out).await;
            return Err(io::Error::new(io::ErrorKind::TimedOut, TIMEOUT_WRITE_MSG));
        };

        comp.status.map_err(io::Error::from)?;
        Ok(comp.actual_len)
    }

    async fn read_bulk(
        &self,
        endpoint: u8,
        buf: &mut [u8],
        timeout: Duration,
    ) -> io::Result<usize> {
        #[cfg(target_family = "wasm")]
        stats::add(&stats::READS, 1);
        let mut ep_in = timed!(stats::EP_NS, self
            .endpoint::<Bulk, In>(endpoint)
            .map_err(io::Error::from))?;

        // nusb >= 0.2 rejects an IN transfer whose requested length is zero or
        // not a multiple of the endpoint's max packet size, with
        // TransferError::InvalidArgument. Callers pass arbitrary lengths, so
        // round the request up and copy back only what was asked for.
        let max_packet_size = ep_in.max_packet_size().max(1);
        let requested_len = buf.len().div_ceil(max_packet_size) * max_packet_size;

        ep_in.submit(ep_in.allocate(requested_len));

        let Some(comp) = timed!(stats::XFER_NS, with_timeout(ep_in.next_complete(), timeout).await) else {
            cancel_and_drain(&mut ep_in).await;
            return Err(io::Error::new(io::ErrorKind::TimedOut, TIMEOUT_READ_MSG));
        };

        comp.status.map_err(io::Error::from)?;

        let actual_len = comp.actual_len;
        let data = comp.buffer;

        if actual_len > buf.len() || data.len() > buf.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "device returned {actual_len} bytes, buffer length is {}",
                    buf.len()
                ),
            ));
        }

        buf[..actual_len].copy_from_slice(&data[..actual_len]);
        #[cfg(target_family = "wasm")]
        stats::add(&stats::BYTES, actual_len as u64);
        Ok(actual_len)
    }
}

#[cfg(target_family = "wasm")]
pub async fn wait(timeout: Duration) {
    pub(crate) fn set_timeout(resolve: wasm_bindgen_futures::js_sys::Function, ms: i32) {
        let window = wasm_bindgen::JsCast::dyn_into::<web_sys::Window>(
            wasm_bindgen_futures::js_sys::global(),
        )
        .ok();

        if let Some(window) = window {
            window
                .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms)
                .expect("timeouts work");
            return;
        }

        let wgs = wasm_bindgen::JsCast::dyn_into::<web_sys::WorkerGlobalScope>(
            wasm_bindgen_futures::js_sys::global(),
        )
        .ok();

        if let Some(wgs) = wgs {
            wgs.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms)
                .expect("timeouts work");
            return;
        }

        panic!("Timeout could not be set")
    }

    let promise = wasm_bindgen_futures::js_sys::Promise::new(&mut |resolve, _| {
        set_timeout(resolve, timeout.as_millis() as i32)
    });

    wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .expect("promise completes without issues");
}

#[cfg(not(target_family = "wasm"))]
pub async fn wait(timeout: Duration) {
    async_io::Timer::after(timeout).await;
}
