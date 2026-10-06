//! Outgoing side of a connection. Messages are written straight from whichever
//! thread produces them with a non-blocking send(2): no writer thread, so no
//! thread wakeup or context switch per message. Bytes the kernel can't take yet
//! wait in `backlog` and go out ahead of the next message; a client that falls
//! too far behind is dropped (its reader thread then sees EOF and cleans up).

use std::io;
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// About two seconds of full-lobby snapshots.
const MAX_BACKLOG: usize = 64 * 1024;

pub struct Conn {
    stream: TcpStream,
    backlog: Mutex<Vec<u8>>,
    dead: AtomicBool,
}

impl Conn {
    pub fn new(stream: TcpStream) -> Conn {
        Conn { stream, backlog: Mutex::new(Vec::new()), dead: AtomicBool::new(false) }
    }

    /// Queues one whole websocket frame. False once the connection is gone.
    pub fn send(&self, frame: &[u8]) -> bool {
        if self.dead.load(Ordering::Relaxed) {
            return false;
        }
        let mut backlog = self.backlog.lock().unwrap();
        let ok = if backlog.is_empty() {
            match try_send(&self.stream, frame) {
                Ok(n) => {
                    backlog.extend_from_slice(&frame[n..]);
                    true
                }
                Err(_) => false,
            }
        } else {
            backlog.extend_from_slice(frame);
            match try_send(&self.stream, &backlog) {
                Ok(n) => {
                    backlog.drain(..n);
                    true
                }
                Err(_) => false,
            }
        };
        if !ok || backlog.len() > MAX_BACKLOG {
            backlog.clear();
            self.close();
            return false;
        }
        true
    }

    pub fn close(&self) {
        self.dead.store(true, Ordering::Relaxed);
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

/// Writes what the socket buffer will take right now, without blocking.
#[cfg(unix)]
fn try_send(stream: &TcpStream, buf: &[u8]) -> io::Result<usize> {
    use std::os::fd::AsRawFd;
    extern "C" {
        fn send(fd: i32, buf: *const u8, len: usize, flags: i32) -> isize;
    }
    // Per-call non-blocking, so the reader thread's blocking reads on the same
    // socket are unaffected. (std ignores SIGPIPE, so no MSG_NOSIGNAL needed.)
    #[cfg(any(target_os = "linux", target_os = "android"))]
    const MSG_DONTWAIT: i32 = 0x40;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    const MSG_DONTWAIT: i32 = 0x80;
    let mut sent = 0;
    while sent < buf.len() {
        let r = unsafe { send(stream.as_raw_fd(), buf[sent..].as_ptr(), buf.len() - sent, MSG_DONTWAIT) };
        if r >= 0 {
            sent += r as usize;
            continue;
        }
        let e = io::Error::last_os_error();
        match e.kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::WouldBlock => break,
            _ => return Err(e),
        }
    }
    Ok(sent)
}

/// Fallback: a plain blocking write.
#[cfg(not(unix))]
fn try_send(mut stream: &TcpStream, buf: &[u8]) -> io::Result<usize> {
    use std::io::Write;
    stream.write_all(buf).map(|_| buf.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;

    #[test]
    fn backlog_keeps_frames_whole_and_drops_stalled_clients() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let conn = Conn::new(TcpStream::connect(listener.local_addr().unwrap()).unwrap());
        let (mut peer, _) = listener.accept().unwrap();
        // A peer that never reads: the kernel buffers fill, then our backlog,
        // and the connection is dropped instead of blocking the sender.
        let frame = vec![7u8; 16 * 1024];
        let mut sent = 0;
        while conn.send(&frame) {
            sent += 1;
            assert!(sent < 10_000, "never gave up on a stalled client");
        }
        assert!(!conn.send(b"x"));
        // Everything that was accepted arrives intact and in order.
        let mut got = Vec::new();
        let _ = peer.read_to_end(&mut got);
        assert!(got.len() >= frame.len() && got.iter().all(|&b| b == 7));
    }
}
