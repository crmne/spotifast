// The native adapters own the OS objects. Rust owns DSP and visualizer state.
// No callback retains its buffer. Only control() runs on audio callbacks;
// process() and event() run on the dedicated worker, never on the UI thread.
#pragma once
#include <cstddef>
#include <cstdint>
using Control = uint32_t (*)(void *); // bit 0: stop; bit 1: playing
using Process = void (*)(void *, float *, size_t, uint32_t);
using Event = void (*)(void *, uint32_t); // 1: routed, 2: reset, 3: failure
extern "C" int spotifast_soloist_audio(uint32_t, void *, Control, Process, Event);
