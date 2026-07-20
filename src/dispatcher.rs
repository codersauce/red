//! Synchronous request and response channels used by the Husk host bridge.
//!
//! [`Dispatcher`] exposes independently clonable channel endpoints behind a shared
//! receiver lock. The current global plugin dispatcher assumes its channels remain
//! connected for the process lifetime; send, blocking receive, and poisoned-lock
//! failures therefore panic rather than representing recoverable plugin errors.

use std::sync::{mpsc, Arc, Mutex};

#[allow(unused)]
/// Paired synchronous channels for host requests and their responses.
pub struct Dispatcher<T, U> {
    request_tx: mpsc::Sender<T>,
    request_rx: Arc<Mutex<mpsc::Receiver<T>>>,
    response_tx: mpsc::Sender<U>,
    response_rx: Arc<Mutex<mpsc::Receiver<U>>>,
}

#[allow(unused)]
impl<T, U> Default for Dispatcher<T, U> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, U> Dispatcher<T, U> {
    /// Creates empty request and response channels.
    pub fn new() -> Self {
        let (request_tx, request_rx) = mpsc::channel();
        let (response_tx, response_rx) = mpsc::channel();

        Self {
            request_tx,
            request_rx: Arc::new(Mutex::new(request_rx)),
            response_tx,
            response_rx: Arc::new(Mutex::new(response_rx)),
        }
    }

    /// Enqueues a request.
    ///
    /// The process-lifetime dispatcher treats a disconnected receiver as a programming
    /// error and panics.
    pub fn send_request(&self, request: T) {
        self.request_tx.send(request).unwrap();
    }

    /// Blocks until the next request arrives.
    ///
    /// A disconnected channel or poisoned receiver lock panics.
    pub fn recv_request(&self) -> T {
        self.request_rx.lock().unwrap().recv().unwrap()
    }

    /// Returns the next request without blocking.
    ///
    /// Empty and disconnected channels both return `None`; a poisoned lock panics.
    pub fn try_recv_request(&self) -> Option<T> {
        self.request_rx.lock().unwrap().try_recv().ok()
    }

    /// Enqueues a response, panicking if its receiver has disconnected.
    pub fn send_response(&self, response: U) {
        self.response_tx.send(response).unwrap();
    }

    /// Blocks until the next response arrives.
    ///
    /// A disconnected channel or poisoned receiver lock panics.
    pub fn recv_response(&self) -> U {
        self.response_rx.lock().unwrap().recv().unwrap()
    }
}
