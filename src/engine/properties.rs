//! HAL property dispatch: one `match`-on-selector function per object kind
//! (plugin, device, stream), each returning a logical [`Value`]. Only
//! `ffi::plugin` turns a `Value` into bytes through a raw pointer.
//!
//! Every `Value` payload is plain `Copy`, C-layout data (never a Rust tuple),
//! so the ffi layer can byte-copy any payload without knowing its meaning.

use super::device::{
    find_by_object_id, find_by_stream_id, DEVICES, MAX_BUFFER_FRAMES, MIN_BUFFER_FRAMES,
    PLUGIN_OBJECT_ID, RING_FRAMES, SAMPLE_RATES, STATE,
};
use crate::ffi::types::{
    fourcc, AudioObjectID, AudioObjectPropertyAddress, AudioStreamBasicDescription,
    AudioStreamRangedDescription, AudioValueRange, CFStringRef, CFURLRef, OSStatus,
};
use std::sync::atomic::Ordering;

/// `kAudioHardwareUnknownPropertyError` ('who?'): selector not answered by this
/// object kind. Computed via `fourcc`, never a hand-copied decimal (2003332927).
pub const ERR_UNKNOWN_PROPERTY: OSStatus = fourcc(b"who?") as OSStatus;
/// `kAudioHardwareBadObjectError` ('!obj'): the ID names nothing this driver owns.
pub const ERR_BAD_OBJECT: OSStatus = fourcc(b"!obj") as OSStatus;
/// `kAudioHardwareIllegalOperationError` ('nope'): a value this driver does not accept.
pub const ERR_ILLEGAL_VALUE: OSStatus = fourcc(b"nope") as OSStatus;
/// `kAudioHardwareUnsupportedOperationError` ('unop'): a known property that is
/// not settable. Defined here so the engine depends only on `ffi::types`.
pub const ERR_UNSUPPORTED_OPERATION: OSStatus = fourcc(b"unop") as OSStatus;

/// `kLinearPCMFormatFlagIsFloat | kLinearPCMFormatFlagIsPacked`.
const LPCM_FLOAT_PACKED: u32 = (1 << 0) | (1 << 3);

/// Any other scope (including `'glob'` and zero) is treated as global.
const SCOPE_INPUT: u32 = fourcc(b"inpt");
const SCOPE_OUTPUT: u32 = fourcc(b"outp");

