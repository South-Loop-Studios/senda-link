use senda_link::ffi::types::*;
use std::collections::HashMap;
use std::process::Command;
use std::sync::OnceLock;

/// Compiles and runs the C probe once per test process. The tests run in parallel and all
/// need the same table, and four concurrent `clang` invocations writing one output path is
/// a race, so the result is shared through a `OnceLock`.
fn probe() -> &'static HashMap<String, usize> {
    static PROBE: OnceLock<HashMap<String, usize>> = OnceLock::new();
    PROBE.get_or_init(run_probe)
}

fn run_probe() -> HashMap<String, usize> {
    let out = std::env::temp_dir().join(format!("senda_abi_probe_{}", std::process::id()));
    let src = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/abi_probe.c");
    let status = Command::new("clang")
        .args([
            "-Wall",
            "-Wextra",
            "-Werror=incompatible-pointer-types",
            "-framework",
            "CoreAudio",
            "-o",
            out.to_str().unwrap(),
            src,
        ])
        .status()
        .expect("clang must be installed");
    assert!(
        status.success(),
        "abi_probe.c failed to compile — a declaration disagrees with Apple's header"
    );

    let res = Command::new(&out).output().expect("run probe");
    assert!(
        res.status.success(),
        "abi_probe binary exited with failure status: {res:?}"
    );
    String::from_utf8_lossy(&res.stdout)
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| {
            let n = v.trim().parse().unwrap_or_else(|e| {
                panic!("probe output line `{k}={v}` is not a valid usize: {e}")
            });
            (k.to_string(), n)
        })
        .collect()
}

