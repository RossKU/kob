//! Connection-level limits of the HTTP servers (the read API and the x402 facilitator).
//!
//! The request guards (rate limits, concurrency caps, request timeouts) only see a request once its head has been read,
//! so the transport needs its own bounds or a client can hold sockets without ever sending a request:
//!
//! - a header-read deadline: a request head (the first one, and each keep-alive one after a response) must arrive within
//!   `header_timeout`, so slow-header (slowloris) and idle keep-alive connections are closed;
//! - a write-stall deadline: a write that makes no progress for `write_timeout` (a client that pipelines requests and never
//!   reads the responses, or stops reading a response) closes the connection;
//! - a cap on open connections, and a cap per client address (IPv6 per `ipv6_prefix_bits` prefix) so one host cannot hold
//!   every slot. Trusted reverse proxies are exempt from the per-address cap (every client behind them shares their address).
//!   A connection over a cap gets a best-effort `503` and is closed, never served.
//!
//! On shutdown the listener stops accepting, every connection is asked to finish its in-flight request and close
//! (HTTP/1 graceful shutdown), and [`serve`] returns once all of them are gone.

use super::client_ip::{rate_key, TrustedProxies};
use axum::extract::ConnectInfo;
use axum::Router;
use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpListener;
use tokio::sync::{watch, Semaphore};
use tokio::time::Sleep;

/// Transport bounds of one server (see the module docs).
#[derive(Clone, Debug)]
pub struct ConnLimits {
    /// Longest wait for a complete request head (also the keep-alive idle limit between requests).
    pub header_timeout: Duration,
    /// Longest a write may make no progress; zero disables the check.
    pub write_timeout: Duration,
    /// Open connections at once.
    pub max_connections: usize,
    /// Open connections per client address (IPv6: per `ipv6_prefix_bits` prefix); zero disables the per-address cap.
    pub max_per_ip: usize,
    pub ipv6_prefix_bits: u8,
    /// Peers exempt from the per-address cap (the operator's reverse proxies).
    pub exempt: TrustedProxies,
}

/// Answer of a connection over a cap (written without blocking, best effort, then the socket is closed).
const OVER_CAP: &[u8] = b"HTTP/1.1 503 Service Unavailable\r\nretry-after: 1\r\nconnection: close\r\ncontent-length: 0\r\n\r\n";

#[derive(Default)]
struct PerIp(Mutex<HashMap<IpAddr, usize>>);

/// One counted connection of a client address; released on drop.
struct IpSlot {
    reg: Arc<PerIp>,
    key: IpAddr,
}

impl PerIp {
    fn try_acquire(self: &Arc<Self>, key: IpAddr, max: usize) -> Option<IpSlot> {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let n = m.entry(key).or_insert(0);
        if *n >= max {
            if *n == 0 {
                m.remove(&key);
            }
            return None;
        }
        *n += 1;
        Some(IpSlot { reg: self.clone(), key })
    }

    #[cfg(test)]
    fn open(&self, key: IpAddr) -> usize {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).get(&key).copied().unwrap_or(0)
    }
}

impl Drop for IpSlot {
    fn drop(&mut self) {
        let mut m = self.reg.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = m.get_mut(&self.key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                m.remove(&self.key);
            }
        }
    }
}

