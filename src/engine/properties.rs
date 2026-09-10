//! HAL property dispatch: the answer to "what is this object, and what does
//! it say when asked for property X". Three `match`-on-selector functions —
//! one per object kind (plugin, device, stream) — each returning a logical
//! [`Value`]. `src/ffi/plugin.rs` is the only place that turns a `Value`
//! into bytes written through a raw pointer (including CFString creation),
//! per the "no unsafe in engine/" rule.
//!
//! Every `Value` payload is a plain, `Copy`, `#[repr(C)]`-compatible type
//! (`u32`, `f64`, fixed-size arrays of those, or the `AudioValueRange` /
//! `AudioStreamBasicDescription` / `AudioStreamRangedDescription` structs
//! from `ffi::types`) — never a raw Rust tuple, whose field layout C code
//! must not assume. That is what lets the ffi layer do a plain byte copy of
//! the payload without needing to know anything about *why* the value has
//! the shape it does.

use super::device::{
    find_by_object_id, find_by_stream_id, DEVICES, MAX_BUFFER_FRAMES, MIN_BUFFER_FRAMES,
    PLUGIN_OBJECT_ID, RING_FRAMES, SAMPLE_RATES, STATE,
};
use crate::ffi::types::{
    fourcc, AudioObjectID, AudioObjectPropertyAddress, AudioStreamBasicDescription,
    AudioStreamRangedDescription, AudioValueRange, CFStringRef, CFURLRef, OSStatus,
};
use std::sync::atomic::Ordering;

/// `kAudioHardwareUnknownPropertyError` ('who?'): the selector isn't one
/// this object kind answers. Computed via `fourcc` rather than hand-copied
/// as a decimal literal — same treatment as
/// `ERR_ILLEGAL_VALUE`/`ERR_UNSUPPORTED_OPERATION` below, and the matching
/// constant in `ffi::plugin`. Equals 2003332927.
pub const ERR_UNKNOWN_PROPERTY: OSStatus = fourcc(b"who?") as OSStatus;
/// `kAudioHardwareBadObjectError` ('!obj'): the `AudioObjectID` doesn't name
/// a plugin, device, or stream this driver owns. Same `fourcc`-not-decimal
/// treatment as `ERR_UNKNOWN_PROPERTY` above. Equals 560947818.
pub const ERR_BAD_OBJECT: OSStatus = fourcc(b"!obj") as OSStatus;
/// `kAudioHardwareIllegalOperationError` ('nope'): well-formed request, but
/// the value being set isn't one this driver accepts (e.g. an unsupported
/// sample rate).
pub const ERR_ILLEGAL_VALUE: OSStatus = fourcc(b"nope") as OSStatus;
/// `kAudioHardwareUnsupportedOperationError` ('unop'): the property exists
/// (`has_property`/`HasProperty` is true for it) but this driver doesn't
/// allow *setting* it — distinct from `ERR_UNKNOWN_PROPERTY`, which means
/// the selector isn't a property of this object at all. Defined here (same
/// value as `ffi::plugin::ERR_UNSUPPORTED`) rather than imported from
/// `ffi::plugin`, so that the engine depends only on `ffi::types`.
pub const ERR_UNSUPPORTED_OPERATION: OSStatus = fourcc(b"unop") as OSStatus;

/// PCM format flags: float (bit 0) + packed (bit 3). Matches
/// `kLinearPCMFormatFlagIsFloat | kLinearPCMFormatFlagIsPacked`.
const LPCM_FLOAT_PACKED: u32 = (1 << 0) | (1 << 3);

/// `kAudioObjectPropertyScopeInput` / `kAudioObjectPropertyScopeOutput`.
/// Anything else (including `kAudioObjectPropertyScopeGlobal` = `'glob'`,
/// and the zero value most hand-built test addresses use) is treated as
/// "global" by `streams_for_scope` below.
const SCOPE_INPUT: u32 = fourcc(b"inpt");
const SCOPE_OUTPUT: u32 = fourcc(b"outp");

