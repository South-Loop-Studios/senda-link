/* Prints the SDK's sizes, offsets and four-character codes for tests/abi.rs.
 * The static initialiser below is the signature check: a wrongly-typed
 * function assigned to a struct member fails -Werror=incompatible-pointer-types.
 *
 * LIMITATION: every vtable member is an 8-byte pointer, so transposing two
 * same-signature members (BeginIOOperation and EndIOOperation, say) passes
 * every check here. Keep the stub list in the header's order, by eye. */
#include <CoreAudio/AudioServerPlugIn.h>
#include <CoreAudio/AudioHardware.h> /* kAudioHardwarePropertyMixStereoToMono */
#include <CoreAudio/CoreAudioTypes.h> /* kAudioFormatLinearPCM */
#include <mach/mach_time.h>
#include <stdio.h>
#include <stddef.h>

/* The same signature check for the two mach_time.h entry points. */
static uint64_t (*p_mach_absolute_time)(void) = mach_absolute_time;
static kern_return_t (*p_mach_timebase_info)(mach_timebase_info_t) = mach_timebase_info;

static HRESULT p_QueryInterface(void* d, REFIID i, LPVOID* o) { (void)d; (void)i; (void)o; return 0; }
static ULONG p_AddRef(void* d) { (void)d; return 0; }
static ULONG p_Release(void* d) { (void)d; return 0; }
static OSStatus p_Initialize(AudioServerPlugInDriverRef d, AudioServerPlugInHostRef h) {
    (void)d; (void)h; return 0;
}
static OSStatus p_CreateDevice(AudioServerPlugInDriverRef d, CFDictionaryRef desc,
                               const AudioServerPlugInClientInfo* ci, AudioObjectID* out) {
    (void)d; (void)desc; (void)ci; (void)out; return 0;
}
static OSStatus p_DestroyDevice(AudioServerPlugInDriverRef d, AudioObjectID o) {
    (void)d; (void)o; return 0;
}
static OSStatus p_AddDeviceClient(AudioServerPlugInDriverRef d, AudioObjectID o,
                                  const AudioServerPlugInClientInfo* ci) {
    (void)d; (void)o; (void)ci; return 0;
}
static OSStatus p_RemoveDeviceClient(AudioServerPlugInDriverRef d, AudioObjectID o,
                                     const AudioServerPlugInClientInfo* ci) {
    (void)d; (void)o; (void)ci; return 0;
}
static OSStatus p_PerformDeviceConfigurationChange(AudioServerPlugInDriverRef d, AudioObjectID o,
                                                   UInt64 a, void* i) {
    (void)d; (void)o; (void)a; (void)i; return 0;
}
static OSStatus p_AbortDeviceConfigurationChange(AudioServerPlugInDriverRef d, AudioObjectID o,
                                                 UInt64 a, void* i) {
    (void)d; (void)o; (void)a; (void)i; return 0;
}
static Boolean p_HasProperty(AudioServerPlugInDriverRef d, AudioObjectID o, pid_t p,
                             const AudioObjectPropertyAddress* a) {
    (void)d; (void)o; (void)p; (void)a; return 0;
}
static OSStatus p_IsPropertySettable(AudioServerPlugInDriverRef d, AudioObjectID o, pid_t p,
                                     const AudioObjectPropertyAddress* a, Boolean* s) {
    (void)d; (void)o; (void)p; (void)a; (void)s; return 0;
}
static OSStatus p_GetPropertyDataSize(AudioServerPlugInDriverRef d, AudioObjectID o, pid_t p,
                                      const AudioObjectPropertyAddress* a, UInt32 qs,
                                      const void* q, UInt32* os) {
    (void)d; (void)o; (void)p; (void)a; (void)qs; (void)q; (void)os; return 0;
}
static OSStatus p_GetPropData(AudioServerPlugInDriverRef d, AudioObjectID o, pid_t p,
                              const AudioObjectPropertyAddress* a, UInt32 qs,
                              const void* q, UInt32 ds, UInt32* os, void* od) {
    (void)d; (void)o; (void)p; (void)a; (void)qs; (void)q; (void)ds; (void)os; (void)od;
    return 0;
}
static OSStatus p_SetPropertyData(AudioServerPlugInDriverRef d, AudioObjectID o, pid_t p,
                                  const AudioObjectPropertyAddress* a, UInt32 qs,
                                  const void* q, UInt32 ds, const void* id) {
    (void)d; (void)o; (void)p; (void)a; (void)qs; (void)q; (void)ds; (void)id; return 0;
}
static OSStatus p_StartIO(AudioServerPlugInDriverRef d, AudioObjectID o, UInt32 c) {
    (void)d; (void)o; (void)c; return 0;
}
static OSStatus p_StopIO(AudioServerPlugInDriverRef d, AudioObjectID o, UInt32 c) {
    (void)d; (void)o; (void)c; return 0;
}
static OSStatus p_GetZeroTS(AudioServerPlugInDriverRef d, AudioObjectID o, UInt32 c,
                            Float64* st, UInt64* ht, UInt64* sd) {
    (void)d; (void)o; (void)c; (void)st; (void)ht; (void)sd; return 0;
}
static OSStatus p_WillDoIOOperation(AudioServerPlugInDriverRef d, AudioObjectID o, UInt32 c, UInt32 op,
                                    Boolean* wd, Boolean* wip) {
    (void)d; (void)o; (void)c; (void)op; (void)wd; (void)wip; return 0;
}
static OSStatus p_BeginIOOperation(AudioServerPlugInDriverRef d, AudioObjectID o, UInt32 c, UInt32 op,
                                   UInt32 bs, const AudioServerPlugInIOCycleInfo* ci) {
    (void)d; (void)o; (void)c; (void)op; (void)bs; (void)ci; return 0;
}
static OSStatus p_DoIO(AudioServerPlugInDriverRef d, AudioObjectID o, AudioObjectID s,
                       UInt32 c, UInt32 op, UInt32 bs,
                       const AudioServerPlugInIOCycleInfo* ci, void* mb, void* sb) {
    (void)d; (void)o; (void)s; (void)c; (void)op; (void)bs; (void)ci; (void)mb; (void)sb;
    return 0;
}
static OSStatus p_EndIOOperation(AudioServerPlugInDriverRef d, AudioObjectID o, UInt32 c, UInt32 op,
                                 UInt32 bs, const AudioServerPlugInIOCycleInfo* ci) {
    (void)d; (void)o; (void)c; (void)op; (void)bs; (void)ci; return 0;
}