/// The logical result of a property read. Every variant is plain, `Copy`,
/// C-layout data so `ffi::plugin::copy_out` is one uniform byte copy.
// Boxing `RangedAsbdArr6` (336 bytes) would cost `Copy`; never built on a real-time path.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy)]
pub enum Value {
    U32(u32),
    F64(f64),
    /// Handed to the host as a freshly created `CFStringRef`, one per call.
    Str(&'static str),
    /// `kAudioDevicePropertyIcon`: a `CFURLRef` `ffi::plugin` resolves from the bundle per call.
    Url,
    U32Arr1([u32; 1]),
    U32Arr2([u32; 2]),
    U32Arr5([u32; 5]),
    U32ArrEmpty,
    Range1(AudioValueRange),
    Range6([AudioValueRange; 6]),
    Asbd(AudioStreamBasicDescription),
    /// `Available*Formats`: `AudioStreamRangedDescription`, not bare ASBDs.
    RangedAsbdArr6([AudioStreamRangedDescription; 6]),
}

impl Value {
    /// What `GetPropertyDataSize` reports; object types are pointer-sized.
    pub fn size_in_bytes(self) -> usize {
        match self {
            Value::U32(_) => std::mem::size_of::<u32>(),
            Value::F64(_) => std::mem::size_of::<f64>(),
            Value::Str(_) => std::mem::size_of::<CFStringRef>(),
            Value::Url => std::mem::size_of::<CFURLRef>(),
            Value::U32Arr1(_) => std::mem::size_of::<[u32; 1]>(),
            Value::U32Arr2(_) => std::mem::size_of::<[u32; 2]>(),
            Value::U32Arr5(_) => std::mem::size_of::<[u32; 5]>(),
            Value::U32ArrEmpty => 0,
            Value::Range1(_) => std::mem::size_of::<AudioValueRange>(),
            Value::Range6(_) => std::mem::size_of::<[AudioValueRange; 6]>(),
            Value::Asbd(_) => std::mem::size_of::<AudioStreamBasicDescription>(),
            Value::RangedAsbdArr6(_) => std::mem::size_of::<[AudioStreamRangedDescription; 6]>(),
        }
    }
}

/// The one place `ZeroTimeStampPeriod` is computed. Must be `RING_FRAMES`, a
/// compile-time constant, never derived from a client's requested buffer size.
pub fn zero_timestamp_period() -> u32 {
    RING_FRAMES as u32
}

fn build_asbd(channels: u32, rate: f64) -> AudioStreamBasicDescription {
    let bytes_per_frame = channels.saturating_mul(4); // 32-bit float samples
    AudioStreamBasicDescription {
        sample_rate: rate,
        format_id: fourcc(b"lpcm"),
        format_flags: LPCM_FLOAT_PACKED,
        bytes_per_packet: bytes_per_frame,
        frames_per_packet: 1,
        bytes_per_frame,
        channels_per_frame: channels,
        bits_per_channel: 32,
        reserved: 0,
    }
}

fn build_ranged_asbd(channels: u32, rate: f64) -> AudioStreamRangedDescription {
    AudioStreamRangedDescription {
        format: build_asbd(channels, rate),
        sample_rate_range: AudioValueRange {
            minimum: rate,
            maximum: rate,
        },
    }
}

/// `Streams`/`OwnedObjects` filtered by scope: handing the output stream back
/// for an input-scope query desynchronises the host's enumeration.
fn streams_for_scope(out_stream_id: u32, in_stream_id: u32, scope: u32) -> Value {
    if scope == SCOPE_INPUT {
        Value::U32Arr1([in_stream_id])
    } else if scope == SCOPE_OUTPUT {
        Value::U32Arr1([out_stream_id])
    } else {
        Value::U32Arr2([out_stream_id, in_stream_id])
    }
}

enum ObjectKind {
    Plugin,
    Device(usize),
    Stream(usize, bool), // (device index, is_input)
    Unknown,
}

fn classify(id: AudioObjectID) -> ObjectKind {
    if id == PLUGIN_OBJECT_ID {
        ObjectKind::Plugin
    } else if let Some(dev) = find_by_object_id(id) {
        ObjectKind::Device(dev)
    } else if let Some((dev, is_input)) = find_by_stream_id(id) {
        ObjectKind::Stream(dev, is_input)
    } else {
        ObjectKind::Unknown
    }
}

/// Plugin-object properties. `qualifier` is the decoded `TranslateUIDToDevice`
/// input qualifier, if any (CFString decoding happens in `ffi::plugin`).
pub fn plugin_property(
    a: &AudioObjectPropertyAddress,
    qualifier: Option<&str>,
) -> Result<Value, OSStatus> {
    Ok(match a.selector {
        s if s == fourcc(b"bcls") => Value::U32(fourcc(b"aobj")),
        s if s == fourcc(b"clas") => Value::U32(fourcc(b"aplg")),
        // Owner is 'stdv'; 'stmo' is MixStereoToMono, not Owner.
        s if s == fourcc(b"stdv") => Value::U32(0),
        s if s == fourcc(b"ownd") => Value::U32Arr5(DEVICES.map(|d| d.device_id)),
        s if s == fourcc(b"dev#") => Value::U32Arr5(DEVICES.map(|d| d.device_id)),
        s if s == fourcc(b"uidd") => {
            let target = qualifier.unwrap_or("");
            let id = DEVICES
                .iter()
                .find(|d| d.uid == target)
                .map(|d| d.device_id)
                .unwrap_or(0); // kAudioObjectUnknown: no match
            Value::U32(id)
        }
        s if s == fourcc(b"rsrc") => Value::Str(""),
        s if s == fourcc(b"lmak") => Value::Str("Senda Audio"),
        _ => return Err(ERR_UNKNOWN_PROPERTY),
    })
}

/// Properties answered by one of the five device objects.
pub fn device_property(dev: usize, a: &AudioObjectPropertyAddress) -> Result<Value, OSStatus> {
    let Some(cfg) = DEVICES.get(dev) else {
        return Err(ERR_BAD_OBJECT);
    };
    let Some(st) = STATE.get(dev) else {
        return Err(ERR_BAD_OBJECT);
    };
    Ok(match a.selector {
        s if s == fourcc(b"bcls") => Value::U32(fourcc(b"aobj")),
        s if s == fourcc(b"clas") => Value::U32(fourcc(b"adev")),
        s if s == fourcc(b"stdv") => Value::U32(PLUGIN_OBJECT_ID),
        s if s == fourcc(b"lnam") => Value::Str(cfg.name),
        s if s == fourcc(b"lmak") => Value::Str("Senda Audio"),
        s if s == fourcc(b"uid ") => Value::Str(cfg.uid),
        // One unit per model, so ModelUID equals DeviceUID.
        s if s == fourcc(b"muid") => Value::Str(cfg.uid),
        s if s == fourcc(b"tran") => Value::U32(fourcc(b"virt")),
        s if s == fourcc(b"stm#") => {
            streams_for_scope(cfg.out_stream_id, cfg.in_stream_id, a.scope)
        }
        s if s == fourcc(b"ownd") => {
            streams_for_scope(cfg.out_stream_id, cfg.in_stream_id, a.scope)
        }
        s if s == fourcc(b"livn") => Value::U32(1),
        s if s == fourcc(b"goin") => Value::U32(u32::from(st.io_running.load(Ordering::Acquire))),
        // 'gone' (DeviceIsRunningSomewhere): one IO context per device, so same as 'goin'.
        s if s == fourcc(b"gone") => Value::U32(u32::from(st.io_running.load(Ordering::Acquire))),
        s if s == fourcc(b"dflt") => Value::U32(1),
        s if s == fourcc(b"sflt") => Value::U32(1),
        // Zero like BlackHole: no physical conversion latency.
        s if s == fourcc(b"ltnc") => Value::U32(0),
        s if s == fourcc(b"saft") => Value::U32(0),
        // Must equal the clock's advance period (`RING_FRAMES`), never the client's buffer size.
        s if s == fourcc(b"ring") => Value::U32(zero_timestamp_period()),
        s if s == fourcc(b"clkd") => Value::U32(cfg.device_id),
        s if s == fourcc(b"nsrt") => Value::F64(st.rate()),
        s if s == fourcc(b"nsr#") => Value::Range6(SAMPLE_RATES.map(|r| AudioValueRange {
            minimum: r,
            maximum: r,
        })),
        // Negotiated per device; never feeds `RING_FRAMES`.
        s if s == fourcc(b"fsiz") => Value::U32(st.buffer_frames()),
        s if s == fourcc(b"fsz#") => Value::Range1(AudioValueRange {
            minimum: f64::from(MIN_BUFFER_FRAMES),
            maximum: f64::from(MAX_BUFFER_FRAMES),
        }),
        s if s == fourcc(b"hidn") => Value::U32(0),
        s if s == fourcc(b"icon") => Value::Url,
        // Not scope-filtered: both streams number channels from 1.
        s if s == fourcc(b"dch2") => Value::U32Arr2([1, 2]),
        s if s == fourcc(b"akin") => Value::U32ArrEmpty,
        s if s == fourcc(b"ctrl") => Value::U32ArrEmpty,
        _ => return Err(ERR_UNKNOWN_PROPERTY),
    })
}

/// Properties answered by one of a device's two streams.
pub fn stream_property(
    dev: usize,
    is_input: bool,
    a: &AudioObjectPropertyAddress,
) -> Result<Value, OSStatus> {
    let Some(cfg) = DEVICES.get(dev) else {
        return Err(ERR_BAD_OBJECT);
    };
    let Some(st) = STATE.get(dev) else {
        return Err(ERR_BAD_OBJECT);
    };
    Ok(match a.selector {
        s if s == fourcc(b"stdv") => Value::U32(cfg.device_id),
        s if s == fourcc(b"clas") => Value::U32(fourcc(b"astr")),
        s if s == fourcc(b"bcls") => Value::U32(fourcc(b"aobj")),
        s if s == fourcc(b"sact") => Value::U32(1),
        // 0 = output, 1 = input.
        s if s == fourcc(b"sdir") => Value::U32(u32::from(is_input)),
        s if s == fourcc(b"term") => Value::U32(0),
        s if s == fourcc(b"schn") => Value::U32(1),
        // Single format, not ranged.
        s if s == fourcc(b"sfmt") || s == fourcc(b"pft ") => {
            Value::Asbd(build_asbd(cfg.channels, st.rate()))
        }
        // Ranged descriptions, 56 bytes each.
        s if s == fourcc(b"sfma") || s == fourcc(b"pfta") => {
            Value::RangedAsbdArr6(SAMPLE_RATES.map(|r| build_ranged_asbd(cfg.channels, r)))
        }
        s if s == fourcc(b"ltnc") => Value::U32(0),
        _ => return Err(ERR_UNKNOWN_PROPERTY),
    })
}

/// Single source for `get_property_data`, `get_property_data_size` and `has_property`.
fn resolve(
    id: AudioObjectID,
    a: &AudioObjectPropertyAddress,
    qualifier: Option<&str>,
) -> Result<Value, OSStatus> {
    match classify(id) {
        ObjectKind::Plugin => plugin_property(a, qualifier),
        ObjectKind::Device(dev) => device_property(dev, a),
        ObjectKind::Stream(dev, is_input) => stream_property(dev, is_input, a),
        ObjectKind::Unknown => Err(ERR_BAD_OBJECT),
    }
}

pub fn has_property(id: AudioObjectID, a: &AudioObjectPropertyAddress) -> bool {
    resolve(id, a, None).is_ok()
}

pub fn is_property_settable(
    id: AudioObjectID,
    a: &AudioObjectPropertyAddress,
) -> Result<bool, OSStatus> {
    // A selector `HasProperty` would refuse must error here too.
    resolve(id, a, None)?;
    let is_device = matches!(classify(id), ObjectKind::Device(_));
    let settable_selector = a.selector == fourcc(b"nsrt") || a.selector == fourcc(b"fsiz");
    Ok(is_device && settable_selector)
}

pub fn get_property_data_size(
    id: AudioObjectID,
    a: &AudioObjectPropertyAddress,
    qualifier: Option<&str>,
) -> Result<usize, OSStatus> {
    resolve(id, a, qualifier).map(Value::size_in_bytes)
}

pub fn get_property_data(
    id: AudioObjectID,
    a: &AudioObjectPropertyAddress,
    qualifier: Option<&str>,
) -> Result<Value, OSStatus> {
    resolve(id, a, qualifier)
}

/// Applies `SetPropertyData`. `data` is already a bounds-checked slice. Only
/// a device's `NominalSampleRate` and `BufferFrameSize` are settable.
pub fn set_property_data(
    id: AudioObjectID,
    a: &AudioObjectPropertyAddress,
    data: &[u8],
) -> Result<(), OSStatus> {
    let ObjectKind::Device(dev) = classify(id) else {
        return Err(ERR_BAD_OBJECT);
    };
    // Keeps "unknown property" and "known but read-only" distinct.
    device_property(dev, a)?;
    let Some(st) = STATE.get(dev) else {
        return Err(ERR_BAD_OBJECT);
    };
    if a.selector == fourcc(b"nsrt") {
        let bytes: [u8; 8] = data
            .get(..8)
            .and_then(|s| s.try_into().ok())
            .ok_or(ERR_ILLEGAL_VALUE)?;
        let rate = f64::from_ne_bytes(bytes);
        if !SAMPLE_RATES.contains(&rate) {
            return Err(ERR_ILLEGAL_VALUE);
        }
        st.set_rate(rate);
        // TODO: the HAL caches NominalSampleRate/VirtualFormat, so this should go
        // through `RequestDeviceConfigurationChange` and fire `PropertiesChanged`;
        // no `AudioServerPlugInHostRef` reaches the engine yet, so it is applied directly.
        return Ok(());
    }
    if a.selector == fourcc(b"fsiz") {
        let bytes: [u8; 4] = data
            .get(..4)
            .and_then(|s| s.try_into().ok())
            .ok_or(ERR_ILLEGAL_VALUE)?;
        let frames = u32::from_ne_bytes(bytes);
        st.set_buffer_frames(frames); // clamps internally; never rejects
        return Ok(());
    }
    Err(ERR_UNSUPPORTED_OPERATION)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::types::AudioObjectPropertyAddress;

    fn addr(sel: u32) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress {
            selector: sel,
            scope: 0,
            element: 0,
        }
    }

