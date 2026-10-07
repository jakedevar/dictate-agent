//! `audio_level` events reach a subscriber while recording, and only then.
//!
//! The capability has been advertised since S01; before S32 nothing emitted
//! the event, so a HUD meter would have sat flat without any error.

mod harness;

use std::sync::Arc;
use std::time::Duration;

use dictate_core::ports::mock::MockAudio;
use dictate_proto::{Command, DictationMode, Event, State};
use harness::{within, Harness, Setup};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn levels_flow_while_recording_at_a_meter_rate_and_stop_with_it() {
    let audio = Arc::new(MockAudio::with_seconds(1.0).with_level(0.25));
    let h = Harness::with(Setup::default().with_audio(audio)).await;
    let mut c = h.client().await;
    assert!(
        c.hello
            .clone()
            .unwrap()
            .capabilities
            .features
            .audio_level_events
    );
    c.subscribe().await;

    c.request(Command::StartDictation {
        mode: DictationMode::Toggle,
        options: None,
    })
    .await
    .expect("start");
    c.wait_for_state(State::Recording).await;

    // Seven consecutive levels. Counting rather than timing out keeps every
    // read whole (a cancelled line read would lose data).
    let mut levels = Vec::new();
    let mut first = None;
    while levels.len() < 7 {
        if let Event::AudioLevel { rms, peak, .. } = c.next_event().await {
            first.get_or_insert_with(std::time::Instant::now);
            levels.push((rms, peak));
        }
    }
    let spread = first.unwrap().elapsed();
    assert!(
        spread >= Duration::from_millis(150),
        "six intervals at ~30 Hz take ~200 ms; {spread:?} is a flood a HUD would drown in"
    );
    assert!(levels
        .iter()
        .all(|&(rms, peak)| rms == 0.25 && peak == Some(0.25)));

    c.request(Command::Stop).await.expect("stop");
    within("the session to finish", async {
        loop {
            match c.next_event().await {
                Event::StateChanged {
                    to: State::Done, ..
                } => break,
                Event::AudioLevel { .. } => {}
                _ => {}
            }
        }
    })
    .await;
    // Nothing more after the session left recording.
    let late = tokio::time::timeout(Duration::from_millis(150), async {
        loop {
            if let Event::AudioLevel { .. } = c.next_event().await {
                return;
            }
        }
    })
    .await;
    assert!(late.is_err(), "no audio_level after recording ended");
    h.stop().await;
}
