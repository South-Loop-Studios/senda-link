//! Compiles and runs `abi_probe.c` against Apple's SDK, then compares the
//! sizes, offsets and four-character codes it prints with the declarations
//! in `senda_link::ffi::types`.

use senda_link::ffi::types::*;
use std::collections::HashMap;
use std::process::Command;
use std::sync::OnceLock;

/// Once per test process: concurrent `clang` runs writing one output path would race.
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

    // All 22 members in declaration order. Insertions, deletions and reorders
    // are caught; a transposition of two same-signature members is not, since
    // every slot is an 8-byte pointer.
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

/// A wrong constant here is a device that enumerates, keeps time and passes no audio.
#[test]
fn fourcc_matches_every_remaining_production_constant() {
    let p = probe();
    let g = |k: &str| *p.get(k).unwrap_or_else(|| panic!("probe missing {k}"));

    let checks: &[(&[u8; 4], &str, &str)] = &[
        (b"aobj", "class_object", "kAudioObjectClassID"),
        (b"aplg", "class_plugin", "kAudioPlugInClassID"),
        (b"adev", "class_device", "kAudioDeviceClassID"),
        (b"astr", "class_stream", "kAudioStreamClassID"),
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
        (b"inpt", "scope_input", "kAudioObjectPropertyScopeInput"),
        (b"outp", "scope_output", "kAudioObjectPropertyScopeOutput"),
        (b"glob", "scope_global", "kAudioObjectPropertyScopeGlobal"),
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
        (
            b"ring",
            "dev_zero_timestamp_period",
            "kAudioDevicePropertyZeroTimeStampPeriod",
        ),
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
        (b"lpcm", "format_linear_pcm", "kAudioFormatLinearPCM"),
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

/// Layout equality says nothing about the values read through the `extern "C"`
/// declaration; an inverted ratio passes every offset check and is ~40x wrong.
#[test]
fn mach_timebase_info_matches_a_live_probe_call() {
    let p = probe();
    let g = |k: &str| *p.get(k).unwrap_or_else(|| panic!("probe missing {k}"));

    let mut info = MachTimebaseInfo::default();
    // SAFETY: `info` is a valid, exclusively borrowed `MachTimebaseInfo`.
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
    assert!(
        info.numer > 0 && info.denom > 0,
        "a zero numer/denom would divide by zero downstream"
    );
}
