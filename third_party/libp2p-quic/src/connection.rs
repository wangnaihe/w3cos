// Copyright 2020 Parity Technologies (UK) Ltd.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the "Software"),
// to deal in the Software without restriction, including without limitation
// the rights to use, copy, modify, merge, publish, distribute, sublicense,
// and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

mod connecting;
mod stream;

use std::{
    future::Future,
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

pub use connecting::Connecting;
use futures::{FutureExt, future::BoxFuture};
use libp2p_core::{
    Multiaddr,
    multiaddr::Protocol,
    muxing::{StreamMuxer, StreamMuxerEvent},
};
pub use stream::Stream;

use crate::{ConnectionError, Error};

/// State for a single opened QUIC connection.
pub struct Connection {
    /// Underlying connection.
    connection: quinn::Connection,
    /// Future for accepting a new incoming bidirectional stream.
    incoming: Option<
        BoxFuture<'static, Result<(quinn::SendStream, quinn::RecvStream), quinn::ConnectionError>>,
    >,
    /// Future for opening a new outgoing bidirectional stream.
    outgoing: Option<
        BoxFuture<'static, Result<(quinn::SendStream, quinn::RecvStream), quinn::ConnectionError>>,
    >,
    /// Future to wait for the connection to be closed.
    closing: Option<BoxFuture<'static, quinn::ConnectionError>>,
    /// Last remote address observed on this connection.
    last_remote: Option<SocketAddr>,
    /// Wakes `poll` so a migrated path is noticed without other stream traffic.
    watch: Option<Pin<Box<futures_timer::Delay>>>,
}

impl Connection {
    /// Build a [`Connection`] from raw components.
    ///
    /// This function assumes that the [`quinn::Connection`] is completely fresh and none of
    /// its methods has ever been called. Failure to comply might lead to logic errors and panics.
    fn new(connection: quinn::Connection) -> Self {
        Self {
            connection,
            incoming: None,
            outgoing: None,
            closing: None,
            last_remote: None,
            watch: None,
        }
    }
}

fn quic_v1_addr(socket_addr: SocketAddr) -> Multiaddr {
    Multiaddr::empty()
        .with(socket_addr.ip().into())
        .with(Protocol::Udp(socket_addr.port()))
        .with(Protocol::QuicV1)
}

impl StreamMuxer for Connection {
    type Substream = Stream;
    type Error = Error;

    fn poll_inbound(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::Substream, Self::Error>> {
        let this = self.get_mut();

        let incoming = this.incoming.get_or_insert_with(|| {
            let connection = this.connection.clone();
            async move { connection.accept_bi().await }.boxed()
        });

        let (send, recv) = futures::ready!(incoming.poll_unpin(cx)).map_err(ConnectionError)?;
        this.incoming.take();
        let stream = Stream::new(send, recv);
        Poll::Ready(Ok(stream))
    }

    fn poll_outbound(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::Substream, Self::Error>> {
        let this = self.get_mut();

        let outgoing = this.outgoing.get_or_insert_with(|| {
            let connection = this.connection.clone();
            async move { connection.open_bi().await }.boxed()
        });

        let (send, recv) = futures::ready!(outgoing.poll_unpin(cx)).map_err(ConnectionError)?;
        this.outgoing.take();
        let stream = Stream::new(send, recv);
        Poll::Ready(Ok(stream))
    }

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<StreamMuxerEvent, Self::Error>> {
        let this = self.get_mut();
        let remote = this.connection.remote_address();
        if let Some(previous) = this.last_remote
            && previous != remote
        {
            this.last_remote = Some(remote);
            return Poll::Ready(Ok(StreamMuxerEvent::AddressChange(quic_v1_addr(remote))));
        }
        this.last_remote = Some(remote);
        if this.watch.is_none() {
            this.watch = Some(Box::pin(futures_timer::Delay::new(Duration::from_secs(1))));
        }
        if let Some(watch) = this.watch.as_mut()
            && watch.as_mut().poll(cx).is_ready()
        {
            this.watch = None;
            cx.waker().wake_by_ref();
        }
        Poll::Pending
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();

        let closing = this.closing.get_or_insert_with(|| {
            this.connection.close(From::from(0u32), &[]);
            let connection = this.connection.clone();
            async move { connection.closed().await }.boxed()
        });

        match futures::ready!(closing.poll_unpin(cx)) {
            // Expected error given that `connection.close` was called above.
            quinn::ConnectionError::LocallyClosed => {}
            error => return Poll::Ready(Err(Error::Connection(ConnectionError(error)))),
        };

        Poll::Ready(Ok(()))
    }
}
