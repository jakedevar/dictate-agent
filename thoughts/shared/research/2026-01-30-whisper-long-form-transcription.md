---
date: 2026-01-30
researcher: Claude (Opus 4.5)
git_commit: e99ea3467f3dae98e443302c05c0d977fdac816d
branch: master
repository: dictate_agent
topic: "Whisper Long-Form Transcription: Silent Truncation Bug and Pipeline Migration"
tags: [research, whisper, transcription, huggingface, pipeline, speculative-decoding, bug-fix]
status: complete
last_updated: 2026-01-30
last_updated_by: Claude (Opus 4.5)
---

# Whisper Long-Form Transcription: Silent Truncation Bug and Pipeline Migration

## Problem

Dictation was silently cutting off after ~30 seconds. The symptom was spotty — shorter dictations worked fine, longer ones lost everything after the 30-second mark with no error or warning.

## Root Cause

`WhisperProcessor.__call__()` defaults to `truncation=True`. When the transcriber called:

```python
inputs = self.processor(audio, sampling_rate=16000, return_tensors="pt")
```

...any audio beyond 30 seconds was **silently discarded** before reaching `model.generate()`. The processor pads audio shorter than 30s and truncates audio longer than 30s to fit Whisper's fixed 1500-frame mel spectrogram window.

There is no warning logged. The API silently drops your data.

## Key Technical Details

### Whisper's 30-Second Architecture Constraint

Whisper's encoder has a fixed receptive field of exactly 30 seconds (1500 mel-spectrogram frames at 16kHz sampling rate). This is a hard architectural limit — a single `model.generate()` call can only process 30 seconds of audio regardless of input length.

### Three Approaches to Long Audio

| Approach | How It Works | Speculative Decoding | Reliability |
|----------|-------------|---------------------|-------------|
| **Pipeline with chunking** (chosen) | Splits mel spectrogram into overlapping 30s windows, transcribes each, merges text | Incompatible | High — purpose-built, maintained by HuggingFace |
| **Sequential long-form** | Set `truncation=False`, `return_timestamps=True`, `return_attention_mask=True` on processor; model processes sequentially using timestamp heuristics | Fragile — version-dependent compatibility issues | Medium — has had multiple bugs across transformers versions (issues #28978, #29287) |
| **Manual chunking** | Split audio yourself, call `model.generate()` per chunk, merge text | Compatible | Medium — must handle chunk boundaries yourself |

### Why Speculative Decoding Is Incompatible with the Pipeline

Whisper overrides the standard `GenerationMixin.generate()` with its own `generate()` method:

```
pipeline._forward() → model.generate(**generate_kwargs)
→ Whisper.generate() → generate_with_fallback()
→ super().generate()  # GenerationMixin
→ _beam_search()  # NOT _assisted_decoding
→ self(**model_inputs)  # forward() — assistant_model leaks here
```

The `assistant_model` kwarg is supposed to trigger `_assisted_decoding` in `GenerationMixin.generate()`, but Whisper's `generate_with_fallback` calls `super().generate()` in a way that routes to beam search instead. The `assistant_model` kwarg then leaks through to `forward()`, which doesn't accept it, causing:

```
TypeError: WhisperForConditionalGeneration.forward() got an unexpected keyword argument 'assistant_model'
```

This is a structural incompatibility in the transformers library, not a bug in our code.

### Performance Impact of Dropping Speculative Decoding

- `whisper-large-v3-turbo` is already 6-8x faster than `whisper-large-v3`
- Speculative decoding added ~2x on top of that
- On an RTX 5080, `large-v3-turbo` without speculative decoding transcribes ~30s of audio in <1s
- The assistant model (`distil-whisper/distil-large-v3`) consumed ~1GB VRAM — now freed

## What Was Changed

**File**: `dictate/transcribe.py`

### Before (direct model.generate)
- Loaded audio manually via librosa/torchaudio
- Called `self.processor()` which silently truncated at 30s
- Called `self.model.generate()` directly
- Stored `self.model`, `self.processor`, `self.assistant_model` as instance vars
- Speculative decoding worked but only on <=30s audio

### After (pipeline with chunking)
- Pipeline loads audio internally (uses ffmpeg or soundfile)
- `hf_pipeline("automatic-speech-recognition", chunk_length_s=30)` handles chunking
- Audio of any length is supported
- Stores `self.pipe` as single instance var
- Speculative decoding disabled (incompatible with chunked pipeline)
- Duration computed from WAV file size: `(file_size - 44) / (16000 * 2)`

## Configuration Notes

These `config.toml` settings now actively affect the pipeline:

| Setting | Used By | Effect |
|---------|---------|--------|
| `chunk_length_s = 30` | Pipeline constructor | Size of each audio chunk window |
| `batch_size = 1` | Pipeline constructor | Chunks processed in parallel (increase for faster multi-chunk transcription) |
| `use_speculative_decoding` | `_load_models()` | Logged as skipped; no functional effect |

The `batch_size` setting could be increased (e.g., to 4 or 8) to process multiple chunks in parallel on the GPU. With 16GB VRAM on the RTX 5080 and no assistant model loaded, there's headroom for this.

## Gotchas for Future Work

1. **Pipeline audio loading**: The pipeline uses `ffmpeg` (subprocess) or `soundfile` to load audio. Neither is listed in `pyproject.toml` but `ffmpeg` is available on the Arch system. If moving to a system without ffmpeg, add `soundfile` to dependencies.

2. **The `no_speech_threshold` config value exists but is unused** — neither the old code nor the new pipeline code references it. The pipeline has its own silence handling.

3. **If speculative decoding becomes needed again**: The sequential long-form approach (`truncation=False` + `return_timestamps=True`) theoretically supports `assistant_model`, but has had version-dependent bugs. Test thoroughly against the installed transformers version before adopting.

4. **The `chunk_length_s` warning suppression** at the top of the file (`warnings.filterwarnings("ignore", message=".*chunk_length_s.*is very experimental.*")`) exists because HuggingFace marks this feature as experimental. It has been stable in practice.

5. **WAV duration calculation** assumes standard 44-byte header, 16kHz, 16-bit mono — which matches `parecord --format=s16le --rate=16000 --channels=1 --file-format=wav`. If the recording format changes, this calculation breaks silently.
