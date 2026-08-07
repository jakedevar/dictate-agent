---
date: 2026-08-07
researcher: claude
git_commit: 4f339376632d920c4f25f31bb4b2f79d089f13c
branch: master
repository: dictate_agent
topic: "R4 — Wake-word engine bake-off (gates S34)"
tags: [research, wake-word, openwakeword, rustpotter, porcupine, livekit-wakeword, silero-vad, privacy, rust]
status: complete
type: research
---

# R4 — Wake-word engine bake-off

## Question

Which wake-word engine, if any, should `dictate_agent` embed for the
optional, strictly opt-in "Hey Flow"-parity hands-free trigger (feature #15
in the master slice map), and is the idle CPU cost acceptable for a daemon
that runs 24/7 on Jake's workstation? This spike gates whether S34 gets
built at all (`thoughts/shared/plans/2026-08-07-wisprflow-parity-master-slice-map.md:71,325-329,387-389`).

Constraint carried in from the slice map: always-on microphone listening is
a privacy-posture change for a product whose entire pitch is fully-local /
private, so any candidate that phones home, requires a hosted access key, or
otherwise breaks full-offline operation is disqualified regardless of
accuracy.

## Comparison table

| Axis | openWakeWord | rustpotter | Porcupine (Picovoice) | livekit-wakeword |
|---|---|---|---|---|
| License (code) | Apache-2.0 | Apache-2.0 | Apache-2.0 (SDK code) | Apache-2.0 |
| License (models) | **CC BY-NC-SA 4.0** (pretrained; non-commercial) | N/A — you train your own | Free tier **discontinued 2026-06-30**; no non-commercial tier going forward | N/A — you train your own |
| Local-first compatible | Yes, fully offline | Yes, fully offline | **No** — requires an AccessKey validated against Picovoice servers; docs confirm periodic outbound usage-reporting calls; `create()` observed to hang on a firewalled host | Yes, fully offline, no key |
| Rust integration | No official Rust; 3-stage ONNX/TFLite pipeline (melspec → embedding → classifier). Community port `oww_rs`/`oww-rs` (crates.io, MIT) wraps it via `tract-onnx` (pure-Rust ONNX, no `libonnxruntime.so`), 0.3.3, pushed 2026-07-05, but tiny (7 stars, 1 maintainer) | **Pure Rust**, `candle-core`/`candle-nn` for the neural mode, no ONNX/Python runtime | No official Rust binding (`binding/` dir has android/ios/java/node/python/react/web — no `rust/`); would need raw FFI to the C SDK | **Pure Rust**, ONNX-based, no native lib deps required — purpose-built for Rust daemons |
| Pretrained models | Yes: Alexa, Hey Mycroft, Hey Jarvis, Hey Rhasspy (no "hey dictate") | No pretrained models shipped | Yes: Alexa, Hey Google, Hey Siri, etc. | Yes: "hey livekit" (+ Chinese), no "hey dictate" |
| Custom keyword ("hey dictate") training | Yes — synthetic TTS pipeline (Piper-based), several thousand synthetic positive clips, reuses a prebuilt ~30k-hr negative corpus; Colab notebook ~1hr, GPU effectively required for reasonable training time | Yes — two modes: (a) template/DTW "reference," 3–8 real WAV recordings, no real training; (b) neural "model," tagged WAV training set, sample-count guidance **not documented** | Yes via Picovoice Console, but **non-commercial only** and the optimizer tool's output is explicitly barred from commercial products without a paid agreement; now further gated by free-tier shutdown | Yes — YAML-configured pipeline, TTS synthesis (VoxCPM/Piper) → augmentation → train → export, 30+ languages (English best) |
| Model size on disk | ~200 KB per custom classifier head (shared embedding model is separate, exact base size unconfirmed — flagged gap) | 320 KB (tiny) – 3.1 MB (large) for ~1950ms audio window | Not independently confirmed | Not independently confirmed |
| Idle CPU cost | Official: 1 RPi3 core runs 15–20 models simultaneously in real time; HA/Wyoming users report near-100% single-core on **Pi Zero 2W** in some configs (not representative of a desktop) | **No published figures anywhere** — real gap | Third-party (competitor-sourced, treat as low-confidence): ~0.6% CPU on RPi5 | **No published figures** — project is brand new (first release 2026-03-13) |
| False-accept / false-reject | Self-reported target: <5% FRR, <0.5/hr FAR; self-reported "beats Porcupine" on Porcupine's own test set (small sample, maintainer-published, not independent) | No FAR/FRR numbers published; only qualitative "pretty good" claim | Self-reported: 97.1% acc @ 1 FA/10hr @ 10dB SNR; older benchmark vs. Snowboy/PocketSphinx (not vs. modern engines), self-published | Self-reported vs. openWakeWord on "hey livekit": 0.08 FA/hr vs. 8.50 FA/hr, 86.1% vs 68.6% recall — single-keyword, maintainer-published, unverified independently |
| Maintenance (as of 2026-08-07) | Active — commits through **Dec 2025**, single primary maintainer + external PRs; latest tagged release v0.6.0 (date inconsistently reported across fetches, flagged) | **Dormant since Oct 2023** — zero commits ~2y10m, no GitHub Releases tagged, 6 open issues, `candle-core` pinned to a very old 0.2.2. Still used as an openHAB voice add-on (bundle updated Dec 2025), giving it a real-world track record despite the commit freeze | Actively maintained by a commercial vendor, but the product direction (free-tier shutdown, enterprise-only pricing) is now hostile to this use case | **Brand new** (first published 2026-03-13, latest 0.1.3 as of 2026-04-02) — too young to judge long-term maintenance |

