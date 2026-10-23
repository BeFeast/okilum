//! Request-driven fixture lifetime, independent of application preparation time.
use std::{
    io,
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

pub struct Listener {
    socket: TcpListener,
    finished: Arc<AtomicBool>,
}
impl Listener {
    pub fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        let connection = self.socket.accept()?;
        if self.finished.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "fixture finished before an expected request arrived",
            ));
        }
        Ok(connection)
    }
}

pub struct Worker<T> {
    thread: Option<thread::JoinHandle<T>>,
    address: SocketAddr,
    // Reserve the endpoint until shutdown has finished, even if the worker exits.
    _socket: TcpListener,
    finished: Arc<AtomicBool>,
}
impl<T: Send + 'static> Worker<T> {
    pub fn spawn(socket: TcpListener, run: impl FnOnce(Listener) -> T + Send + 'static) -> Self {
        let address = socket.local_addr().unwrap();
        let socket_guard = socket.try_clone().unwrap();
        let finished = Arc::new(AtomicBool::new(false));
        let listener = Listener {
            socket,
            finished: finished.clone(),
        };
        Self {
            thread: Some(thread::spawn(move || run(listener))),
            address,
            _socket: socket_guard,
            finished,
        }
    }

    pub fn join(mut self) -> thread::Result<T> {
        // The client operation has returned. Any outstanding expected request is
        // now missing, so wake accept and report it rather than waiting forever.
        self.finish();
        self.thread.take().unwrap().join()
    }
}
impl<T> Worker<T> {
    fn finish(&self) {
        self.finished.store(true, Ordering::Release);
        // The retained socket reserves this endpoint until shutdown completes.
        // This connection carries no synthetic HTTP request.
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_secs(1));
    }
}
impl<T> Drop for Worker<T> {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.finish();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        sync::mpsc,
    };

    #[test]
    fn missing_request_is_reported_when_the_client_finishes() {
        let socket = TcpListener::bind("127.0.0.1:0").unwrap();
        let (ready, waiting) = mpsc::channel();
        let worker = Worker::spawn(socket, move |listener| {
            ready.send(()).unwrap();
            listener.accept().expect("expected fixture request");
        });
        waiting.recv().unwrap();
        let panic = worker.join().expect_err("missing request must fail");
        let message = panic.downcast_ref::<String>().unwrap();
        assert!(message.contains("fixture finished before an expected request arrived"));
    }

    #[test]
    fn a_real_request_is_served_before_join() {
        let socket = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = socket.local_addr().unwrap();
        let worker = Worker::spawn(socket, |listener| {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(b"served").unwrap();
        });
        let mut stream = TcpStream::connect(address).unwrap();
        let mut reply = [0; 6];
        stream.read_exact(&mut reply).unwrap();
        assert_eq!(&reply, b"served");
        worker.join().unwrap();
    }
}