    #[test]
    fn error_constants_match_the_documented_decimal_values() {
        assert_eq!(ERR_UNKNOWN_PROPERTY, 2003332927);
        assert_eq!(ERR_BAD_OBJECT, 560947818);
    }

    fn scoped_addr(sel: u32, scope: u32) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress {
            selector: sel,
            scope,
            element: 0,
        }
    }

    #[test]
    fn zero_timestamp_period_is_ring_frames_and_constant() {
        assert_eq!(zero_timestamp_period(), RING_FRAMES as u32);
        assert_eq!(zero_timestamp_period(), 32768);
    }

    #[test]
    fn plugin_object_has_expected_properties() {
        assert!(has_property(PLUGIN_OBJECT_ID, &addr(fourcc(b"dev#"))));
        assert!(has_property(PLUGIN_OBJECT_ID, &addr(fourcc(b"lmak"))));
        assert!(!has_property(PLUGIN_OBJECT_ID, &addr(fourcc(b"nsrt"))));
    }

    #[test]
    fn device_list_has_all_five_devices_in_order() {
        let result = plugin_property(&addr(fourcc(b"dev#")), None);
        assert!(matches!(
            result,
            Ok(Value::U32Arr5(ids)) if ids == [10, 20, 30, 40, 50]
        ));
    }

    #[test]
    fn translate_uid_to_device_resolves_known_and_unknown_uids() {
        assert!(matches!(
            plugin_property(&addr(fourcc(b"uidd")), Some("SendaLink_8ch")),
            Ok(Value::U32(20))
        ));
        assert!(matches!(
            plugin_property(&addr(fourcc(b"uidd")), Some("not-a-real-uid")),
            Ok(Value::U32(0))
        ));
    }

    #[test]
    fn device_zero_timestamp_period_property_reads_32768_for_every_device() {
        for dev in 0..5 {
            let result = device_property(dev, &addr(fourcc(b"ring")));
            assert!(
                matches!(result, Ok(Value::U32(32768))),
                "device {dev} missing ring property"
            );
        }
    }

    #[test]
    fn safety_offset_and_latency_are_zero() {
        for dev in 0..5 {
            assert!(
                matches!(
                    device_property(dev, &addr(fourcc(b"saft"))),
                    Ok(Value::U32(0))
                ),
                "device {dev}: SafetyOffset must be 0"
            );
            assert!(
                matches!(
                    device_property(dev, &addr(fourcc(b"ltnc"))),
                    Ok(Value::U32(0))
                ),
                "device {dev}: Latency must be 0"
            );
        }
    }

    #[test]
    fn unknown_device_property_is_an_error() {
        assert!(device_property(0, &addr(fourcc(b"xxxx"))).is_err());
    }

    #[test]
    fn unknown_object_id_is_bad_object() {
        assert!(matches!(
            get_property_data(9999, &addr(fourcc(b"lnam")), None),
            Err(ERR_BAD_OBJECT)
        ));
    }

    #[test]
    fn streams_property_is_filtered_by_scope() {
        let input = scoped_addr(fourcc(b"stm#"), fourcc(b"inpt"));
        assert!(matches!(
            device_property(0, &input),
            Ok(Value::U32Arr1([12]))
        ));

        let output = scoped_addr(fourcc(b"stm#"), fourcc(b"outp"));
        assert!(matches!(
            device_property(0, &output),
            Ok(Value::U32Arr1([11]))
        ));

        let global = scoped_addr(fourcc(b"stm#"), fourcc(b"glob"));
        assert!(matches!(
            device_property(0, &global),
            Ok(Value::U32Arr2([11, 12]))
        ));

        let owned_input = scoped_addr(fourcc(b"ownd"), fourcc(b"inpt"));
        assert!(matches!(
            device_property(0, &owned_input),
            Ok(Value::U32Arr1([12]))
        ));
    }

    #[test]
    fn only_nominal_sample_rate_and_buffer_size_are_settable_on_a_device() {
        assert_eq!(is_property_settable(10, &addr(fourcc(b"nsrt"))), Ok(true));
        assert_eq!(is_property_settable(10, &addr(fourcc(b"fsiz"))), Ok(true));
        assert_eq!(is_property_settable(10, &addr(fourcc(b"lnam"))), Ok(false));
        assert_eq!(is_property_settable(10, &addr(fourcc(b"icon"))), Ok(false));
    }

    #[test]
    fn icon_property_is_a_readonly_pointer_sized_device_property() {
        let mut checked = 0;
        for (dev, cfg) in DEVICES.iter().enumerate() {
            let a = addr(fourcc(b"icon"));
            assert!(has_property(cfg.device_id, &a), "device {dev} missing icon");
            assert!(matches!(device_property(dev, &a), Ok(Value::Url)));
            assert_eq!(
                get_property_data_size(cfg.device_id, &a, None),
                Ok(std::mem::size_of::<CFURLRef>())
            );
            assert_eq!(is_property_settable(cfg.device_id, &a), Ok(false));
            assert_eq!(
                set_property_data(cfg.device_id, &a, &[0u8; 8]),
                Err(ERR_UNSUPPORTED_OPERATION)
            );
            checked += 1;
        }
        assert_eq!(checked, 5, "must have exercised all five devices");
    }

    // Device index 4 (id 50) is used only by this test; `STATE` is shared.
    #[test]
    fn set_nominal_sample_rate_accepts_listed_rates_and_rejects_others() {
        assert!(set_property_data(50, &addr(fourcc(b"nsrt")), &96000.0_f64.to_ne_bytes()).is_ok());
        assert!(matches!(
            device_property(4, &addr(fourcc(b"nsrt"))),
            Ok(Value::F64(r)) if r == 96000.0
        ));
        assert_eq!(
            set_property_data(50, &addr(fourcc(b"nsrt")), &12345.0_f64.to_ne_bytes()),
            Err(ERR_ILLEGAL_VALUE)
        );
    }

    // Device index 1 (id 20) is used only by this test.
    #[test]
    fn set_buffer_frame_size_clamps_to_advertised_range() {
        assert!(set_property_data(20, &addr(fourcc(b"fsiz")), &1024u32.to_ne_bytes()).is_ok());
        assert!(matches!(
            device_property(1, &addr(fourcc(b"fsiz"))),
            Ok(Value::U32(1024))
        ));

        assert!(set_property_data(20, &addr(fourcc(b"fsiz")), &1u32.to_ne_bytes()).is_ok());
        assert!(matches!(
            device_property(1, &addr(fourcc(b"fsiz"))),
            Ok(Value::U32(MIN_BUFFER_FRAMES))
        ));

        assert!(set_property_data(20, &addr(fourcc(b"fsiz")), &u32::MAX.to_ne_bytes()).is_ok());
        assert!(matches!(
            device_property(1, &addr(fourcc(b"fsiz"))),
            Ok(Value::U32(MAX_BUFFER_FRAMES))
        ));
    }

    #[test]
    fn setting_a_known_readonly_property_is_unsupported_not_unknown() {
        assert!(has_property(10, &addr(fourcc(b"lnam"))));
        assert_eq!(
            set_property_data(10, &addr(fourcc(b"lnam")), &[0u8; 8]),
            Err(ERR_UNSUPPORTED_OPERATION)
        );
        assert_eq!(
            set_property_data(10, &addr(fourcc(b"xxxx")), &[0u8; 8]),
            Err(ERR_UNKNOWN_PROPERTY)
        );
    }

    #[test]
    fn stream_direction_matches_in_vs_out() {
        assert!(matches!(
            stream_property(0, false, &addr(fourcc(b"sdir"))),
            Ok(Value::U32(0))
        ));
        assert!(matches!(
            stream_property(0, true, &addr(fourcc(b"sdir"))),
            Ok(Value::U32(1))
        ));
    }

    #[test]
    fn value_sizes_match_c_layout_expectations() {
        assert_eq!(Value::U32(0).size_in_bytes(), 4);
        assert_eq!(Value::F64(0.0).size_in_bytes(), 8);
        assert_eq!(
            Value::Str("x").size_in_bytes(),
            std::mem::size_of::<CFStringRef>()
        );
        assert_eq!(Value::Url.size_in_bytes(), std::mem::size_of::<CFURLRef>());
        assert_eq!(Value::U32Arr1([0]).size_in_bytes(), 4);
        assert_eq!(Value::U32Arr2([0, 0]).size_in_bytes(), 8);
        assert_eq!(Value::U32Arr5([0; 5]).size_in_bytes(), 20);
        assert_eq!(Value::U32ArrEmpty.size_in_bytes(), 0);
        assert_eq!(
            Value::Range1(AudioValueRange {
                minimum: 0.0,
                maximum: 0.0
            })
            .size_in_bytes(),
            16
        );
        assert_eq!(
            Value::Range6(
                [AudioValueRange {
                    minimum: 0.0,
                    maximum: 0.0
                }; 6]
            )
            .size_in_bytes(),
            96
        );
        assert_eq!(std::mem::size_of::<AudioStreamBasicDescription>(), 40);
        // A bare ASBD array would be 240 bytes; the Available* lists are 336.
        let zero_asbd = AudioStreamBasicDescription {
            sample_rate: 0.0,
            format_id: 0,
            format_flags: 0,
            bytes_per_packet: 0,
            frames_per_packet: 0,
            bytes_per_frame: 0,
            channels_per_frame: 0,
            bits_per_channel: 0,
            reserved: 0,
        };
        let zero_range = AudioValueRange {
            minimum: 0.0,
            maximum: 0.0,
        };
        assert_eq!(Value::Asbd(zero_asbd).size_in_bytes(), 40);
        assert_eq!(
            Value::RangedAsbdArr6(
                [AudioStreamRangedDescription {
                    format: zero_asbd,
                    sample_rate_range: zero_range,
                }; 6]
            )
            .size_in_bytes(),
            336
        );
    }

    #[test]
    fn every_mandated_property_answers_with_its_declared_size() {
        const CFSTR: usize = std::mem::size_of::<CFStringRef>();

        let plugin_props: &[(u32, usize)] = &[
            (fourcc(b"bcls"), 4),
            (fourcc(b"clas"), 4),
            (fourcc(b"stdv"), 4),
            (fourcc(b"ownd"), 20),
            (fourcc(b"dev#"), 20),
            (fourcc(b"uidd"), 4),
            (fourcc(b"rsrc"), CFSTR),
            (fourcc(b"lmak"), CFSTR),
        ];
        for &(sel, size) in plugin_props {
            let a = addr(sel);
            assert!(
                has_property(PLUGIN_OBJECT_ID, &a),
                "plugin missing {sel:#x}"
            );
            assert_eq!(
                get_property_data_size(PLUGIN_OBJECT_ID, &a, None),
                Ok(size),
                "plugin size mismatch for {sel:#x}"
            );
        }

        // Unscoped address: `stm#`/`ownd` answer with both streams.
        let device_props: &[(u32, usize)] = &[
            (fourcc(b"stdv"), 4),
            (fourcc(b"bcls"), 4),
            (fourcc(b"clas"), 4),
            (fourcc(b"lnam"), CFSTR),
            (fourcc(b"lmak"), CFSTR),
            (fourcc(b"uid "), CFSTR),
            (fourcc(b"muid"), CFSTR),
            (fourcc(b"tran"), 4),
            (fourcc(b"stm#"), 8),
            (fourcc(b"ownd"), 8),
            (fourcc(b"livn"), 4),
            (fourcc(b"goin"), 4),
            (fourcc(b"gone"), 4),
            (fourcc(b"dflt"), 4),
            (fourcc(b"sflt"), 4),
            (fourcc(b"ltnc"), 4),
            (fourcc(b"saft"), 4),
            (fourcc(b"ring"), 4),
            (fourcc(b"clkd"), 4),
            (fourcc(b"nsrt"), 8),
            (fourcc(b"nsr#"), 96),
            (fourcc(b"fsiz"), 4),
            (fourcc(b"fsz#"), 16),
            (fourcc(b"hidn"), 4),
            (fourcc(b"dch2"), 8),
            (fourcc(b"akin"), 0),
            (fourcc(b"ctrl"), 0),
            (fourcc(b"icon"), CFSTR), // CFURLRef is pointer-sized, same as CFStringRef
        ];
        assert_eq!(device_props.len(), 28);
        for &(sel, size) in device_props {
            let a = addr(sel);
            assert!(has_property(10, &a), "device missing {sel:#x}");
            assert_eq!(
                get_property_data_size(10, &a, None),
                Ok(size),
                "device size mismatch for {sel:#x}"
            );
        }

        let stream_props: &[(u32, usize)] = &[
            (fourcc(b"stdv"), 4),
            (fourcc(b"clas"), 4),
            (fourcc(b"bcls"), 4),
            (fourcc(b"sact"), 4),
            (fourcc(b"sdir"), 4),
            (fourcc(b"term"), 4),
            (fourcc(b"schn"), 4),
            (fourcc(b"sfmt"), 40),
            (fourcc(b"pft "), 40),
            (fourcc(b"sfma"), 336), // ranged array, not bare ASBDs (240)
            (fourcc(b"pfta"), 336), // ranged array, not bare ASBDs (240)
            (fourcc(b"ltnc"), 4),
        ];
        assert_eq!(stream_props.len(), 12);
        for &(sel, size) in stream_props {
            let a = addr(sel);
            assert!(has_property(11, &a), "stream missing {sel:#x}");
            assert_eq!(
                get_property_data_size(11, &a, None),
                Ok(size),
                "stream size mismatch for {sel:#x}"
            );
        }

        assert_eq!(
            plugin_props.len() + device_props.len() + stream_props.len(),
            48
        );
    }
}