## Detail notes

### Porcupine — disqualified

Picovoice's own docs state "internet is only required for monthly
licensing validation and usage reporting" and confirm usage data is cached
on-device and pushed to Picovoice's servers when connectivity is available
(`community.home-assistant.io/t/fyi-picovoice-confirmed-free-tier-accesskeys-will-stop-working-after-june-30-2026/1012744`,
accessed 2026-08-07). A GitHub issue documents `pvporcupine.create()`
hanging indefinitely on a firewalled machine
(`github.com/Picovoice/porcupine/issues/579`). Independent of licensing
terms, this alone is disqualifying for a product whose entire pitch is
fully-local/private — the daemon would be phoning a third party on every
license-check cycle just to run local wake-word inference.

On top of that, Picovoice discontinued the free tier entirely effective
**2026-06-30** ("no non-commercial tier planned going forward," per
Picovoice support quoted in the Home Assistant community thread), leaving
only a 7-day trial and paid enterprise contracts. There's also no official
Rust SDK. Three independent reasons to drop it: phone-home, no free path,
no Rust binding.

### openWakeWord — most mature, but not native Rust

Best-documented training story (synthetic TTS pipeline, actual sample-count
guidance) and the only engine with real production deployment evidence at
scale (Home Assistant's Assist pipeline defaults to it, explicitly for
local-first privacy reasons — `home-assistant.io/voice_control/about_wake_word`).
No official Rust binding, but the `oww_rs`/`oww-rs` crate (MIT, `tract-onnx`
backend, pure Rust, no native ONNX runtime needed) is a working low-adoption
port that reuses openWakeWord's model format and (per its Cargo.toml) does
mic capture + resampling + inference natively — a real integration path,
not vaporware, but a single-maintainer, 7-star dependency risk. Pretrained
models are CC-BY-NC-SA (non-commercial) — irrelevant for Jake's personal
use, but would block redistribution if `dictate_agent` were ever shared.

### rustpotter — pure Rust but dormant