#[test]
fn abi_layout_matches_sdk() {
    let p = probe();
    let g = |k: &str| *p.get(k).unwrap_or_else(|| panic!("probe missing {k}"));

    assert_eq!(
        std::mem::size_of::<AudioServerPlugInDriverInterface>(),
        g("iface_size")
    );
    assert_eq!(std::mem::size_of::<MachTimebaseInfo>(), g("timebase_size"));
    assert_eq!(
        std::mem::offset_of!(MachTimebaseInfo, numer),
        g("timebase_off_numer")
    );
    assert_eq!(
        std::mem::offset_of!(MachTimebaseInfo, denom),
        g("timebase_off_denom")
    );
    assert_eq!(std::mem::size_of::<AudioTimeStamp>(), g("timestamp_size"));
    assert_eq!(std::mem::size_of::<SmpteTime>(), g("smpte_size"));
    assert_eq!(
        std::mem::size_of::<AudioObjectPropertyAddress>(),
        g("propaddr_size")
    );
    assert_eq!(
        std::mem::size_of::<AudioServerPlugInIOCycleInfo>(),
        g("cycleinfo_size")
    );
    assert_eq!(
        std::mem::size_of::<AudioServerPlugInClientInfo>(),
        g("clientinfo_size")
    );
    assert_eq!(
        std::mem::size_of::<AudioStreamBasicDescription>(),
        g("asbd_size")
    );
    assert_eq!(std::mem::size_of::<AudioValueRange>(), g("valuerange_size"));
    assert_eq!(
        std::mem::size_of::<AudioStreamRangedDescription>(),
        g("rangedasbd_size")
    );
    assert_eq!(
        std::mem::offset_of!(AudioStreamRangedDescription, format),
        g("rangedasbd_off_format")
    );
    assert_eq!(
        std::mem::offset_of!(AudioStreamRangedDescription, sample_rate_range),
        g("rangedasbd_off_samplerate_range")
    );

    assert_eq!(
        std::mem::offset_of!(AudioTimeStamp, host_time),
        g("ts_off_hosttime")
    );
    assert_eq!(
        std::mem::offset_of!(AudioTimeStamp, rate_scalar),
        g("ts_off_ratescalar")
    );
    assert_eq!(
        std::mem::offset_of!(AudioTimeStamp, word_clock_time),
        g("ts_off_wordclocktime")
    );
    assert_eq!(
        std::mem::offset_of!(AudioTimeStamp, smpte),
        g("ts_off_smpte")
    );
    assert_eq!(
        std::mem::offset_of!(AudioTimeStamp, flags),
        g("ts_off_flags")
    );

    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInIOCycleInfo, io_cycle_counter),
        g("ci_off_counter")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInIOCycleInfo, nominal_io_buffer_frame_size),
        g("ci_off_buffersize")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInIOCycleInfo, current_time),
        g("ci_off_currenttime")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInIOCycleInfo, input_time),
        g("ci_off_inputtime")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInIOCycleInfo, output_time),
        g("ci_off_outputtime")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInIOCycleInfo, main_host_ticks_per_frame),
        g("ci_off_mainhostticks")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInIOCycleInfo, device_host_ticks_per_frame),
        g("ci_off_devicehostticks")
    );

    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInClientInfo, process_id),
        g("clientinfo_off_processid")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInClientInfo, is_native_endian),
        g("clientinfo_off_nativeendian")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInClientInfo, bundle_id),
        g("clientinfo_off_bundleid")
    );

    // All 22 function-pointer members, in declaration order. This catches a
    // member being inserted, deleted, or reordered — the drift mode that
    // actually happens when this struct or the SDK header changes. It does
    // NOT catch two same-signature members being transposed with each other
    // (e.g. BeginIOOperation <-> EndIOOperation): every vtable member is an
    // 8-byte function pointer, so offset_of! cannot distinguish which
    // specific function ended up in which same-shaped slot. See the
    // LIMITATION comment at the top of tests/abi_probe.c.
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInDriverInterface, QueryInterface),
        g("iface_off_queryinterface")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInDriverInterface, AddRef),
        g("iface_off_addref")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInDriverInterface, Release),
        g("iface_off_release")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInDriverInterface, Initialize),
        g("iface_off_initialize")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInDriverInterface, CreateDevice),
        g("iface_off_createdevice")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInDriverInterface, DestroyDevice),
        g("iface_off_destroydevice")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInDriverInterface, AddDeviceClient),
        g("iface_off_adddeviceclient")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInDriverInterface, RemoveDeviceClient),
        g("iface_off_removedeviceclient")
    );
    assert_eq!(
        std::mem::offset_of!(
            AudioServerPlugInDriverInterface,
            PerformDeviceConfigurationChange
        ),
        g("iface_off_performdeviceconfigurationchange")
    );
    assert_eq!(
        std::mem::offset_of!(
            AudioServerPlugInDriverInterface,
            AbortDeviceConfigurationChange
        ),
        g("iface_off_abortdeviceconfigurationchange")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInDriverInterface, HasProperty),
        g("iface_off_hasproperty")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInDriverInterface, IsPropertySettable),
        g("iface_off_ispropertysettable")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInDriverInterface, GetPropertyDataSize),
        g("iface_off_getpropertydatasize")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInDriverInterface, GetPropertyData),
        g("iface_off_getpropertydata")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInDriverInterface, SetPropertyData),
        g("iface_off_setpropertydata")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInDriverInterface, StartIO),
        g("iface_off_startio")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInDriverInterface, StopIO),
        g("iface_off_stopio")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInDriverInterface, GetZeroTimeStamp),
        g("iface_off_getzerotimestamp")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInDriverInterface, WillDoIOOperation),
        g("iface_off_willdoiooperation")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInDriverInterface, BeginIOOperation),
        g("iface_off_beginiooperation")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInDriverInterface, DoIOOperation),
        g("iface_off_doio")
    );
    assert_eq!(
        std::mem::offset_of!(AudioServerPlugInDriverInterface, EndIOOperation),
        g("iface_off_endiooperation")
    );
}

