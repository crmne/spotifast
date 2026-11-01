// WASAPI process loopback includes only our owned Soloist PID and descendants.
// This is observation only: it never changes a session or endpoint volume.
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include "audio.h"
#include <windows.h>
#include <mmdeviceapi.h>
#include <audioclient.h>
#include <audioclientactivationparams.h>
#include <wrl.h>
#include <wrl/implements.h>
#include <algorithm>
#include <vector>
using namespace Microsoft::WRL;
namespace {
void check(HRESULT result) { if (FAILED(result)) throw result; }
struct Handle {
    HANDLE value = CreateEvent(nullptr, FALSE, FALSE, nullptr);
    ~Handle() { if (value) CloseHandle(value); }
    Handle(const Handle &) = delete;
    Handle() = default;
};
struct Activation : RuntimeClass<RuntimeClassFlags<ClassicCom>, IActivateAudioInterfaceCompletionHandler, FtmBase> {
    Handle done;
    HRESULT result = E_PENDING;
    ComPtr<IAudioClient> client;
    STDMETHODIMP ActivateCompleted(IActivateAudioInterfaceAsyncOperation *operation) override {
        ComPtr<IUnknown> object;
        HRESULT activated = E_FAIL;
        result = operation->GetActivateResult(&activated, &object);
        if (SUCCEEDED(result)) result = activated;
        if (SUCCEEDED(result)) result = object.As(&client);
        SetEvent(done.value);
        return S_OK;
    }
};
struct Capture {
    ComPtr<IAudioClient> client;
    ComPtr<IAudioCaptureClient> packets;
    Handle ready;
    ~Capture() { if (client) client->Stop(); }
};
}
extern "C" int spotifast_soloist_audio(uint32_t pid, void *context, Control control, Process process, Event event) {
    HRESULT com = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    if (FAILED(com)) return static_cast<int>(com);
    int result = 0;
    try {
        Capture capture;
        auto activation = Make<Activation>();
        if (!activation || !activation->done.value || !capture.ready.value) throw E_OUTOFMEMORY;
        AUDIOCLIENT_ACTIVATION_PARAMS parameters{};
        parameters.ActivationType = AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK;
        parameters.ProcessLoopbackParams.TargetProcessId = pid;
        parameters.ProcessLoopbackParams.ProcessLoopbackMode = PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE;
        PROPVARIANT property{};
        property.vt = VT_BLOB;
        property.blob.cbSize = sizeof parameters;
        property.blob.pBlobData = reinterpret_cast<BYTE *>(&parameters);
        ComPtr<IActivateAudioInterfaceAsyncOperation> operation;
        check(ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, __uuidof(IAudioClient), &property, activation.Get(), &operation));
        const auto deadline = GetTickCount64() + 10000;
        while (WaitForSingleObject(activation->done.value, 50) != WAIT_OBJECT_0) {
            if ((control(context) & 1) || GetTickCount64() >= deadline) throw HRESULT_FROM_WIN32(ERROR_TIMEOUT);
        }
        check(activation->result);
        capture.client = activation->client;
        WAVEFORMATEX format{};
        format.wFormatTag = WAVE_FORMAT_IEEE_FLOAT;
        format.nChannels = 2;
        format.nSamplesPerSec = 48000;
        format.wBitsPerSample = 32;
        format.nBlockAlign = 8;
        format.nAvgBytesPerSec = 384000;
        check(capture.client->Initialize(AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            0, 0, &format, nullptr));
        check(capture.client->GetService(IID_PPV_ARGS(&capture.packets)));
        check(capture.client->SetEventHandle(capture.ready.value));
        bool playing = false, routed = false;
        std::vector<float> samples;
        samples.reserve(4800);
        while (!(control(context) & 1)) {
            bool wanted = (control(context) & 2) != 0;
            if (wanted != playing) {
                if (wanted) check(capture.client->Start());
                else {
                    check(capture.client->Stop());
                    // Reset can succeed yet leave process loopback silent after
                    // Start on Windows. Drain pending packets without resetting
                    // the WASAPI client; event 2 still clears the Rust DSP/tap.
                    UINT32 pending = 0;
                    check(capture.packets->GetNextPacketSize(&pending));
                    while (pending) {
                        BYTE *data = nullptr;
                        UINT32 frames = 0;
                        DWORD flags = 0;
                        check(capture.packets->GetBuffer(&data, &frames, &flags, nullptr, nullptr));
                        check(capture.packets->ReleaseBuffer(frames));
                        check(capture.packets->GetNextPacketSize(&pending));
                    }
                    event(context, 2);
                }
                playing = wanted;
            }
            if (!playing) { Sleep(25); continue; }
            WaitForSingleObject(capture.ready.value, 50);
            UINT32 next = 0;
            check(capture.packets->GetNextPacketSize(&next));
            while (next && !(control(context) & 1)) {
                BYTE *data = nullptr;
                UINT32 frames = 0;
                DWORD flags = 0;
                check(capture.packets->GetBuffer(&data, &frames, &flags, nullptr, nullptr));
                // Bound allocations even if a driver reports an invalid packet.
                if (frames > 24000) { capture.packets->ReleaseBuffer(frames); throw E_UNEXPECTED; }
                samples.resize(static_cast<size_t>(frames) * 2);
                if ((flags & AUDCLNT_BUFFERFLAGS_SILENT) || !data) std::fill(samples.begin(), samples.end(), 0.f);
                else std::copy_n(reinterpret_cast<float *>(data), samples.size(), samples.begin());
                check(capture.packets->ReleaseBuffer(frames));
                if (flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY) event(context, 2);
                if (!routed && std::any_of(samples.begin(), samples.end(), [](float x) { return x != 0.f; })) {
                    routed = true; event(context, 1);
                }
                if (control(context) & 2) process(context, samples.data(), samples.size(), 48000);
                check(capture.packets->GetNextPacketSize(&next));
            }
        }
    } catch (HRESULT error) { result = static_cast<int>(error); }
      catch (...) { result = static_cast<int>(E_FAIL); }
    CoUninitialize();
    return result;
}