Genuinely the simplest embed (pure Rust, no ONNX/Python), and has a
low-friction 3–8-sample bootstrap mode for quick prototyping. But the repo
has had zero commits since October 2023, no CPU or accuracy numbers are
published anywhere, and the neural training mode's sample-count guidance is
undocumented. It's still shipped as an openHAB voice add-on with a Dec 2025
bundle release, so it isn't abandoned in the sense of "unused," but nobody
is fixing bugs or updating dependencies (candle-core is pinned to an old
0.2.2).

### livekit-wakeword — the one to prototype next, not yet provable

A genuinely new (first release 2026-03-13) native-Rust, ONNX-based engine
built by LiveKit specifically for the "embed a wake word in a Rust service"
use case: no native lib dependencies, ships a training pipeline
(TTS-synthesis → augmentation → train → export) that directly targets a
custom phrase like "hey dictate," and self-reports large accuracy gains
over openWakeWord on its own single-keyword benchmark (0.08 vs 8.50
false-accepts/hour). All of that is maintainer-published on one test case
and has zero independent verification — treat the accuracy numbers as
directional only, and there is no idle-CPU figure published for it at all.

### Silero VAD as a cheap gate

`snakers4/silero-vad` (MIT, "zero telemetry, no keys, no registration") runs
in <1ms per 30ms+ chunk on a single CPU thread and is already the kind of
VAD `dictate_agent` would want regardless of wake-word plans. openWakeWord
natively supports gating wake-word inference behind a VAD threshold — the
VAD stays essentially free and continuous, and the (comparatively) more
expensive wake-word classifier only runs when the VAD says "this is speech."
This pattern is the practical answer to "is idle CPU cost acceptable": you
don't run full wake-word inference on silence at all.

## Idle CPU cost — the decisive axis

No engine here has a rigorous, independently-reproduced idle-CPU/power
benchmark on comparable hardware. The only anchor points:

- openWakeWord: a single core of a **Raspberry Pi 3** — a ~2015-era ARM
  Cortex-A53 at 1.2GHz — runs 15–20 wake-word models *simultaneously* in
  real time per the maintainer's own README. Jake's workstation (an RTX
  5080 rig already running whisper.cpp + CUDA per the R2 spike) has orders
  of magnitude more single-thread CPU headroom than a Pi 3. Even the
  pessimistic counter-evidence (near-100% single-core use on a **Pi Zero
  2W**, a much weaker chip than a Pi 3) is not representative of the target
  hardware.
- Gating with Silero VAD (<1ms/chunk) means the wake-word classifier itself
  only runs on detected speech, not continuously on silence — this is the
  standard mitigation and is directly supported by openWakeWord's API.

Given the Pi-3-class headroom already demonstrated for openWakeWord-style
models, and that Jake's target hardware is a modern desktop workstation, the
idle CPU cost of a VAD-gated wake-word classifier is very likely negligible
in absolute terms (low single-digit percent of one core, extrapolating down
from Pi3 real-time-with-headroom numbers) — but this is an extrapolation,
not a measurement on the actual target machine, and should be confirmed
with a 10-minute local benchmark before S34 ships.

## Verdict

**BUILD-S34** — conditional, narrow scope, with an explicit follow-up
measurement gate before shipping.

Ranked recommendation:

1. **openWakeWord via `oww_rs`/`oww-rs`** (primary pick). Most mature
   training story, real production track record (Home Assistant), fully
   local/offline, Apache-2.0 code. The Rust wrapper is a small dependency
   risk (single maintainer, 7 stars) but is a thin shim over a well-proven
   model format — if it breaks, the fallback is running openWakeWord's
   Python reference implementation as a local sidecar process communicating
   over a socket, which is uglier but not a dead end.
2. **livekit-wakeword** as a parallel/next-generation candidate worth a
   half-day spike once S34 is scoped — pure Rust, no native deps, built for
   exactly this use case, but too new (5 months old, single benchmark) to
   commit to as the only option today. Worth prototyping alongside
   openWakeWord and picking whichever measures better on Jake's actual
   hardware.