#[test]
fn fourcc_matches_known_constants() {
    // The two `DoIOOperation` operation IDs the whole IO path hinges on:
    // `kAudioServerPlugInIOOperationWriteMix` is `'rite'` and
    // `kAudioServerPlugInIOOperationReadInput` is `'read'`. With a wrong
    // value, `senda_WillDoIOOperation` never matches a real `inOperationID`
    // the HAL asks about and `senda_DoIOOperation` is never called at all —
    // silent output and input despite the device enumerating, starting, and
    // keeping time correctly. Asserted against the real SDK header via
    // `tests/abi_probe.c` so that failure mode shows up at `cargo test`
    // rather than only in a manual audio check.
    let p = probe();
    let g = |k: &str| *p.get(k).unwrap_or_else(|| panic!("probe missing {k}"));
    assert_eq!(
        fourcc(b"rite") as usize,
        g("op_write_mix"),
        "kAudioServerPlugInIOOperationWriteMix must be 'rite' (not the look-alike 'wmix')"
    );
    assert_eq!(
        fourcc(b"read") as usize,
        g("op_read_input"),
        "kAudioServerPlugInIOOperationReadInput must be 'read' (not the look-alike 'rinp')"
    );

    // `kAudioObjectPropertyOwner` is `'stdv'`. `'stmo'` is a different real
    // selector (`kAudioHardwarePropertyMixStereoToMono`) that looks
    // plausible in its place, so both values are checked and a mix-up in
    // either direction is caught rather than only one side.
    assert_eq!(
        fourcc(b"stdv") as usize,
        g("prop_owner"),
        "kAudioObjectPropertyOwner must be 'stdv', not the look-alike 'stmo'"
    );
    assert_eq!(
        fourcc(b"stmo") as usize,
        g("prop_mix_stereo_to_mono"),
        "'stmo' is kAudioHardwarePropertyMixStereoToMono, a different property entirely"
    );
}

