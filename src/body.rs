//! Provides types for working with request and response bodies.

use crate::io::Text;
use crate::task::Join;
use bytes::Bytes;
use futures_io::AsyncRead;
use futures_util::io::AsyncReadExt;
use std::fmt;
use std::io::{self, Cursor, Read};
use std::pin::Pin;
use std::str;
use std::task::{Context, Poll};

/// Contains the body of an HTTP request or response.
///
/// This type is used to encapsulate the underlying stream or region of memory
/// where the contents of the body are stored. A `Body` can be created from many
/// types of sources using the [`Into`](std::convert::Into) trait or one of its
/// constructor functions.
///
/// Since the entire request life-cycle in Isahc is asynchronous, bodies must
/// also be asynchronous. You can create a body from anything that implements
/// [`AsyncRead`], which [`Body`] itself also implements.
pub struct Body<'b>(Inner<'b>);

/// All possible body implementations.
enum Inner<'b> {
    /// An empty body.
    Empty,

    Borrowed(Cursor<&'b [u8]>),

    /// A body stored in memory.
    Bytes(Cursor<Bytes>),

    /// An asynchronous reader.
    AsyncRead(Pin<Box<dyn AsyncRead + Send>>, Option<u64>),
}

impl Body<'static> {
    /// Create a new body from bytes stored in memory.
    ///
    /// The body will have a known length equal to the number of bytes given.
    pub fn bytes(bytes: impl Into<Bytes>) -> Self {
        Body(Inner::Bytes(Cursor::new(bytes.into())))
    }

    /// Create a streaming body that reads from the given reader.
    ///
    /// The body will have an unknown length. When used as a request body,
    /// chunked transfer encoding might be used to send the request.
    pub fn reader(read: impl AsyncRead + Send + 'static) -> Self {
        Body(Inner::AsyncRead(Box::pin(read), None))
    }

    /// Create a streaming body with a known length.
    ///
    /// If the size of the body is known in advance, such as with a file, then
    /// this function can be used to create a body that can determine its
    /// `Content-Length` while still reading the bytes asynchronously.
    ///
    /// Giving a value for `length` that doesn't actually match how much data
    /// the reader will produce may result in errors when sending the body in a
    /// request.
    pub fn reader_sized(read: impl AsyncRead + Send + 'static, length: u64) -> Self {
        Body(Inner::AsyncRead(Box::pin(read), Some(length)))
    }
}

impl<'b> Body<'b> {
    /// Create a new empty body.
    ///
    /// An empty body will have a known length of 0 bytes.
    pub const fn empty() -> Self {
        Body(Inner::Empty)
    }

    /// Report if this body is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == Some(0)
    }

    /// Get the size of the body, if known.
    ///
    /// The value reported by this method is used to set the `Content-Length`
    /// for outgoing requests.
    ///
    /// When coming from a response, this method will report the value of the
    /// `Content-Length` response header if present. If this method returns
    /// `None` then there's a good chance that the server used something like
    /// chunked transfer encoding to send the response body.
    ///
    /// Since the length may be determined totally separately from the actual
    /// bytes, even if a value is returned it should not be relied on as always
    /// being accurate, and should be treated as a "hint".
    pub fn len(&self) -> Option<u64> {
        match &self.0 {
            Inner::Empty => Some(0),
            Inner::Bytes(bytes) => Some(bytes.get_ref().len() as u64),
            Inner::AsyncRead(_, len) => *len,
        }
    }

    /// If this body is repeatable, reset the body stream back to the start of
    /// the content. Returns `false` if the body cannot be reset.
    pub fn reset(&mut self) -> bool {
        match &mut self.0 {
            Inner::Empty => true,
            Inner::Bytes(cursor) => {
                cursor.set_position(0);
                true
            }
            Inner::AsyncRead(_, _) => false,
        }
    }

    /// Get the response body as a string.
    ///
    /// If the body comes from a stream, the steam bytes will be consumed and
    /// this method will return an empty string next call. If this body supports
    /// seeking, you can seek to the beginning of the body if you need to call
    /// this method again later.
    pub fn text(&mut self) -> Result<String, io::Error> {
        let mut s = String::default();
        Read::read_to_string(self, &mut s)?;
        Ok(s)
    }

    /// Get the response body as a string asynchronously.
    ///
    /// If the body comes from a stream, the steam bytes will be consumed and
    /// this method will return an empty string next call. If this body supports
    /// seeking, you can seek to the beginning of the body if you need to call
    /// this method again later.
    pub fn text_async(&mut self) -> Text<'_, Body<'b>> {
        Text::new(self)
    }

    /// Deserialize the response body as JSON into a given type.
    ///
    /// This method requires the `json` feature to be enabled.
    #[cfg(feature = "json")]
    pub fn json<T: serde::de::DeserializeOwned>(&mut self) -> Result<T, serde_json::Error> {
        serde_json::from_reader(self)
    }
}

impl<'b> Read for Body<'b> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        AsyncReadExt::read(self, buf).join()
    }
}

impl<'b> AsyncRead for Body<'b> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        match &mut self.0 {
            Inner::Empty => Poll::Ready(Ok(0)),
            Inner::Bytes(cursor) => AsyncRead::poll_read(Pin::new(cursor), cx, buf),
            Inner::AsyncRead(read, _) => AsyncRead::poll_read(read.as_mut(), cx, buf),
        }
    }
}

impl<'b> Default for Body<'b> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<'b> From<()> for Body<'b> {
    fn from(_: ()) -> Self {
        Self::empty()
    }
}

impl From<Vec<u8>> for Body<'static> {
    fn from(body: Vec<u8>) -> Self {
        Self::bytes(body)
    }
}

impl<'b> From<&'b [u8]> for Body<'b> {
    fn from(body: &'b [u8]) -> Self {
        Body(Inner::Borrowed(Cursor::new(body)))
    }
}

impl From<Bytes> for Body<'static> {
    fn from(body: Bytes) -> Self {
        Self::bytes(body)
    }
}

impl From<String> for Body<'static> {
    fn from(body: String) -> Self {
        body.into_bytes().into()
    }
}

impl<'b> From<&'b str> for Body<'b> {
    fn from(body: &'b str) -> Self {
        body.as_bytes().into()
    }
}

impl<'b, T: Into<Body<'b>>> From<Option<T>> for Body<'b> {
    fn from(body: Option<T>) -> Self {
        match body {
            Some(body) => body.into(),
            None => Self::default(),
        }
    }
}

impl<'b> fmt::Debug for Body<'b> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.len() {
            Some(len) => write!(f, "Body({})", len),
            None => write!(f, "Body(?)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_send<T: Send>() {}

    #[test]
    fn traits() {
        is_send::<Body>();
    }
}