/// The logical result of a property read, independent of how it will be
/// serialised. See the module doc for why every variant is plain, `Copy`,
/// C-layout data.
///
/// `RangedAsbdArr6` (336 bytes) makes this enum considerably larger than
/// its other variants, which clippy flags. Boxing it to shrink the enum
/// would require giving up `Copy` (per clippy's own note on this lint) and
/// would mean `ffi::plugin::copy_out` — currently a single, uniform,
/// allocation-free byte copy for every variant — would need a special case
/// to look through a heap pointer for exactly one variant. This value is
/// only ever produced transiently by a property query (never per-audio-
/// block, never on any real-time path), so the extra stack space costs
/// nothing that matters; keeping every variant plain, `Copy`, inline data
/// is worth more than the size win.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy)]
pub enum Value {
    U32(u32),
    F64(f64),
    /// A static string to be handed to the host as a freshly created
    /// `CFStringRef` (one per call — see ffi::plugin's cfstring helper).
    Str(&'static str),
    /// `kAudioDevicePropertyIcon`: a `CFURLRef` pointing at this driver's own
    /// icon resource inside its bundle. Carries no payload — unlike `Str`,
    /// which needs the engine to supply *which* string, the URL is fully
    /// determined by this driver's own bundle contents, so `ffi::plugin`
    /// resolves it from scratch on every call (one per call, same as `Str`
    /// — see `ffi::plugin`'s `icon_resource_url`).
    Url,
    U32Arr1([u32; 1]),
    U32Arr2([u32; 2]),
    U32Arr5([u32; 5]),
    U32ArrEmpty,
    Range1(AudioValueRange),
    Range6([AudioValueRange; 6]),
    Asbd(AudioStreamBasicDescription),
    /// `AvailableVirtualFormats` / `AvailablePhysicalFormats`: an array of
    /// `AudioStreamRangedDescription` (format + the sample-rate range it's
    /// valid over), NOT `AudioStreamBasicDescription` — see the type's doc
    /// in `ffi::types` for why conflating the two corrupts every element.
    RangedAsbdArr6([AudioStreamRangedDescription; 6]),
}

impl Value {
    /// Byte size the host should expect for this value, i.e. what
    /// `GetPropertyDataSize` reports. CFStringRef and other object types are
    /// pointer-sized (the HAL takes ownership of the pointee separately).
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

/// The one and only place `ZeroTimeStampPeriod` is computed. Must return
/// `RING_FRAMES` (32768), a compile-time constant — never anything derived
/// from a client's requested IO buffer size. See `device::RING_FRAMES` for
/// the full rationale; this function exists so the invariant has exactly one
/// call site to audit and test.
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

/// Answers `Streams`/`OwnedObjects` for a device, filtered by `scope`. A
/// device's two stream IDs must NOT both be handed back regardless of scope
/// — a host querying the input scope and getting the output stream back
/// would go on to ask that stream its `Direction` and be told "output",
/// desynchronising enumeration.
fn streams_for_scope(out_stream_id: u32, in_stream_id: u32, scope: u32) -> Value {
    if scope == SCOPE_INPUT {
        Value::U32Arr1([in_stream_id])
    } else if scope == SCOPE_OUTPUT {
        Value::U32Arr1([out_stream_id])
    } else {
        // kAudioObjectPropertyScopeGlobal, or an address built without an
        // explicit scope (e.g. this module's own tests): both streams.
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

/// Properties answered by the plugin object itself (`AudioObjectID == 1`).
/// `qualifier` carries the decoded UTF-8 payload of the input qualifier for
/// `TranslateUIDToDevice`, if any was supplied — decoding the qualifier
/// `CFStringRef` is CoreFoundation FFI and happens in `ffi::plugin`.
pub fn plugin_property(
    a: &AudioObjectPropertyAddress,
    qualifier: Option<&str>,
) -> Result<Value, OSStatus> {
    Ok(match a.selector {
        s if s == fourcc(b"bcls") => Value::U32(fourcc(b"aobj")),
        s if s == fourcc(b"clas") => Value::U32(fourcc(b"aplg")),
        // kAudioObjectPropertyOwner is 'stdv'. 'stmo' is a different real
        // selector — kAudioHardwarePropertyMixStereoToMono — that merely
        // looks plausible; answering it instead would leave Owner
        // unanswered. The plugin object has no owner of its own within
        // this driver's object model.
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
        // kAudioObjectPropertyOwner ('stdv', not 'stmo' — see the note
        // in plugin_property above).
        s if s == fourcc(b"stdv") => Value::U32(PLUGIN_OBJECT_ID),
        s if s == fourcc(b"lnam") => Value::Str(cfg.name),
        s if s == fourcc(b"lmak") => Value::Str("Senda Audio"),
        s if s == fourcc(b"uid ") => Value::Str(cfg.uid),
        // One physical unit per model in this driver, so ModelUID and
        // DeviceUID coincide.
        s if s == fourcc(b"muid") => Value::Str(cfg.uid),
        s if s == fourcc(b"tran") => Value::U32(fourcc(b"virt")),
        // Filtered by scope: querying the input scope must not hand back
        // the output stream's ID (or vice versa) — see `streams_for_scope`.
        // The device owns exactly its two streams and ControlList is
        // empty, so OwnedObjects and Streams coincide.
        s if s == fourcc(b"stm#") => {
            streams_for_scope(cfg.out_stream_id, cfg.in_stream_id, a.scope)
        }
        s if s == fourcc(b"ownd") => {
            streams_for_scope(cfg.out_stream_id, cfg.in_stream_id, a.scope)
        }
        s if s == fourcc(b"livn") => Value::U32(1),
        s if s == fourcc(b"goin") => Value::U32(u32::from(st.io_running.load(Ordering::Acquire))),
        // kAudioDevicePropertyDeviceIsRunningSomewhere ('gone'): this
        // driver has exactly one IO context per device (no separate
        // "hardware is running independently of any client" concept the
        // way a physical device might), so it reports the same
        // `io_running` flag as `'goin'` (DeviceIsRunning). It is answered
        // rather than left unknown because "is another process already
        // using this device" is exactly the question a loopback cable's
        // whole purpose (being open in two processes at once) makes
        // client code likely to ask.
        s if s == fourcc(b"gone") => Value::U32(u32::from(st.io_running.load(Ordering::Acquire))),
        s if s == fourcc(b"dflt") => Value::U32(1),
        s if s == fourcc(b"sflt") => Value::U32(1),
        // BlackHole, a widely used cable, reports zero for both — normal
        // for a virtual device with no physical conversion latency — and
        // matching it keeps host scheduling identical.
        s if s == fourcc(b"ltnc") => Value::U32(0),
        s if s == fourcc(b"saft") => Value::U32(0),
        // The load-bearing property of this whole project: a fixed ring
        // geometry constant, never derived from ioBufferFrameSize. See
        // `zero_timestamp_period` and `device::RING_FRAMES`.
        s if s == fourcc(b"ring") => Value::U32(zero_timestamp_period()),
        s if s == fourcc(b"clkd") => Value::U32(cfg.device_id),
        s if s == fourcc(b"nsrt") => Value::F64(st.rate()),
        s if s == fourcc(b"nsr#") => Value::Range6(SAMPLE_RATES.map(|r| AudioValueRange {
            minimum: r,
            maximum: r,
        })),
        // Negotiated per device, settable via SetPropertyData — see
        // `PerDevice::buffer_frames` and the note on `device::BLOCK`: this
        // value never feeds RING_FRAMES.
        s if s == fourcc(b"fsiz") => Value::U32(st.buffer_frames()),
        s if s == fourcc(b"fsz#") => Value::Range1(AudioValueRange {
            minimum: f64::from(MIN_BUFFER_FRAMES),
            maximum: f64::from(MAX_BUFFER_FRAMES),
        }),
        s if s == fourcc(b"hidn") => Value::U32(0),
        // kAudioDevicePropertyIcon: read-only (not in `settable_selector`
        // below, so `is_property_settable` answers `false` for it same as
        // any other read-only device property).
        s if s == fourcc(b"icon") => Value::Url,
        // PreferredChannelsForStereo. Deliberately not filtered by
        // `a.scope` like `stm#`/`ownd` above: unlike a stream-ID list, the
        // *value* here doesn't depend on direction — both of this device's
        // streams number their channels starting at 1, so the preferred
        // stereo pair is (1, 2) whether queried under 'inpt', 'outp', or
        // 'glob'.
        s if s == fourcc(b"dch2") => Value::U32Arr2([1, 2]),
        s if s == fourcc(b"akin") => Value::U32ArrEmpty,
        s if s == fourcc(b"ctrl") => Value::U32ArrEmpty,
        _ => return Err(ERR_UNKNOWN_PROPERTY),
    })
}

/// Properties answered by one of the two streams (in or out) owned by a
/// device.
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
        // kAudioObjectPropertyOwner ('stdv', not 'stmo' — see the note
        // in plugin_property above).
        s if s == fourcc(b"stdv") => Value::U32(cfg.device_id),
        s if s == fourcc(b"clas") => Value::U32(fourcc(b"astr")),
        s if s == fourcc(b"bcls") => Value::U32(fourcc(b"aobj")),
        s if s == fourcc(b"sact") => Value::U32(1),
        // kAudioStreamPropertyDirection: 0 = output, 1 = input.
        s if s == fourcc(b"sdir") => Value::U32(u32::from(is_input)),
        // No physical terminal on a virtual device.
        s if s == fourcc(b"term") => Value::U32(0),
        s if s == fourcc(b"schn") => Value::U32(1),
        // Bare ASBD — not ranged. Do not change to RangedAsbdArr6-style;
        // `'sfmt'`/`'pft '` are single-format properties, unlike the
        // Available* list properties below.
        s if s == fourcc(b"sfmt") || s == fourcc(b"pft ") => {
            Value::Asbd(build_asbd(cfg.channels, st.rate()))
        }
        // AudioStreamRangedDescription array (56 bytes/element, 336 total)
        // — see the type doc in `ffi::types` and Value::RangedAsbdArr6.
        s if s == fourcc(b"sfma") || s == fourcc(b"pfta") => {
            Value::RangedAsbdArr6(SAMPLE_RATES.map(|r| build_ranged_asbd(cfg.channels, r)))
        }
        s if s == fourcc(b"ltnc") => Value::U32(0),
        _ => return Err(ERR_UNKNOWN_PROPERTY),
    })
}

/// Resolves a property for whichever object kind `id` names, returning the
/// logical value. This is the single source both `get_property_data` and
/// `get_property_data_size` read from, and also what `has_property` probes
/// (discarding the computed value) — one match arm list per object kind,
/// used everywhere that object kind's properties are needed.
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
    // Confirms the property is known before answering, matching the HAL's
    // contract that IsPropertySettable errors for a selector HasProperty
    // would have refused.
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

/// Applies a `SetPropertyData` request. `data` is already a bounds-checked
/// safe slice built by `ffi::plugin` from the raw pointer/size pair the host
/// provided. The only settable properties in this driver are a device's
/// `NominalSampleRate` (one of the six rates this driver advertises) and
/// `BufferFrameSize` (clamped to `[MIN_BUFFER_FRAMES, MAX_BUFFER_FRAMES]`).
pub fn set_property_data(
    id: AudioObjectID,
    a: &AudioObjectPropertyAddress,
    data: &[u8],
) -> Result<(), OSStatus> {
    let ObjectKind::Device(dev) = classify(id) else {
        return Err(ERR_BAD_OBJECT);
    };
    // Confirms this selector is a real device property before deciding
    // whether it's one of the two this driver allows setting. Keeps "not a
    // property of this object" (ERR_UNKNOWN_PROPERTY) and "a real property,
    // but read-only" (ERR_UNSUPPORTED_OPERATION below) as distinct, and
    // consistent with what `has_property`/`is_property_settable` already
    // say about the same selector.
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
        // TODO: the HAL caches NominalSampleRate/VirtualFormat after first
        // reading them. Changing the rate here without telling the host
        // leaves that cache out of step with what `'sfmt'`/`'pft '` now
        // report. Doing this properly needs the `AudioServerPlugInHostRef`
        // handed to `Initialize` plumbed through to here so this call can
        // invoke `RequestDeviceConfigurationChange` (and, once inside the
        // approved change, fire the host's `PropertiesChanged` notification
        // for `'nsrt'`/`'sfmt'`/`'pft '`). No host reference reaches
        // `engine::properties` today, so the rate is applied synchronously.
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

    // `ERR_UNKNOWN_PROPERTY`/`ERR_BAD_OBJECT` are computed via
    // `fourcc(b"...")` (mirroring `ffi::plugin`'s own constants). Pinning
    // them against the exact decimal values from the SDK headers means the
    // `fourcc` computation can never silently change the wire value a real
    // HAL would see.
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

    // A device's Streams/OwnedObjects list must be filtered by the scope
    // of the query — the input scope must never see the output stream's
    // ID and vice versa.
    #[test]
    fn streams_property_is_filtered_by_scope() {
        // Device 0 (id 10): out_stream_id = 11, in_stream_id = 12.
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

        // OwnedObjects must behave identically.
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

    // `kAudioDevicePropertyIcon` must be a known, read-only device
    // property that reports a pointer-sized (`CFURLRef`) payload.
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

    // Uses device index 4 (id 50), which no other test in this file
    // reads or writes the NominalSampleRate of — `STATE` is a shared
    // `static`, and `cargo test` runs tests in the same process
    // concurrently, so a test that mutates a device another test also
    // inspects would be a data race waiting to flake.
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

    // Uses device index 1 (id 20), exclusive to this test for the same
    // reason as above.
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

    // Setting a real-but-read-only device property must be reported as
    // unsupported, not as an unknown property (has_property is true for it).
    #[test]
    fn setting_a_known_readonly_property_is_unsupported_not_unknown() {
        assert!(has_property(10, &addr(fourcc(b"lnam"))));
        assert_eq!(
            set_property_data(10, &addr(fourcc(b"lnam")), &[0u8; 8]),
            Err(ERR_UNSUPPORTED_OPERATION)
        );
        // A selector that isn't a device property at all is still the
        // other error.
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
        // Guards against the easy size confusion: a bare ASBD array would
        // be 240 bytes, but the Available* format lists are
        // AudioStreamRangedDescription arrays (336 bytes).
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

    // Table-driven coverage of every selector the plugin, device, and
    // stream objects answer, checked against the object kind that answers
    // it and the exact byte size the wire format requires. The count
    // assertion at the bottom guards against a property being dropped
    // from the table.
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

        // Device 0 (id 10), unscoped address => `stm#`/`ownd` answer with
        // both streams (2 elements).
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

        // Stream object: device 0's output stream (id 11).
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