/// Serves `app` (HTTP/1, upgrades allowed) on `listener` until `shutdown` resolves, under `limits`. Every request carries
/// the peer address as `ConnectInfo<SocketAddr>`. `name` prefixes the connection-level log lines.
pub async fn serve(
    listener: TcpListener,
    app: Router,
    limits: ConnLimits,
    name: &'static str,
    shutdown: impl Future<Output = ()>,
) -> io::Result<()> {
    use hyper_util::rt::{TokioIo, TokioTimer};
    use hyper_util::service::TowerToHyperService;
    let max = limits.max_connections.max(1);
    let slots = Arc::new(Semaphore::new(max));
    let per_ip = Arc::new(PerIp::default());
    let (stop_tx, stop_rx) = watch::channel(false);
    tokio::pin!(shutdown);
    loop {
        let (stream, peer) = tokio::select! {
            r = listener.accept() => match r {
                Ok(x) => x,
                Err(e) => {
                    tracing::warn!("{name}: accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
            _ = &mut shutdown => break,
        };
        let ip = super::client_ip::unmap(peer.ip());
        let ip_slot = if limits.max_per_ip > 0 && !limits.exempt.is_trusted(ip) {
            match per_ip.try_acquire(rate_key(ip, limits.ipv6_prefix_bits), limits.max_per_ip) {
                Some(s) => Some(s),
                None => {
                    tracing::debug!("{name}: {peer} over the per-address connection cap");
                    let _ = stream.try_write(OVER_CAP);
                    continue;
                }
            }
        } else {
            None
        };
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            tracing::debug!("{name}: {peer} over the connection cap");
            let _ = stream.try_write(OVER_CAP);
            continue;
        };
        let svc = TowerToHyperService::new(app.clone().layer(axum::Extension(ConnectInfo(peer))));
        let mut stop = stop_rx.clone();
        let (header_timeout, write_timeout) = (limits.header_timeout, limits.write_timeout);
        tokio::spawn(async move {
            let _permit = permit;
            let _ip_slot = ip_slot;
            let io = TokioIo::new(WriteStall::new(stream, write_timeout));
            let mut b = hyper::server::conn::http1::Builder::new();
            b.timer(TokioTimer::new()).header_read_timeout(header_timeout);
            let conn = b.serve_connection(io, svc).with_upgrades();
            tokio::pin!(conn);
            let r = tokio::select! {
                r = conn.as_mut() => r,
                _ = async { stop.wait_for(|s| *s).await.map(|_| ()) } => {
                    conn.as_mut().graceful_shutdown();
                    conn.await
                }
            };
            if let Err(e) = r {
                if !e.is_incomplete_message() {
                    tracing::debug!("{name}: connection from {peer}: {e}");
                }
            }
        });
    }
    drop(listener);
    let _ = stop_tx.send(true);
    // every connection task holds one permit: all of them back = every connection closed
    let _ = slots.acquire_many(u32::try_from(max).unwrap_or(u32::MAX)).await;
    Ok(())
}

/// A stream whose writes fail with `TimedOut` once one of them has made no progress for `timeout` (the peer stopped
/// reading). Reads are untouched (the header deadline and the handlers' own deadlines bound them).
pub struct WriteStall<S> {
    inner: S,
    timeout: Duration,
    stalled: Option<Pin<Box<Sleep>>>,
}

impl<S> WriteStall<S> {
    pub fn new(inner: S, timeout: Duration) -> Self {
        WriteStall { inner, timeout, stalled: None }
    }

    /// `Pending` from the inner write: start (or keep) the stall timer; its expiry turns the write into an error.
    fn on_pending<T>(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<T>> {
        if self.timeout.is_zero() {
            return Poll::Pending;
        }
        let t = self.timeout;
        let s = self.stalled.get_or_insert_with(|| Box::pin(tokio::time::sleep(t)));
        if s.as_mut().poll(cx).is_ready() {
            self.stalled = None;
            Poll::Ready(Err(io::Error::new(io::ErrorKind::TimedOut, "write stalled: the peer stopped reading")))
        } else {
            Poll::Pending
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for WriteStall<S> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for WriteStall<S> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        match Pin::new(&mut self.inner).poll_write(cx, buf) {
            Poll::Ready(r) => {
                self.stalled = None;
                Poll::Ready(r)
            }
            Poll::Pending => self.on_pending(cx),
        }
    }

    fn poll_write_vectored(mut self: Pin<&mut Self>, cx: &mut Context<'_>, bufs: &[io::IoSlice<'_>]) -> Poll<io::Result<usize>> {
        match Pin::new(&mut self.inner).poll_write_vectored(cx, bufs) {
            Poll::Ready(r) => {
                self.stalled = None;
                Poll::Ready(r)
            }
            Poll::Pending => self.on_pending(cx),
        }
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match Pin::new(&mut self.inner).poll_flush(cx) {
            Poll::Ready(r) => {
                self.stalled = None;
                Poll::Ready(r)
            }
            Poll::Pending => self.on_pending(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match Pin::new(&mut self.inner).poll_shutdown(cx) {
            Poll::Ready(r) => {
                self.stalled = None;
                Poll::Ready(r)
            }
            Poll::Pending => self.on_pending(cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_address_slots_are_counted_and_released() {
        let reg = Arc::new(PerIp::default());
        let a: IpAddr = "10.0.0.1".parse().unwrap();
        let s1 = reg.try_acquire(a, 2).unwrap();
        let s2 = reg.try_acquire(a, 2).unwrap();
        assert!(reg.try_acquire(a, 2).is_none());
        assert_eq!(reg.open(a), 2);
        drop(s1);
        let s3 = reg.try_acquire(a, 2).unwrap();
        drop((s2, s3));
        assert_eq!(reg.open(a), 0);
        assert!(reg.0.lock().unwrap().is_empty(), "no entry is left behind");
    }
}
