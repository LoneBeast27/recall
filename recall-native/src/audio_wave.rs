//! The WinMM wave-out device and its queued PCM buffers.
use std::mem::size_of;

use windows::core::PSTR;
use windows::Win32::Media::Audio::{
    waveOutClose, waveOutGetPosition, waveOutOpen, waveOutPause, waveOutPrepareHeader,
    waveOutReset, waveOutUnprepareHeader, waveOutWrite, CALLBACK_NULL, HWAVEOUT, WAVEFORMATEX,
    WAVEHDR, WAVE_FORMAT_PCM, WAVE_MAPPER, WHDR_DONE,
};
use windows::Win32::Media::{MMTIME, TIME_SAMPLES};

use super::{
    mm_ok, WaveBuffer, WaveDevice, BUFFER_COUNT, FRAMES_PER_BUFFER, OUTPUT_CHANNELS, SAMPLE_RATE,
};

impl WaveDevice {
    pub(super) fn open() -> Result<Self, String> {
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: OUTPUT_CHANNELS as u16,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * OUTPUT_CHANNELS as u32 * 2,
            nBlockAlign: (OUTPUT_CHANNELS * 2) as u16,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        let mut handle = HWAVEOUT::default();
        let code = unsafe {
            waveOutOpen(
                Some(&mut handle),
                WAVE_MAPPER,
                &format,
                None,
                None,
                CALLBACK_NULL,
            )
        };
        mm_ok(code, "open default wave output")?;
        mm_ok(unsafe { waveOutPause(handle) }, "pause new wave output")?;
        let buffers = (0..BUFFER_COUNT)
            .map(|_| WaveBuffer {
                samples: vec![0; FRAMES_PER_BUFFER * OUTPUT_CHANNELS],
                header: Box::new(WAVEHDR::default()),
                prepared: false,
            })
            .collect();
        Ok(Self { handle, buffers })
    }

    pub(super) fn reset(&mut self) -> Result<(), String> {
        mm_ok(unsafe { waveOutReset(self.handle) }, "reset wave output")?;
        for buffer in &mut self.buffers {
            buffer.unprepare(self.handle)?;
        }
        Ok(())
    }

    pub(super) fn played_frames(&self) -> Result<u32, String> {
        let mut time = MMTIME {
            wType: TIME_SAMPLES,
            ..Default::default()
        };
        mm_ok(
            unsafe { waveOutGetPosition(self.handle, &mut time, size_of::<MMTIME>() as u32) },
            "read wave output clock",
        )?;
        if time.wType != TIME_SAMPLES {
            return Err("wave output did not provide a sample clock".into());
        }
        Ok(unsafe { time.u.sample })
    }
}

impl Drop for WaveDevice {
    fn drop(&mut self) {
        let _ = unsafe { waveOutReset(self.handle) };
        for buffer in &mut self.buffers {
            let _ = buffer.unprepare(self.handle);
        }
        let _ = unsafe { waveOutClose(self.handle) };
    }
}

impl WaveBuffer {
    pub(super) fn done(&self) -> bool {
        !self.prepared
            || unsafe { std::ptr::read_volatile(std::ptr::addr_of!(self.header.dwFlags)) }
                & WHDR_DONE
                != 0
    }

    pub(super) fn submit(&mut self, handle: HWAVEOUT) -> Result<(), String> {
        self.header.lpData = PSTR(self.samples.as_mut_ptr().cast());
        self.header.dwBufferLength = (self.samples.len() * size_of::<i16>()) as u32;
        self.header.dwFlags = 0;
        mm_ok(
            unsafe {
                waveOutPrepareHeader(handle, self.header.as_mut(), size_of::<WAVEHDR>() as u32)
            },
            "prepare wave buffer",
        )?;
        self.prepared = true;
        if let Err(error) = mm_ok(
            unsafe { waveOutWrite(handle, self.header.as_mut(), size_of::<WAVEHDR>() as u32) },
            "queue wave buffer",
        ) {
            let _ = self.unprepare(handle);
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn unprepare(&mut self, handle: HWAVEOUT) -> Result<(), String> {
        if self.prepared {
            mm_ok(
                unsafe {
                    waveOutUnprepareHeader(
                        handle,
                        self.header.as_mut(),
                        size_of::<WAVEHDR>() as u32,
                    )
                },
                "unprepare wave buffer",
            )?;
            self.prepared = false;
        }
        Ok(())
    }
}
