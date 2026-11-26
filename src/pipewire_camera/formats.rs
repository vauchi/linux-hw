// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use pipewire::{self as pw, spa};
use spa::param::{
    ParamType,
    format::{FormatProperties as F, MediaSubtype, MediaType},
    video::VideoFormat,
};
use spa::pod::{Object, Pod, Property, PropertyFlags, Value};
use spa::utils::{Choice, ChoiceEnum, ChoiceFlags, SpaTypes};

fn serialize(value: Value) -> Result<Vec<u8>, String> {
    spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &value)
        .map(|result| result.0.into_inner())
        .map_err(|_| "camera format encoding failed".into())
}

pub(super) fn video(preview: bool) -> Result<Vec<u8>, String> {
    let pixel_format = if preview {
        spa::pod::property!(F::VideoFormat, Id, VideoFormat::RGBA)
    } else {
        spa::pod::property!(
            F::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::YUY2,
            VideoFormat::YUY2,
            VideoFormat::RGB,
            VideoFormat::RGBA,
            VideoFormat::RGBx,
            VideoFormat::BGRx
        )
    };
    serialize(Value::Object(spa::pod::object!(
        SpaTypes::ObjectParamFormat,
        ParamType::EnumFormat,
        spa::pod::property!(F::MediaType, Id, MediaType::Video),
        spa::pod::property!(F::MediaSubtype, Id, MediaSubtype::Raw),
        pixel_format,
        spa::pod::property!(
            F::VideoSize,
            Rectangle,
            spa::utils::Rectangle {
                width: 640,
                height: 480
            }
        ),
        spa::pod::property!(
            F::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: 30, denom: 1 },
            spa::utils::Fraction { num: 1, denom: 1 },
            spa::utils::Fraction { num: 60, denom: 1 }
        ),
    )))
}

pub(super) fn buffers(stream: &pw::stream::Stream, bytes_per_pixel: i32) -> Result<(), String> {
    let mask = (1 << spa::sys::SPA_DATA_MemPtr) | (1 << spa::sys::SPA_DATA_MemFd);
    let property = |key, value| Property {
        key,
        flags: PropertyFlags::empty(),
        value,
    };
    let bytes = serialize(Value::Object(Object {
        type_: SpaTypes::ObjectParamBuffers.as_raw(),
        id: ParamType::Buffers.as_raw(),
        properties: vec![
            property(
                spa::sys::SPA_PARAM_BUFFERS_dataType,
                Value::Choice(spa::pod::ChoiceValue::Int(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Flags {
                        default: mask,
                        flags: vec![mask],
                    },
                ))),
            ),
            property(spa::sys::SPA_PARAM_BUFFERS_blocks, Value::Int(1)),
            property(
                spa::sys::SPA_PARAM_BUFFERS_size,
                Value::Int(640 * 480 * bytes_per_pixel),
            ),
            property(
                spa::sys::SPA_PARAM_BUFFERS_stride,
                Value::Int(640 * bytes_per_pixel),
            ),
        ],
    }))?;
    let param = Pod::from_bytes(&bytes).ok_or("invalid camera buffer format")?;
    stream
        .update_params(&mut [param])
        .map_err(|_| "camera buffer negotiation failed".into())
}
