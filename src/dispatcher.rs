use std::sync::{mpsc, Arc, Mutex};

#[allow(unused)]
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

    pub fn send_request(&self, request: T) {
        self.request_tx.send(request).unwrap();
    }

    pub fn recv_request(&self) -> T {
        self.request_rx.lock().unwrap().recv().unwrap()
    }

    pub fn try_recv_request(&self) -> Option<T> {
        self.request_rx.lock().unwrap().try_recv().ok()
    }

    pub fn send_response(&self, response: U) {
        self.response_tx.send(response).unwrap();
    }

    pub fn recv_response(&self) -> U {
        self.response_rx.lock().unwrap().recv().unwrap()
    }
}
