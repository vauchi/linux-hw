// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use super::formats;
use crate::{
    CameraFrames, LumaFrame,
    camera_frame::{CameraFrame, PixelFormat},
};
use pipewire::{self as pw, properties::properties, spa};
use pw::stream::{StreamFlags, StreamListener, StreamRc, StreamState};
use spa::{
    param::{
        ParamType,
        video::{VideoFormat, VideoInfoRaw},
    },
    pod::Pod,
    utils::Direction,
};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Default)]
struct Latest {
    frame: Option<CameraFrame>,
    generation: u64,
    error: Option<String>,
}

pub(super) struct Capture {
    publication: super::publication::Publication,
    _input_events: StreamListener<VideoInfoRaw>,
    _output_events: StreamListener<()>,
    _input: StreamRc,
    output: StreamRc,
    mainloop: pw::main_loop::MainLoopRc,
    core: pw::core::CoreRc,
    latest: Rc<RefCell<Latest>>,
    delivered: u64,
    cancelled: Arc<AtomicBool>,
}

impl Capture {
    pub(super) fn new(
        mainloop: pw::main_loop::MainLoopRc,
        core: pw::core::CoreRc,
        target: String,
        name: String,
        cancelled: Arc<AtomicBool>,
        publication: super::publication::Publication,
    ) -> Result<Self, String> {
        let latest = Rc::new(RefCell::new(Latest::default()));
        let input = StreamRc::new(
            core.clone(),
            "Vauchi camera capture",
            properties! {
                "media.type" => "Video", "media.category" => "Capture", "media.role" => "Camera",
                "target.object" => target,
            },
        )
        .map_err(|_| "camera capture unavailable")?;
        let input_events = capture_events(&input, latest.clone())?;
        let input_format = formats::video(false)?;
        input
            .connect(
                Direction::Input,
                None,
                StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::DONT_RECONNECT,
                &mut [Pod::from_bytes(&input_format).ok_or("invalid camera format")?],
            )
            .map_err(|_| "camera capture could not start")?;
        let output = StreamRc::new(
            core.clone(),
            "Vauchi camera preview",
            properties! {
                "media.type" => "Video", "media.category" => "Capture", "media.role" => "Camera",
                "media.class" => "Video/Source", "node.name" => name,
            },
        )
        .map_err(|_| "camera preview unavailable")?;
        let output_events = preview_events(&output, latest.clone())?;
        let output_format = formats::video(true)?;
        output
            .connect(
                Direction::Output,
                None,
                StreamFlags::DRIVER | StreamFlags::MAP_BUFFERS,
                &mut [Pod::from_bytes(&output_format).ok_or("invalid camera preview format")?],
            )
            .map_err(|_| "camera preview could not start")?;
        Ok(Self {
            publication,
            _input_events: input_events,
            _output_events: output_events,
            _input: input,
            output,
            mainloop,
            core,
            latest,
            delivered: 0,
            cancelled,
        })
    }
}

impl CameraFrames for Capture {
    fn next_luma(&mut self) -> Result<Option<LumaFrame>, String> {
        let waiting = Instant::now();
        while !self.cancelled.load(Ordering::SeqCst) {
            self.mainloop
                .loop_()
                .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(25)));
            self.publication.update(&self.core, &self.output)?;
            let latest = self.latest.borrow();
            if let Some(error) = &latest.error {
                return Err(error.clone());
            }
            if latest.generation != self.delivered && self.publication.ready() {
                self.delivered = latest.generation;
                if matches!(self.output.state(), StreamState::Streaming) {
                    self.output
                        .trigger_process()
                        .map_err(|_| "camera preview delivery failed")?;
                }
                if let Some(frame) = &latest.frame {
                    return Ok(Some(LumaFrame {
                        luma: frame.luma(),
                        width: frame.width(),
                        height: frame.height(),
                    }));
                }
            }
            if waiting.elapsed() > Duration::from_secs(5) {
                return Err("camera stopped delivering frames".into());
            }
        }
        Ok(None)
    }
}