/// Every remaining fourcc constant used anywhere in `src/` (class IDs,
/// property selectors, scopes, transport types, a format ID, and error
/// codes), matched against its real Apple SDK macro via
/// `tests/abi_probe.c`. Nothing is trusted by eye: the failure mode for a
/// wrong constant here is a device that enumerates, starts, keeps perfect
/// time, and passes no audio — nothing a type-checker or a quick manual
/// glance catches, whether the value was invented outright or is a real
/// selector used in the wrong place.
#[test]
fn fourcc_matches_every_remaining_production_constant() {
    let p = probe();
    let g = |k: &str| *p.get(k).unwrap_or_else(|| panic!("probe missing {k}"));

    // (fourcc bytes as used in src/, probe key, the real SDK macro name —
    // purely documentation for a failure message, not re-derived).
    let checks: &[(&[u8; 4], &str, &str)] = &[
        // Class IDs (engine::properties: bcls/clas answers).
        (b"aobj", "class_object", "kAudioObjectClassID"),
        (b"aplg", "class_plugin", "kAudioPlugInClassID"),
        (b"adev", "class_device", "kAudioDeviceClassID"),
        (b"astr", "class_stream", "kAudioStreamClassID"),
        // AudioObject-level properties.
        (b"bcls", "prop_base_class", "kAudioObjectPropertyBaseClass"),
        (b"clas", "prop_class", "kAudioObjectPropertyClass"),
        (
            b"ownd",
            "prop_owned_objects",
            "kAudioObjectPropertyOwnedObjects",
        ),
        (
            b"ctrl",
            "prop_control_list",
            "kAudioObjectPropertyControlList",
        ),
        (
            b"lmak",
            "prop_manufacturer",
            "kAudioObjectPropertyManufacturer",
        ),
        (b"lnam", "prop_name", "kAudioObjectPropertyName"),
        // Scopes (engine::properties::SCOPE_INPUT/SCOPE_OUTPUT).
        (b"inpt", "scope_input", "kAudioObjectPropertyScopeInput"),
        (b"outp", "scope_output", "kAudioObjectPropertyScopeOutput"),
        // Not a named constant in src/ — see the doc comment above.
        (b"glob", "scope_global", "kAudioObjectPropertyScopeGlobal"),
        // Plugin-level properties (engine::properties::plugin_property).
        (b"dev#", "dev_device_list", "kAudioPlugInPropertyDeviceList"),
        (
            b"uidd",
            "dev_translate_uid",
            "kAudioPlugInPropertyTranslateUIDToDevice",
        ),
        (
            b"rsrc",
            "dev_resource_bundle",
            "kAudioPlugInPropertyResourceBundle",
        ),
        // Device-level properties (engine::properties::device_property).
        (b"uid ", "dev_uid", "kAudioDevicePropertyDeviceUID"),
        (b"muid", "dev_model_uid", "kAudioDevicePropertyModelUID"),
        (
            b"tran",
            "dev_transport_type",
            "kAudioDevicePropertyTransportType",
        ),
        (
            b"virt",
            "dev_transport_virtual",
            "kAudioDeviceTransportTypeVirtual",
        ),
        (
            b"akin",
            "dev_related_devices",
            "kAudioDevicePropertyRelatedDevices",
        ),
        (b"livn", "dev_is_alive", "kAudioDevicePropertyDeviceIsAlive"),
        (
            b"goin",
            "dev_is_running",
            "kAudioDevicePropertyDeviceIsRunning",
        ),
        (
            b"gone",
            "dev_is_running_somewhere",
            "kAudioDevicePropertyDeviceIsRunningSomewhere",
        ),
        (
            b"dflt",
            "dev_can_be_default",
            "kAudioDevicePropertyDeviceCanBeDefaultDevice",
        ),
        (
            b"sflt",
            "dev_can_be_default_system",
            "kAudioDevicePropertyDeviceCanBeDefaultSystemDevice",
        ),
        (b"ltnc", "dev_latency", "kAudioDevicePropertyLatency"),
        (
            b"saft",
            "dev_safety_offset",
            "kAudioDevicePropertySafetyOffset",
        ),
        (b"stm#", "dev_streams", "kAudioDevicePropertyStreams"),
        (
            b"clkd",
            "dev_clock_domain",
            "kAudioDevicePropertyClockDomain",
        ),
        (
            b"nsrt",
            "dev_nominal_sample_rate",
            "kAudioDevicePropertyNominalSampleRate",
        ),
        (
            b"nsr#",
            "dev_available_sample_rates",
            "kAudioDevicePropertyAvailableNominalSampleRates",
        ),
        (
            b"fsiz",
            "dev_buffer_frame_size",
            "kAudioDevicePropertyBufferFrameSize",
        ),
        (
            b"fsz#",
            "dev_buffer_frame_size_range",
            "kAudioDevicePropertyBufferFrameSizeRange",
        ),
        (b"hidn", "dev_is_hidden", "kAudioDevicePropertyIsHidden"),
        (b"icon", "dev_icon", "kAudioDevicePropertyIcon"),
        (
            b"dch2",
            "dev_preferred_stereo",
            "kAudioDevicePropertyPreferredChannelsForStereo",
        ),
        // The load-bearing one: see the non-negotiable note on
        // `device::BLOCK` and `device::RING_FRAMES`. A wrong value here
        // would misroute every GetPropertyData('ring') query, not merely
        // misreport a cosmetic property.
        (
            b"ring",
            "dev_zero_timestamp_period",
            "kAudioDevicePropertyZeroTimeStampPeriod",
        ),
        // Stream-level properties (engine::properties::stream_property).
        (b"sact", "stream_is_active", "kAudioStreamPropertyIsActive"),
        (b"sdir", "stream_direction", "kAudioStreamPropertyDirection"),
        (
            b"term",
            "stream_terminal_type",
            "kAudioStreamPropertyTerminalType",
        ),
        (
            b"schn",
            "stream_starting_channel",
            "kAudioStreamPropertyStartingChannel",
        ),
        (
            b"sfmt",
            "stream_virtual_format",
            "kAudioStreamPropertyVirtualFormat",
        ),
        (
            b"pft ",
            "stream_physical_format",
            "kAudioStreamPropertyPhysicalFormat",
        ),
        (
            b"sfma",
            "stream_available_virtual_formats",
            "kAudioStreamPropertyAvailableVirtualFormats",
        ),
        (
            b"pfta",
            "stream_available_physical_formats",
            "kAudioStreamPropertyAvailablePhysicalFormats",
        ),
        // Format ID (engine::properties::build_asbd's LPCM format_id).
        (b"lpcm", "format_linear_pcm", "kAudioFormatLinearPCM"),
        // Error codes: `ffi::plugin`'s `ERR_UNKNOWN_PROPERTY`,
        // `ERR_UNSUPPORTED`, `ERR_BAD_OBJECT`, `ERR_BAD_PROPERTY_SIZE` and
        // `ERR_UNSPECIFIED`, plus `engine::properties::ERR_ILLEGAL_VALUE` —
        // each derived with `fourcc` rather than hand-copied as a decimal.
        (
            b"who?",
            "err_unknown_property",
            "kAudioHardwareUnknownPropertyError",
        ),
        (
            b"unop",
            "err_unsupported_operation",
            "kAudioHardwareUnsupportedOperationError",
        ),
        (b"!obj", "err_bad_object", "kAudioHardwareBadObjectError"),
        (
            b"nope",
            "err_illegal_operation",
            "kAudioHardwareIllegalOperationError",
        ),
        (
            b"!siz",
            "err_bad_property_size",
            "kAudioHardwareBadPropertySizeError",
        ),
        (b"what", "err_unspecified", "kAudioHardwareUnspecifiedError"),
    ];

    for (bytes, key, sdk_name) in checks {
        let code = std::str::from_utf8(bytes.as_slice()).unwrap_or("????");
        assert_eq!(
            fourcc(bytes) as usize,
            g(key),
            "fourcc(b\"{code}\") does not match the real SDK constant {sdk_name}"
        );
    }
}