static AudioServerPlugInDriverInterface probe = {
    .QueryInterface   = p_QueryInterface,
    .AddRef           = p_AddRef,
    .Release          = p_Release,
    .Initialize       = p_Initialize,
    .CreateDevice     = p_CreateDevice,
    .DestroyDevice    = p_DestroyDevice,
    .AddDeviceClient  = p_AddDeviceClient,
    .RemoveDeviceClient = p_RemoveDeviceClient,
    .PerformDeviceConfigurationChange = p_PerformDeviceConfigurationChange,
    .AbortDeviceConfigurationChange   = p_AbortDeviceConfigurationChange,
    .HasProperty      = p_HasProperty,
    .IsPropertySettable = p_IsPropertySettable,
    .GetPropertyDataSize = p_GetPropertyDataSize,
    .GetPropertyData  = p_GetPropData,
    .SetPropertyData  = p_SetPropertyData,
    .StartIO          = p_StartIO,
    .StopIO           = p_StopIO,
    .GetZeroTimeStamp = p_GetZeroTS,
    .WillDoIOOperation = p_WillDoIOOperation,
    .BeginIOOperation = p_BeginIOOperation,
    .DoIOOperation    = p_DoIO,
    .EndIOOperation   = p_EndIOOperation,
};

int main(void) {
    (void)probe;
    (void)p_mach_absolute_time;
    (void)p_mach_timebase_info;
    printf("iface_size=%zu\n", sizeof(AudioServerPlugInDriverInterface));
    printf("timebase_size=%zu\n", sizeof(mach_timebase_info_data_t));
    printf("timebase_off_numer=%zu\n", offsetof(mach_timebase_info_data_t, numer));
    printf("timebase_off_denom=%zu\n", offsetof(mach_timebase_info_data_t, denom));
    printf("timestamp_size=%zu\n", sizeof(AudioTimeStamp));
    printf("smpte_size=%zu\n", sizeof(SMPTETime));
    printf("propaddr_size=%zu\n", sizeof(AudioObjectPropertyAddress));
    printf("cycleinfo_size=%zu\n", sizeof(AudioServerPlugInIOCycleInfo));
    printf("clientinfo_size=%zu\n", sizeof(AudioServerPlugInClientInfo));
    printf("asbd_size=%zu\n", sizeof(AudioStreamBasicDescription));
    printf("valuerange_size=%zu\n", sizeof(AudioValueRange));
    printf("rangedasbd_size=%zu\n", sizeof(AudioStreamRangedDescription));
    printf("rangedasbd_off_format=%zu\n", offsetof(AudioStreamRangedDescription, mFormat));
    printf("rangedasbd_off_samplerate_range=%zu\n", offsetof(AudioStreamRangedDescription, mSampleRateRange));

    printf("ts_off_hosttime=%zu\n", offsetof(AudioTimeStamp, mHostTime));
    printf("ts_off_ratescalar=%zu\n", offsetof(AudioTimeStamp, mRateScalar));
    printf("ts_off_wordclocktime=%zu\n", offsetof(AudioTimeStamp, mWordClockTime));
    printf("ts_off_smpte=%zu\n", offsetof(AudioTimeStamp, mSMPTETime));
    printf("ts_off_flags=%zu\n", offsetof(AudioTimeStamp, mFlags));

    printf("ci_off_counter=%zu\n", offsetof(AudioServerPlugInIOCycleInfo, mIOCycleCounter));
    printf("ci_off_buffersize=%zu\n",
           offsetof(AudioServerPlugInIOCycleInfo, mNominalIOBufferFrameSize));
    printf("ci_off_currenttime=%zu\n", offsetof(AudioServerPlugInIOCycleInfo, mCurrentTime));
    printf("ci_off_inputtime=%zu\n", offsetof(AudioServerPlugInIOCycleInfo, mInputTime));
    printf("ci_off_outputtime=%zu\n", offsetof(AudioServerPlugInIOCycleInfo, mOutputTime));
    printf("ci_off_mainhostticks=%zu\n",
           offsetof(AudioServerPlugInIOCycleInfo, mMainHostTicksPerFrame));
    printf("ci_off_devicehostticks=%zu\n",
           offsetof(AudioServerPlugInIOCycleInfo, mDeviceHostTicksPerFrame));

    printf("clientinfo_off_processid=%zu\n",
           offsetof(AudioServerPlugInClientInfo, mProcessID));
    printf("clientinfo_off_nativeendian=%zu\n",
           offsetof(AudioServerPlugInClientInfo, mIsNativeEndian));
    printf("clientinfo_off_bundleid=%zu\n",
           offsetof(AudioServerPlugInClientInfo, mBundleID));

    /* All 22 members, in declaration order. */
    printf("iface_off_queryinterface=%zu\n",
           offsetof(AudioServerPlugInDriverInterface, QueryInterface));
    printf("iface_off_addref=%zu\n",
           offsetof(AudioServerPlugInDriverInterface, AddRef));
    printf("iface_off_release=%zu\n",
           offsetof(AudioServerPlugInDriverInterface, Release));
    printf("iface_off_initialize=%zu\n",
           offsetof(AudioServerPlugInDriverInterface, Initialize));
    printf("iface_off_createdevice=%zu\n",
           offsetof(AudioServerPlugInDriverInterface, CreateDevice));
    printf("iface_off_destroydevice=%zu\n",
           offsetof(AudioServerPlugInDriverInterface, DestroyDevice));
    printf("iface_off_adddeviceclient=%zu\n",
           offsetof(AudioServerPlugInDriverInterface, AddDeviceClient));
    printf("iface_off_removedeviceclient=%zu\n",
           offsetof(AudioServerPlugInDriverInterface, RemoveDeviceClient));
    printf("iface_off_performdeviceconfigurationchange=%zu\n",
           offsetof(AudioServerPlugInDriverInterface, PerformDeviceConfigurationChange));
    printf("iface_off_abortdeviceconfigurationchange=%zu\n",
           offsetof(AudioServerPlugInDriverInterface, AbortDeviceConfigurationChange));
    printf("iface_off_hasproperty=%zu\n",
           offsetof(AudioServerPlugInDriverInterface, HasProperty));
    printf("iface_off_ispropertysettable=%zu\n",
           offsetof(AudioServerPlugInDriverInterface, IsPropertySettable));
    printf("iface_off_getpropertydatasize=%zu\n",
           offsetof(AudioServerPlugInDriverInterface, GetPropertyDataSize));
    printf("iface_off_getpropertydata=%zu\n",
           offsetof(AudioServerPlugInDriverInterface, GetPropertyData));
    printf("iface_off_setpropertydata=%zu\n",
           offsetof(AudioServerPlugInDriverInterface, SetPropertyData));
    printf("iface_off_startio=%zu\n",
           offsetof(AudioServerPlugInDriverInterface, StartIO));
    printf("iface_off_stopio=%zu\n",
           offsetof(AudioServerPlugInDriverInterface, StopIO));
    printf("iface_off_getzerotimestamp=%zu\n",
           offsetof(AudioServerPlugInDriverInterface, GetZeroTimeStamp));
    printf("iface_off_willdoiooperation=%zu\n",
           offsetof(AudioServerPlugInDriverInterface, WillDoIOOperation));
    printf("iface_off_beginiooperation=%zu\n",
           offsetof(AudioServerPlugInDriverInterface, BeginIOOperation));
    printf("iface_off_doio=%zu\n",
           offsetof(AudioServerPlugInDriverInterface, DoIOOperation));
    printf("iface_off_endiooperation=%zu\n",
           offsetof(AudioServerPlugInDriverInterface, EndIOOperation));

    printf("op_write_mix=%u\n", (unsigned)kAudioServerPlugInIOOperationWriteMix);
    printf("op_read_input=%u\n", (unsigned)kAudioServerPlugInIOOperationReadInput);

    /* 'stmo' is a plausible look-alike for Owner's 'stdv'; both are printed. */
    printf("prop_owner=%u\n", (unsigned)kAudioObjectPropertyOwner);
    printf("prop_mix_stereo_to_mono=%u\n", (unsigned)kAudioHardwarePropertyMixStereoToMono);

    printf("class_object=%u\n", (unsigned)kAudioObjectClassID);
    printf("class_plugin=%u\n", (unsigned)kAudioPlugInClassID);
    printf("class_device=%u\n", (unsigned)kAudioDeviceClassID);
    printf("class_stream=%u\n", (unsigned)kAudioStreamClassID);
    printf("prop_base_class=%u\n", (unsigned)kAudioObjectPropertyBaseClass);
    printf("prop_class=%u\n", (unsigned)kAudioObjectPropertyClass);
    printf("prop_owned_objects=%u\n", (unsigned)kAudioObjectPropertyOwnedObjects);
    printf("prop_control_list=%u\n", (unsigned)kAudioObjectPropertyControlList);
    printf("prop_manufacturer=%u\n", (unsigned)kAudioObjectPropertyManufacturer);
    printf("prop_name=%u\n", (unsigned)kAudioObjectPropertyName);
    printf("scope_input=%u\n", (unsigned)kAudioObjectPropertyScopeInput);
    printf("scope_output=%u\n", (unsigned)kAudioObjectPropertyScopeOutput);
    /* Not named in src/: anything that is not 'inpt'/'outp' is treated as global. */
    printf("scope_global=%u\n", (unsigned)kAudioObjectPropertyScopeGlobal);

    printf("dev_device_list=%u\n", (unsigned)kAudioPlugInPropertyDeviceList);
    printf("dev_translate_uid=%u\n", (unsigned)kAudioPlugInPropertyTranslateUIDToDevice);
    printf("dev_resource_bundle=%u\n", (unsigned)kAudioPlugInPropertyResourceBundle);
    printf("dev_uid=%u\n", (unsigned)kAudioDevicePropertyDeviceUID);
    printf("dev_model_uid=%u\n", (unsigned)kAudioDevicePropertyModelUID);
    printf("dev_transport_type=%u\n", (unsigned)kAudioDevicePropertyTransportType);
    printf("dev_transport_virtual=%u\n", (unsigned)kAudioDeviceTransportTypeVirtual);
    printf("dev_related_devices=%u\n", (unsigned)kAudioDevicePropertyRelatedDevices);
    printf("dev_is_alive=%u\n", (unsigned)kAudioDevicePropertyDeviceIsAlive);
    printf("dev_is_running=%u\n", (unsigned)kAudioDevicePropertyDeviceIsRunning);
    printf("dev_is_running_somewhere=%u\n", (unsigned)kAudioDevicePropertyDeviceIsRunningSomewhere);
    printf("dev_can_be_default=%u\n", (unsigned)kAudioDevicePropertyDeviceCanBeDefaultDevice);
    printf("dev_can_be_default_system=%u\n",
           (unsigned)kAudioDevicePropertyDeviceCanBeDefaultSystemDevice);
    printf("dev_latency=%u\n", (unsigned)kAudioDevicePropertyLatency);
    printf("dev_safety_offset=%u\n", (unsigned)kAudioDevicePropertySafetyOffset);
    printf("dev_streams=%u\n", (unsigned)kAudioDevicePropertyStreams);
    printf("dev_clock_domain=%u\n", (unsigned)kAudioDevicePropertyClockDomain);
    printf("dev_nominal_sample_rate=%u\n", (unsigned)kAudioDevicePropertyNominalSampleRate);
    printf("dev_available_sample_rates=%u\n",
           (unsigned)kAudioDevicePropertyAvailableNominalSampleRates);
    printf("dev_buffer_frame_size=%u\n", (unsigned)kAudioDevicePropertyBufferFrameSize);
    printf("dev_buffer_frame_size_range=%u\n",
           (unsigned)kAudioDevicePropertyBufferFrameSizeRange);
    printf("dev_is_hidden=%u\n", (unsigned)kAudioDevicePropertyIsHidden);
    printf("dev_icon=%u\n", (unsigned)kAudioDevicePropertyIcon);
    printf("dev_preferred_stereo=%u\n",
           (unsigned)kAudioDevicePropertyPreferredChannelsForStereo);
    printf("dev_zero_timestamp_period=%u\n", (unsigned)kAudioDevicePropertyZeroTimeStampPeriod);

    printf("stream_is_active=%u\n", (unsigned)kAudioStreamPropertyIsActive);
    printf("stream_direction=%u\n", (unsigned)kAudioStreamPropertyDirection);
    printf("stream_terminal_type=%u\n", (unsigned)kAudioStreamPropertyTerminalType);
    printf("stream_starting_channel=%u\n", (unsigned)kAudioStreamPropertyStartingChannel);
    printf("stream_virtual_format=%u\n", (unsigned)kAudioStreamPropertyVirtualFormat);
    printf("stream_physical_format=%u\n", (unsigned)kAudioStreamPropertyPhysicalFormat);
    printf("stream_available_virtual_formats=%u\n",
           (unsigned)kAudioStreamPropertyAvailableVirtualFormats);
    printf("stream_available_physical_formats=%u\n",
           (unsigned)kAudioStreamPropertyAvailablePhysicalFormats);

    printf("format_linear_pcm=%u\n", (unsigned)kAudioFormatLinearPCM);

    printf("err_unknown_property=%u\n", (unsigned)kAudioHardwareUnknownPropertyError);
    printf("err_unsupported_operation=%u\n", (unsigned)kAudioHardwareUnsupportedOperationError);
    printf("err_bad_object=%u\n", (unsigned)kAudioHardwareBadObjectError);
    printf("err_illegal_operation=%u\n", (unsigned)kAudioHardwareIllegalOperationError);
    printf("err_bad_property_size=%u\n", (unsigned)kAudioHardwareBadPropertySizeError);
    printf("err_unspecified=%u\n", (unsigned)kAudioHardwareUnspecifiedError);

    /* Live values, not just layout: an inverted ratio passes every offset
     * check above and is ~40x wrong on Apple Silicon. */
    mach_timebase_info_data_t tbi;
    kern_return_t tbi_kr = mach_timebase_info(&tbi);
    printf("timebase_live_kr=%u\n", (unsigned)tbi_kr);
    printf("timebase_live_numer=%u\n", (unsigned)tbi.numer);
    printf("timebase_live_denom=%u\n", (unsigned)tbi.denom);

    return 0;
}
