# Changelog

Notable changes to Senda Link, newest first. Each entry gives the version and date, the issue
as a user would have met it, and the fix.

## 1.0.1 (2026-11-10)

### A sample-rate change did not stick

**Issue.** Changing a Senda Link device's sample rate from a host application could snap back
to the previous rate, most visibly on the output side. Two things were wrong:

- The driver changed its own rate as soon as it was asked, without telling CoreAudio. CoreAudio
  keeps its own copy of the rate and of both stream formats, so applications were never notified
  of the change, and while audio was running CoreAudio went on scheduling IO at the old rate
  against a driver clock running at the new one.
- Setting the rate through a stream's format, which is how some applications do it, was refused
  outright.

**Fix.** A rate change is now requested of CoreAudio and applied only when CoreAudio performs
it, with IO stopped. CoreAudio then re-reads the device and notifies every application. A
stream's virtual or physical format is settable, and is treated as a rate change on the device
that owns it.

Measured on the 8-channel device with audio running, 48 kHz to 96 kHz:

| | Before | After |
|---|---|---|
| Notifications after a rate change | none | device rate and both stream formats, about 40 ms later |
| IO cycles per half second at 96 kHz | erratic, 41 to 80 | steady 94 to 95 (expected about 94) |
| Setting a stream's format | refused | accepted; the device and both streams follow |
