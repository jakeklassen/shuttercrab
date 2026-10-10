//! Where each kept frame's time goes in the recorder, measured with GPU
//! timestamp queries and CPU wall time, for tuning its cost to a game.

use crate::{record::FrameTiming, sys::imp::gpu::Gpu};
use anyhow::{Context, Result};
use std::time::Duration;
use windows::Win32::Graphics::Direct3D11::*;

/// Timestamp queries for one frame: the disjoint query around four stamps
/// (before the copy, after it, after the conversion, after NV12).
struct FrameQueries {
    disjoint: ID3D11Query,
    stamps: [ID3D11Query; 4],
    pending: bool,
}

/// Collects [`FrameTiming`] without waiting on the GPU: each frame's
/// queries are read a few frames later, when they are surely done.
pub(super) struct Timer {
    ring: Vec<FrameQueries>,
    next: usize,
    sums: [f64; 3],
    gpu_frames: u64,
    cpu: [f64; 2],
    cpu_frames: u64,
}

impl Timer {
    const DEPTH: usize = 8;

    pub(super) fn new(gpu: &Gpu) -> Result<Self> {
        let query = |kind| -> Result<ID3D11Query> {
            let mut q = None;
            unsafe {
                gpu.device.CreateQuery(
                    &D3D11_QUERY_DESC {
                        Query: kind,
                        MiscFlags: 0,
                    },
                    Some(&mut q),
                )?
            };
            q.context("no query")
        };
        let ring = (0..Self::DEPTH)
            .map(|_| {
                Ok(FrameQueries {
                    disjoint: query(D3D11_QUERY_TIMESTAMP_DISJOINT)?,
                    stamps: [
                        query(D3D11_QUERY_TIMESTAMP)?,
                        query(D3D11_QUERY_TIMESTAMP)?,
                        query(D3D11_QUERY_TIMESTAMP)?,
                        query(D3D11_QUERY_TIMESTAMP)?,
                    ],
                    pending: false,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            ring,
            next: 0,
            sums: [0.0; 3],
            gpu_frames: 0,
            cpu: [0.0; 2],
            cpu_frames: 0,
        })
    }

    /// Read the slot's results if the GPU has them; forget them if not.
    fn collect(&mut self, context: &ID3D11DeviceContext, index: usize, wait: bool) {
        let slot = &mut self.ring[index];
        if !std::mem::take(&mut slot.pending) {
            return;
        }
        let flags = if wait {
            0
        } else {
            D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32
        };
        let read_disjoint = || {
            let mut data = D3D11_QUERY_DATA_TIMESTAMP_DISJOINT::default();
            let ok = unsafe {
                context.GetData(
                    &slot.disjoint,
                    Some((&mut data as *mut D3D11_QUERY_DATA_TIMESTAMP_DISJOINT).cast()),
                    size_of::<D3D11_QUERY_DATA_TIMESTAMP_DISJOINT>() as u32,
                    flags,
                )
            };
            (ok.is_ok() && data.Frequency != 0).then_some(data)
        };
        let mut data = read_disjoint();
        if wait {
            for _ in 0..50 {
                if data.is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
                data = read_disjoint();
            }
        }
        let Some(data) = data.filter(|d| !d.Disjoint.as_bool()) else {
            return;
        };
        let mut ticks = [0u64; 4];
        for (stamp, tick) in slot.stamps.iter().zip(ticks.iter_mut()) {
            let ok = unsafe {
                context.GetData(
                    stamp,
                    Some((tick as *mut u64).cast()),
                    size_of::<u64>() as u32,
                    flags,
                )
            };
            if ok.is_err() || *tick == 0 {
                return;
            }
        }
        let ms = |a: u64, b: u64| b.saturating_sub(a) as f64 * 1000.0 / data.Frequency as f64;
        self.sums[0] += ms(ticks[0], ticks[1]);
        self.sums[1] += ms(ticks[1], ticks[2]);
        self.sums[2] += ms(ticks[2], ticks[3]);
        self.gpu_frames += 1;
    }

    /// Start a frame: reuse the oldest slot, reading it first.
    pub(super) fn begin(&mut self, context: &ID3D11DeviceContext) -> usize {
        let index = self.next;
        self.next = (self.next + 1) % Self::DEPTH;
        self.collect(context, index, false);
        let slot = &mut self.ring[index];
        unsafe {
            context.Begin(&slot.disjoint);
            context.End(&slot.stamps[0]);
        }
        index
    }

    pub(super) fn stamp(&self, context: &ID3D11DeviceContext, index: usize, which: usize) {
        unsafe { context.End(&self.ring[index].stamps[which]) };
    }

    pub(super) fn end(
        &mut self,
        context: &ID3D11DeviceContext,
        index: usize,
        cpu_convert: Duration,
        cpu_write: Duration,
    ) {
        let slot = &mut self.ring[index];
        unsafe { context.End(&slot.disjoint) };
        slot.pending = true;
        self.cpu[0] += cpu_convert.as_secs_f64() * 1000.0;
        self.cpu[1] += cpu_write.as_secs_f64() * 1000.0;
        self.cpu_frames += 1;
    }

    pub(super) fn finish(mut self, context: &ID3D11DeviceContext) -> FrameTiming {
        for index in 0..Self::DEPTH {
            self.collect(context, index, true);
        }
        let per = |sum: f64, n: u64| if n == 0 { 0.0 } else { sum / n as f64 };
        FrameTiming {
            gpu_frames: self.gpu_frames,
            copy_ms: per(self.sums[0], self.gpu_frames),
            convert_ms: per(self.sums[1], self.gpu_frames),
            nv12_ms: per(self.sums[2], self.gpu_frames),
            cpu_frames: self.cpu_frames,
            cpu_convert_ms: per(self.cpu[0], self.cpu_frames),
            cpu_write_ms: per(self.cpu[1], self.cpu_frames),
        }
    }
}