fn capture_events(
    stream: &StreamRc,
    latest: Rc<RefCell<Latest>>,
) -> Result<StreamListener<VideoInfoRaw>, String> {
    let state = latest.clone();
    let params = latest.clone();
    stream
        .add_local_listener_with_user_data(VideoInfoRaw::default())
        .state_changed(move |_, _, _, next| {
            if matches!(next, StreamState::Error(_)) {
                state.borrow_mut().error = Some(
                    "camera permission denied, device unavailable or unsupported format".into(),
                );
            }
        })
        .param_changed(move |stream, format, id, param| {
            if id != ParamType::Format.as_raw() {
                return;
            }
            let Some(param) = param else {
                return;
            };
            let result = format
                .parse(param)
                .map_err(|_| "unsupported camera format".to_string())
                .and_then(|_| {
                    let bytes = match format.format() {
                        VideoFormat::YUY2 => 2,
                        VideoFormat::RGB => 3,
                        _ => 4,
                    };
                    formats::buffers(stream, bytes)
                });
            if let Err(error) = result {
                params.borrow_mut().error = Some(error);
            }
        })
        .process(move |stream, format| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            if data.chunk().size() == 0 {
                return;
            }
            match read_frame(data, format) {
                Ok(frame) => {
                    let mut latest = latest.borrow_mut();
                    latest.frame = Some(frame);
                    latest.generation += 1;
                }
                Err(error) => latest.borrow_mut().error = Some(error),
            }
        })
        .register()
        .map_err(|_| "camera capture listener unavailable".into())
}

fn read_frame(data: &mut spa::buffer::Data, format: &VideoInfoRaw) -> Result<CameraFrame, String> {
    let pixel_format = match format.format() {
        VideoFormat::YUY2 => PixelFormat::Yuy2,
        VideoFormat::RGB => PixelFormat::Rgb,
        VideoFormat::RGBA => PixelFormat::Rgba,
        VideoFormat::RGBx => PixelFormat::Rgbx,
        VideoFormat::BGRx => PixelFormat::Bgrx,
        _ => return Err("unsupported camera pixel format".into()),
    };
    let offset = data.chunk().offset() as usize;
    let end = offset
        .checked_add(data.chunk().size() as usize)
        .ok_or("invalid camera buffer extent")?;
    let stride = usize::try_from(data.chunk().stride()).map_err(|_| "invalid camera stride")?;
    let bytes = data
        .data()
        .and_then(|bytes| bytes.get(offset..end))
        .ok_or("incomplete camera buffer")?;
    CameraFrame::from_raw(
        pixel_format,
        format.size().width,
        format.size().height,
        stride,
        0,
        bytes,
    )
    .map_err(|error| error.to_string())
}

fn preview_events(
    stream: &StreamRc,
    latest: Rc<RefCell<Latest>>,
) -> Result<StreamListener<()>, String> {
    let params = latest.clone();
    stream
        .add_local_listener_with_user_data(())
        .param_changed(move |stream, _, id, param| {
            if id == ParamType::Format.as_raw()
                && param.is_some()
                && let Err(error) = formats::buffers(stream, 4)
            {
                params.borrow_mut().error = Some(error);
            }
        })
        .process(move |stream, _| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            *data.chunk_mut().size_mut() = 0;
            let latest = latest.borrow();
            let Some(frame) = &latest.frame else {
                return;
            };
            let Some(bytes) = data.data() else {
                return;
            };
            if bytes.len() < frame.rgba().len() {
                return;
            }
            bytes[..frame.rgba().len()].copy_from_slice(frame.rgba());
            *data.chunk_mut().offset_mut() = 0;
            *data.chunk_mut().stride_mut() = (frame.width() * 4) as i32;
            *data.chunk_mut().size_mut() = frame.rgba().len() as u32;
        })
        .register()
        .map_err(|_| "camera preview listener unavailable".into())
}
