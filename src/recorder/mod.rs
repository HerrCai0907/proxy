use std::cmp::min;
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::io::{self, ReadBuf};

pub struct RecorderState {
    reader_length: usize,
    waker: Option<std::task::Waker>,
}

pub struct Recorder {
    buf: VecDeque<u8>,
    states: Vec<RecorderState>,
}

impl Recorder {
    pub fn new() -> Self {
        Self {
            buf: VecDeque::new(),
            states: vec![],
        }
    }
}

pub struct RecorderReader {
    index: usize,
    recorder: Arc<Mutex<Recorder>>,
}
impl RecorderReader {
    pub fn new(recorder: Arc<Mutex<Recorder>>) -> Self {
        let mut recorder_locked = recorder.lock().unwrap();
        let index = recorder_locked.buf.len();
        recorder_locked.states.push({
            RecorderState {
                reader_length: 0,
                waker: None,
            }
        });
        Self {
            index,
            recorder: recorder.clone(),
        }
    }
}

fn get_overlap(buf: &[u8], buf_offset: usize, begin: usize, size: usize) -> &[u8] {
    let end = begin + size;
    let begin = if begin > buf_offset {
        begin - buf_offset
    } else {
        0
    };
    let end = if end > buf_offset {
        end - buf_offset
    } else {
        0
    };
    let begin = if begin > buf.len() { buf.len() } else { begin };
    let end = if end > buf.len() { buf.len() } else { end };
    return &buf[begin..end];
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_get_overlap_basic() {
        let buf = [1, 2, 3, 4, 5];
        assert_eq!(get_overlap(&buf, 0, 1, 2), &[2, 3]);
    }
    #[test]
    fn test_get_overlap_with_offset() {
        let buf = [1, 2, 3, 4, 5];
        assert_eq!(get_overlap(&buf, 2, 3, 2), &[2, 3]);
    }
    #[test]
    fn test_get_overlap_out_of_bounds() {
        let buf = [1, 2, 3, 4, 5];
        assert_eq!(get_overlap(&buf, 0, 3, 7), &[4, 5]);
    }
    #[test]
    fn test_get_overlap_empty_buffer() {
        let buf: [u8; 0] = [];
        assert_eq!(get_overlap(&buf, 0, 0, 0), &[]);
    }
    #[test]
    fn test_get_overlap_zero_length() {
        let buf = [1, 2, 3, 4, 5];
        assert_eq!(get_overlap(&buf, 0, 2, 0), &[]);
    }
}

impl io::AsyncRead for RecorderReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut recorder = self.recorder.lock().unwrap();
        let state = &recorder.states[self.index];
        let recorder_buf = &recorder.buf;
        let n = recorder_buf.len() - state.reader_length;
        if n == 0 {
            recorder.states[self.index].waker = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let n = min(n, buf.remaining());
        let (front, back) = recorder_buf.as_slices();
        buf.put_slice(get_overlap(front, 0, state.reader_length, n));
        buf.put_slice(get_overlap(back, front.len(), state.reader_length, n));
        recorder.states[self.index].reader_length += n;

        println!(
            "poll_read: {}",
            recorder
                .buf
                .iter()
                .map(|&b| format!("{:02x}", b))
                .collect::<String>()
        );
        Poll::Ready(Ok(()))
    }
}

pub struct RecorderWriter {
    pub recorder: Arc<Mutex<Recorder>>,
}
impl io::AsyncWrite for RecorderWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let mut recorder = self.recorder.lock().unwrap();
        println!(
            "poll_write: {}",
            buf.iter()
                .map(|&b| format!("{:02x}", b))
                .collect::<String>()
        );
        if buf.len() == 0 {
            panic!("buf is empty");
        }
        recorder.buf.extend(buf);
        dbg!(recorder.buf.len());
        let wakers = recorder
            .states
            .iter_mut()
            .map(|state| state.waker.take())
            .collect::<Vec<_>>();
        let min = recorder
            .states
            .iter()
            .min_by_key(|state| state.reader_length)
            .map(|state| state.reader_length);
        if let Some(min) = min {
            recorder.buf.drain(0..min);
            for state in &mut recorder.states {
                state.reader_length -= min;
            }
        }
        drop(recorder);
        for waker in wakers {
            match waker {
                Some(waker) => waker.wake(),
                None => (),
            };
        }
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }
}
