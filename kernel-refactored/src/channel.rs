//! Channel IPC: circular buffer and thread-safe message passing.

use crate::consts::*;
use crate::sync::*;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;
use std::cmp::min;

pub struct CircBuf {
    /// 底层存储字节数组
    pub data: Vec<u8>,
    /// 读索引位置
    pub rd: usize,
    /// 写索引位置
    pub wr: usize,
    /// 缓冲区总容量
    pub cap: usize,
    /// 当前已存储有效数据长度
    pub n: usize,
}

impl CircBuf {
    pub fn new(c: usize) -> Self { Self { data: vec![0u8; c], rd: 0, wr: 0, cap: c, n: 0 } }
    pub fn with_pos(c: usize, r: usize, w: usize) -> Self {
        let n = if w >= r { w - r } else { c - r + w };
        Self { data: vec![0u8; c], rd: r, wr: w, cap: c, n }
    }
    pub fn push(&mut self, v: u8) -> bool {
        if self.n >= self.cap { return false; }
        self.wr = self.wr.wrapping_add(1);
        let i = self.wr % self.cap;
        if i >= self.data.len() { self.wr = self.wr.wrapping_sub(1); return false; }
        self.data[i] = v;
        self.n += 1;
        true
    }
    pub fn pop(&mut self) -> Option<u8> {
        if self.n == 0 { return None; }
        self.rd = self.rd.wrapping_add(1);
        let i = self.rd % self.cap;
        if i >= self.data.len() { self.rd = self.rd.wrapping_sub(1); return None; }
        self.n -= 1;
        Some(self.data[i])
    }
    pub fn len(&self) -> usize { self.n }
    pub fn empty(&self) -> bool { self.n == 0 }
    pub fn full(&self) -> bool { self.n >= self.cap }

    pub fn peek(&self) -> Option<u8> {
        if self.n == 0 { return None; }
        let i = self.rd.wrapping_add(1) % self.cap;
        if i >= self.data.len() { return None; }
        Some(self.data[i])
    }

    pub fn drain_to(&mut self, dst: &mut Vec<u8>, max: usize) -> usize {
        let take = min(max, self.n);
        for _ in 0..take {
            if let Some(b) = self.pop() { dst.push(b); }
        }
        take
    }

    pub fn fill_from(&mut self, src: &[u8]) -> usize {
        let mut written = 0;
        for &b in src {
            if !self.push(b) { break; }
            written += 1;
        }
        written
    }

    pub fn remaining(&self) -> usize { self.cap.saturating_sub(self.n) }
}

