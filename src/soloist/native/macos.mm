// A private tap of the owned Soloist process, followed by our existing DSP.
// Audio callbacks only move floats through bounded single-producer rings.
#import <Foundation/Foundation.h>
#import <CoreAudio/CoreAudio.h>
#import <CoreAudio/AudioHardwareTapping.h>
#import <CoreAudio/CATapDescription.h>
#import <AudioToolbox/AudioToolbox.h>
#include "audio.h"
#include <algorithm>
#include <array>
#include <atomic>
#include <chrono>
#include <thread>
namespace {
void check(OSStatus status) { if (status != noErr) throw status; }
AudioObjectPropertyAddress property(AudioObjectPropertySelector selector) {
    return {selector, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
}
template<class T> T get(AudioObjectID object, AudioObjectPropertySelector selector) {
    T value{}; UInt32 size = sizeof value; auto address = property(selector);
    check(AudioObjectGetPropertyData(object, &address, 0, nullptr, &size, &value));
    return value;
}
struct Ring {
    static constexpr size_t capacity = 16384;
    std::array<float, capacity> samples{};
    std::atomic<uint64_t> read{0}, write{0};
    // Only the consumer can discard. Never overwrite samples it is reading.
    void discard() { read.store(write.load(std::memory_order_acquire), std::memory_order_release); }
    bool push(const float *data, size_t count) {
        auto w = write.load(std::memory_order_relaxed);
        if (count > capacity - (w - read.load(std::memory_order_acquire))) return false;
        for (size_t i = 0; i < count; ++i) samples[(w + i) % capacity] = data[i];
        write.store(w + count, std::memory_order_release); return true;
    }
    size_t pop(float *data, size_t count) {
        auto r = read.load(std::memory_order_relaxed);
        count = std::min(count, static_cast<size_t>(write.load(std::memory_order_acquire) - r));
        for (size_t i = 0; i < count; ++i) data[i] = samples[(r + i) % capacity];
        read.store(r + count, std::memory_order_release); return count;
    }
};
struct API_AVAILABLE(macos(14.2)) Route {
    void *context; Control control;
    AudioObjectID tap = 0, device = 0, output = 0;
    AudioDeviceIOProcID io = nullptr;
    AudioQueueRef queue = nullptr;
    CATapDescription *description = nil;
    AudioStreamBasicDescription format{};
    Ring input, rendered;
    std::atomic<bool> audible{false}, overflow{false}, resetOutput{false};
    std::atomic<OSStatus> error{noErr};
    bool muted = false;
    Route(void *ctx, Control ctl) : context(ctx), control(ctl) {}
    ~Route() {
        audible.store(false);
        if (queue) { AudioQueueStop(queue, true); AudioQueueDispose(queue, true); }
        if (io) { AudioDeviceStop(device, io); AudioDeviceDestroyIOProcID(device, io); }
        if (device) AudioHardwareDestroyAggregateDevice(device);
        if (tap) AudioHardwareDestroyProcessTap(tap);
    }
    static OSStatus capture(AudioObjectID, const AudioTimeStamp *, const AudioBufferList *buffers,
                            const AudioTimeStamp *, AudioBufferList *, const AudioTimeStamp *, void *ctx) {
        auto &self = *static_cast<Route *>(ctx);
        if (!(self.control(self.context) & 2) || !buffers) return noErr;
        // initStereoMixdownOfProcesses delivers interleaved float32 stereo.
        if (buffers->mNumberBuffers != 1 || buffers->mBuffers[0].mNumberChannels != 2) {
            self.error.store(kAudioHardwareUnsupportedOperationError); return noErr;
        }
        const auto &buffer = buffers->mBuffers[0];
        if (buffer.mData && !self.input.push(static_cast<const float *>(buffer.mData), buffer.mDataByteSize / sizeof(float)))
            self.overflow.store(true);
        return noErr;
    }
    static void render(void *ctx, AudioQueueRef queue, AudioQueueBufferRef buffer) {
        auto &self = *static_cast<Route *>(ctx);
        auto data = static_cast<float *>(buffer->mAudioData);
        const size_t count = buffer->mAudioDataBytesCapacity / sizeof(float);
        size_t copied = 0;
        if (self.resetOutput.exchange(false)) self.rendered.discard();
        if (self.audible.load() && (self.control(self.context) & 2)) copied = self.rendered.pop(data, count);
        else self.rendered.discard();
        std::fill(data + copied, data + count, 0.f);
        buffer->mAudioDataByteSize = static_cast<UInt32>(count * sizeof(float));
        OSStatus result = AudioQueueEnqueueBuffer(queue, buffer, 0, nullptr);
        if (result) self.error.store(result);
    }
    bool open(pid_t pid) {
        AudioObjectID process = 0; UInt32 size = sizeof process;
        auto address = property(kAudioHardwarePropertyTranslatePIDToProcessObject);
        if (AudioObjectGetPropertyData(kAudioObjectSystemObject, &address, sizeof pid, &pid, &size, &process) || !process)
            return false; // Soloist has not opened its audio device yet.
        description = [[CATapDescription alloc] initStereoMixdownOfProcesses:@[@(process)]];
        description.name = @"Spotifast Soloist";
        description.privateTap = YES;
        // Waiting for permission must not silence ordinary playback.
        description.muteBehavior = CATapUnmuted;
        check(AudioHardwareCreateProcessTap(description, &tap));
        format = get<AudioStreamBasicDescription>(tap, kAudioTapPropertyFormat);
        if (format.mFormatID != kAudioFormatLinearPCM || !(format.mFormatFlags & kAudioFormatFlagIsFloat)
            || (format.mFormatFlags & (kAudioFormatFlagIsNonInterleaved | kAudioFormatFlagIsBigEndian))
            || format.mBitsPerChannel != 32 || format.mChannelsPerFrame != 2
            || format.mSampleRate < 8000 || format.mSampleRate > 192000)
            throw static_cast<OSStatus>(kAudioHardwareUnsupportedOperationError);
        output = get<AudioObjectID>(kAudioObjectSystemObject, kAudioHardwarePropertyDefaultOutputDevice);
        NSDictionary *aggregate = @{@kAudioAggregateDeviceNameKey:@"Spotifast Soloist (private)",
            @kAudioAggregateDeviceUIDKey:NSUUID.UUID.UUIDString, @kAudioAggregateDeviceIsPrivateKey:@YES,
            @kAudioAggregateDeviceTapAutoStartKey:@YES,
            @kAudioAggregateDeviceTapListKey:@[@{@kAudioSubTapUIDKey:description.UUID.UUIDString,
                                                @kAudioSubTapDriftCompensationKey:@YES}]};
        check(AudioHardwareCreateAggregateDevice((__bridge CFDictionaryRef)aggregate, &device));
        check(AudioDeviceCreateIOProcID(device, capture, this, &io));
        check(AudioDeviceStart(device, io));
        // Prepare a silent output before muting Soloist. The queue follows the
        // default device; a change restarts the route to renegotiate its format.
        check(AudioQueueNewOutput(&format, render, this, nullptr, nullptr, 0, &queue));
        check(AudioQueueSetParameter(queue, kAudioQueueParam_Volume, 1));
        const UInt32 bytes = static_cast<UInt32>(format.mSampleRate / 100) * 8;
        for (int i = 0; i < 3; ++i) {
            AudioQueueBufferRef buffer = nullptr;
            check(AudioQueueAllocateBuffer(queue, bytes, &buffer)); render(this, queue, buffer);
        }
        check(AudioQueueStart(queue, nullptr));
        return true;
    }
    void mute() {
        description.muteBehavior = CATapMutedWhenTapped;
        auto address = property(kAudioTapPropertyDescription);
        CATapDescription *value = description;
        check(AudioObjectSetPropertyData(tap, &address, 0, nullptr, sizeof value, &value));
        muted = true;
    }
    void run(Process process, Event event) {
        std::array<float, 1920> samples{};
        bool playing = false;
        auto nextCheck = std::chrono::steady_clock::now();
        while (!(control(context) & 1)) {
            check(error.load());
            bool wanted = (control(context) & 2) != 0;
            if (wanted != playing || overflow.exchange(false)) {
                audible.store(false); resetOutput.store(true); input.discard(); event(context, 2); playing = wanted;
            }
            auto now = std::chrono::steady_clock::now();
            if (now >= nextCheck) {
                nextCheck = now + std::chrono::milliseconds(500);
                auto current = get<AudioStreamBasicDescription>(tap, kAudioTapPropertyFormat);
                if (output != get<AudioObjectID>(kAudioObjectSystemObject, kAudioHardwarePropertyDefaultOutputDevice)
                    || current.mSampleRate != format.mSampleRate || current.mFormatFlags != format.mFormatFlags
                    || current.mChannelsPerFrame != format.mChannelsPerFrame)
                    throw static_cast<OSStatus>(kAudioHardwareBadDeviceError);
            }
            size_t count = input.pop(samples.data(), samples.size());
            if (playing && count) {
                if (!muted) {
                    if (!std::any_of(samples.begin(), samples.begin() + count, [](float value) { return value != 0.f; }))
                        continue; // Permission pending, denied, or the source is silent.
                    mute(); input.discard(); event(context, 2); event(context, 1);
                    continue; // Discard audio that was already heard on the dry route.
                }
                process(context, samples.data(), count, static_cast<uint32_t>(format.mSampleRate));
                if (!rendered.push(samples.data(), count)) overflow.store(true);
                audible.store(true);
            }
            std::this_thread::sleep_for(std::chrono::milliseconds(2));
        }
    }
};
int run(uint32_t pid, void *context, Control control, Process process, Event event) API_AVAILABLE(macos(14.2));
int run(uint32_t pid, void *context, Control control, Process process, Event event) {
    Route route(context, control);
    try {
        while (!(control(context) & 1) && !route.open(static_cast<pid_t>(pid)))
            std::this_thread::sleep_for(std::chrono::milliseconds(100));
        if (!(control(context) & 1)) route.run(process, event);
        return 0;
    } catch (OSStatus error) {
        route.audible.store(false);
        if (route.muted) {
            // Keep the dry source muted until the backend stops its child.
            event(context, 3);
            while (!(control(context) & 1)) std::this_thread::sleep_for(std::chrono::milliseconds(20));
        }
        return static_cast<int>(error);
    }
}
}
extern "C" int spotifast_soloist_audio(uint32_t pid, void *context, Control control, Process process, Event event) {
    @autoreleasepool {
        if (@available(macOS 14.2, *)) return run(pid, context, control, process, event);
        return static_cast<int>(kAudioHardwareUnsupportedOperationError);
    }
}