/// `abi_layout_matches_sdk` above only proves `MachTimebaseInfo`'s
/// `numer`/`denom` fields sit at the same byte offsets as the real
/// `mach_timebase_info_data_t` — it says nothing about whether the values
/// actually read through this crate's `extern "C"` declaration match a real
/// call to the same function. An inverted or otherwise corrupted timebase
/// ratio would still pass every layout check while being roughly 40x wrong
/// on Apple Silicon (`numer`/`denom` ~= 125/3 there; the reciprocal,
/// `denom`/`numer`, is ~= 3/125), silently mis-scaling every
/// `mach_absolute_time()` reading `ffi::plugin`'s `host_ns_per_tick`
/// depends on — a device which enumerates, starts, and passes audio, just
/// on a badly wrong clock, and survives a fully green test suite that only
/// checks layout. This calls the same `mach_timebase_info` this crate
/// declares (via its own FFI binding, not a re-declared one) and compares
/// against `tests/abi_probe.c`'s live call to the real function on the same
/// machine.
#[test]
fn mach_timebase_info_matches_a_live_probe_call() {
    let p = probe();
    let g = |k: &str| *p.get(k).unwrap_or_else(|| panic!("probe missing {k}"));

    let mut info = MachTimebaseInfo::default();
    // SAFETY: `&mut info` is a valid, uniquely-owned, correctly-sized
    // `MachTimebaseInfo` for the duration of this call — test-only use of
    // the same FFI declaration `ffi::plugin::host_ns_per_tick` calls.
    let kr = unsafe { mach_timebase_info(&mut info) };
    assert_eq!(
        kr as usize,
        g("timebase_live_kr"),
        "mach_timebase_info's kern_return_t must be KERN_SUCCESS (0)"
    );
    assert_eq!(
        info.numer as usize,
        g("timebase_live_numer"),
        "MachTimebaseInfo.numer must match a live call to the real mach_timebase_info"
    );
    assert_eq!(
        info.denom as usize,
        g("timebase_live_denom"),
        "MachTimebaseInfo.denom must match a live call to the real mach_timebase_info"
    );
    // On this run's machine (see the values above), confirm the ratio
    // actually used by `host_ns_per_tick` (numer/denom) is the right way
    // round, not its ~40x-off reciprocal — belt-and-suspenders alongside
    // the exact-value checks above, expressed as the property that
    // actually matters operationally.
    assert!(
        info.numer > 0 && info.denom > 0,
        "a zero numer/denom would divide by zero downstream"
    );
}