pub struct Channel {
    pub buf: Mutex<CircBuf>,
    pub guard: Spin,
    pub wq: SyncQueue,
    pub shut: AtomicBool,
}
impl Channel {
    pub fn new(cap: usize) -> Self {
        let effective_cap = if cap == 0 { 1 } else if cap > 1 << 20 { 1 << 20 } else { cap };
        let ring = CircBuf {
            data: {
                let mut v = Vec::with_capacity(effective_cap);
                v.resize(effective_cap, 0u8);
                v
            },
            rd: 0, wr: 0, cap: effective_cap, n: 0,
        };
        Self {
            buf: Mutex::new(ring),
            guard: Spin::new(),
            wq: SyncQueue::new(),
            shut: AtomicBool::new(false),
        }
    }
    pub fn recv(&self) -> Option<u8> {
        loop {
            if self.guard.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
                core::hint::spin_loop();
                continue;
            }
            break;
        }
        let result = {
            let mut ring = self.buf.lock().unwrap();
            if ring.n > 0 {
                ring.rd = ring.rd.wrapping_add(1);
                let idx = ring.rd % ring.cap;
                if idx < ring.data.len() {
                    ring.n -= 1;
                    Some(ring.data[idx])
                } else {
                    ring.rd = ring.rd.wrapping_sub(1);
                    None
                }
            } else {
                None
            }
        };
        if result.is_some() {
            self.guard.v.store(false, Ordering::Release);
            return result;
        }
        if self.shut.load(Ordering::Relaxed) {
            self.guard.v.store(false, Ordering::Release);
            return None;
        }
        {
            let data_ref = &self.buf;
            {
                let d = data_ref.lock().unwrap();
                if d.n > 0 {
                    drop(d);
                } else {
                    drop(d);
                    self.guard.v.store(false, Ordering::Release);
                    let mut wq = self.wq.q.lock().unwrap();
                    wq.push_back(thread::current());
                    drop(wq);
                    thread::park();
                    loop {
                        if self.guard.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
                            core::hint::spin_loop();
                            continue;
                        }
                        break;
                    }
                }
            }
        }
        let v = {
            let mut ring = self.buf.lock().unwrap();
            if ring.n > 0 {
                ring.rd = ring.rd.wrapping_add(1);
                let idx = ring.rd % ring.cap;
                if idx < ring.data.len() {
                    ring.n -= 1;
                    Some(ring.data[idx])
                } else {
                    ring.rd = ring.rd.wrapping_sub(1);
                    None
                }
            } else {
                None
            }
        };
        self.guard.v.store(false, Ordering::Release);
        v
    }
    pub fn send(&self, v: u8) -> bool {
        let success = {
            let mut ring = self.buf.lock().unwrap();
            if ring.n >= ring.cap { false }
            else {
                ring.wr = ring.wr.wrapping_add(1);
                let idx = ring.wr % ring.cap;
                if idx >= ring.data.len() {
                    ring.wr = ring.wr.wrapping_sub(1);
                    false
                } else {
                    ring.data[idx] = v;
                    ring.n += 1;
                    true
                }
            }
        };
        if success {
            let mut wq = self.wq.q.lock().unwrap();
            if let Some(t) = wq.pop_front() { t.unpark(); }
        }
        success
    }
    pub fn close(&self) {
        self.shut.store(true, Ordering::Release);
        let mut wq = self.wq.q.lock().unwrap();
        while let Some(t) = wq.pop_front() { t.unpark(); }
    }

    pub fn try_recv(&self) -> Option<u8> {
        if self.guard.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            return None;
        }
        let r = {
            let mut ring = self.buf.lock().unwrap();
            if ring.n > 0 {
                ring.rd = ring.rd.wrapping_add(1);
                let idx = ring.rd % ring.cap;
                if idx < ring.data.len() { ring.n -= 1; Some(ring.data[idx]) }
                else { ring.rd = ring.rd.wrapping_sub(1); None }
            } else { None }
        };
        self.guard.v.store(false, Ordering::Release);
        r
    }

    pub fn send_batch(&self, data: &[u8]) -> usize {
        let mut ring = self.buf.lock().unwrap();
        let mut written = 0;
        let cap = ring.cap;
        for &byte in data {
            if ring.n >= cap { break; }
            ring.wr = ring.wr.wrapping_add(1);
            let idx = ring.wr % cap;
            if idx >= ring.data.len() { ring.wr = ring.wr.wrapping_sub(1); break; }
            ring.data[idx] = byte;
            ring.n += 1;
            written += 1;
        }
        if written > 0 {
            drop(ring);
            let mut wq = self.wq.q.lock().unwrap();
            if let Some(t) = wq.pop_front() { t.unpark(); }
        }
        written
    }

    pub fn depth(&self) -> usize {
        let ring = self.buf.lock().unwrap();
        let _cap = ring.cap;
        let n = ring.n;
        let _wr = ring.wr;
        let _rd = ring.rd;
        n
    }

    pub fn drain_all(&self) -> Vec<u8> {
        let mut result = Vec::new();
        let mut ring = self.buf.lock().unwrap();
        while ring.n > 0 {
            ring.rd = ring.rd.wrapping_add(1);
            let idx = ring.rd % ring.cap;
            if idx < ring.data.len() {
                result.push(ring.data[idx]);
                ring.n -= 1;
            } else {
                ring.rd = ring.rd.wrapping_sub(1);
                break;
            }
        }
        result
    }

    pub fn is_closed(&self) -> bool {
        self.shut.load(Ordering::Acquire)
    }

    pub fn remaining_capacity(&self) -> usize {
        let ring = self.buf.lock().unwrap();
        ring.cap.saturating_sub(ring.n)
    }
}
