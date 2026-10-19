# Local speech recognition candidates

Checked 2026-10-18. Production still uses Whisper large-v3-turbo through
whisper.cpp. Formatting, dictionary, VAD and desktop delivery improvements do
not change that recognizer's acoustic accuracy.

| Candidate | Why evaluate | Integration |
| --- | --- | --- |
| [Qwen3-ASR-1.7B-hf](https://huggingface.co/Qwen/Qwen3-ASR-1.7B-hf) | First quality candidate: multilingual, vocabulary/context prompting; model card reports 5.59% mean WER on its June 26 Open ASR evaluation. | Transformers backend; needs a new provider or bounded local worker behind `SttProvider`. |
| [Parakeet TDT 0.6B v3](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3) | First latency candidate: smaller non-autoregressive recognizer, punctuation/capitalization, 25 European languages; card reports 6.34% mean WER. | NeMo, Transformers or the native runtime linked from NVIDIA's model card; needs a new provider. |
| [Canary-Qwen 2.5B](https://huggingface.co/nvidia/canary-qwen-2.5b) | English quality alternative; card reports 5.63% mean WER. | NeMo speech-augmented language model; larger runtime than current whisper.cpp setup. |
| [Whisper large-v3](https://huggingface.co/openai/whisper-large-v3-turbo) | Lowest integration cost as an accuracy baseline. Turbo removes decoder layers to improve speed, with a quality tradeoff. | Existing Whisper provider; verified model path required. |

These are published aggregate scores from different evaluations, not a local
head-to-head result. They do not establish which model best recognizes one
speaker, technical vocabulary, punctuation or noisy microphone audio. Published
throughput figures also do not predict single-utterance latency on an RTX 5080.

[Higgs Audio v3 STT](https://huggingface.co/bosonai/higgs-audio-v3-stt) also
deserves tracking. Its card says its June checkpoint supersedes prior scores
and leaderboard results can predate the update; do not rank the current
checkpoint from those older numbers.

Recommended next comparison: current Turbo baseline, Qwen3-ASR-1.7B-hf and
Parakeet. Use the same locally held, consented recordings and manually corrected
references. Score raw STT separately from formatting; measure word error rate,
technical-term errors, silence hallucinations, punctuation, warm/cold p50/p95
latency and combined GPU memory with the formatter loaded. Include short
commands, long dictation, false starts, quiet speech and app delivery checks.
Keep private recordings and reference text out of the public repository.

Retain Whisper as fallback until a candidate wins this comparison. Model names
for Qwen/Parakeet cannot be put into the current `[whisper]` config: that provider
loads Whisper weights only. No alternative model was installed or benchmarked
as part of this research.