3. **rustpotter** as a fallback only if both of the above prove impractical
   to integrate — pure Rust is attractive but the 2y10m commit freeze and
   total absence of published accuracy/CPU data make it the weaker bet for
   new work in 2026.
4. **Porcupine: DROP.** Phone-home licensing validation is disqualifying
   for a fully-local product on principle, independent of the free-tier
   shutdown and missing Rust SDK. Do not revisit unless the product's
   privacy posture changes.

Before S34 implementation starts, S34 should begin with a **half-day local
benchmark**: run openWakeWord (via `oww_rs`) and livekit-wakeword
side-by-side on Jake's workstation for actual idle-CPU% and false-accept
behavior with real room noise, gated behind Silero VAD, and pick the winner
from measured numbers rather than the maintainer-published ones in this
document. If that benchmark comes back with idle CPU cost that is
measurably non-negligible (e.g. sustained >2-3% of one core with VAD
gating), fall back to DEFER-S34 — the P2/optional status of this feature
means it isn't worth a real resource cost for a rarely-used hands-free
convenience.

## Sources

- https://github.com/dscripka/openWakeWord (accessed 2026-08-07)
- https://github.com/dscripka/openWakeWord/releases (accessed 2026-08-07)
- https://github.com/dscripka/openWakeWord/commits/main (accessed 2026-08-07)
- https://github.com/dscripka/openWakeWord/blob/main/docs/custom_verifier_models.md
- https://github.com/dscripka/openWakeWord/blob/main/notebooks/automatic_model_training.ipynb
- https://github.com/lgpearson1771/openwakeword-trainer
- https://github.com/alfiedennen/openwakeword-colab-2026
- https://huggingface.co/davidscripka/openwakeword
- https://github.com/rhasspy/wyoming-openwakeword/issues/47
- https://github.com/rhasspy/wyoming-openwakeword/issues/30
- https://github.com/skoky/oww_rs (accessed 2026-08-07)
- https://crates.io/crates/oww-rs (accessed 2026-08-07)
- https://github.com/GiviMAD/rustpotter (accessed 2026-08-07)
- https://github.com/GiviMAD/rustpotter/blob/main/Cargo.toml
- https://crates.io/crates/rustpotter (accessed 2026-08-07)
- https://github.com/GiviMAD/rustpotter/releases (accessed 2026-08-07)
- https://www.openhab.org/addons/voice/rustpotterks/ (accessed 2026-08-07)
- https://github.com/Picovoice/porcupine (accessed 2026-08-07)
- https://github.com/Picovoice/porcupine/tree/master/binding (accessed 2026-08-07)
- https://github.com/Picovoice/porcupine/issues/579
- https://picovoice.ai/docs/faq/general/
- https://github.com/Picovoice/wake-word-benchmark
- https://community.home-assistant.io/t/fyi-picovoice-confirmed-free-tier-accesskeys-will-stop-working-after-june-30-2026/1012744
- https://community.home-assistant.io/t/porcupine-free-tier-shutdown-alternatives-for-home-assistant-voice-users/1012382
- https://news.ycombinator.com/item?id=48248969
- https://news.ycombinator.com/item?id=33964527
- https://www.hackster.io/news/picovoice-launches-completely-free-usage-tier-for-offline-voice-recognition-for-up-to-three-users-e1eafbc97bb0
- https://www.home-assistant.io/voice_control/about_wake_word/
- https://www.home-assistant.io/blog/2024/02/21/voice-chapter-6/
- https://esphome.io/components/micro_wake_word/
- https://github.com/livekit/livekit-wakeword (accessed 2026-08-07)
- https://lib.rs/crates/livekit-wakeword (accessed 2026-08-07)
- https://github.com/snakers4/silero-vad (accessed 2026-08-07)
- https://github.com/snakers4/silero-vad/discussions/738
- https://voxrt.com/wake-word-comparison (low-confidence, competitor-published)
