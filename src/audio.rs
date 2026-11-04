use rodio::{OutputStream, Sink};
use rodio::buffer::SamplesBuffer;

/// Plays a short synthesized "boing" tone once, on its own thread, so the log
/// watcher never blocks on audio I/O.
pub fn play_boing() {
    std::thread::spawn(|| {
        if let Err(e) = play_boing_blocking() {
            eprintln!("hpwarn: could not play warning sound: {e}");
        }
    });
}

/// Plays the sharper, more urgent "panic" tone once (the optional second,
/// lower-HP warning), on its own thread.
pub fn play_panic() {
    std::thread::spawn(|| {
        if let Err(e) = play_panic_blocking() {
            eprintln!("hpwarn: could not play panic sound: {e}");
        }
    });
}

/// Plays the "boing" tone, waits for it to fully finish, then plays the
/// panic tone - for the case where a single HP drop crosses both the first
/// and the second threshold at once. Runs on its own thread (both tones
/// play back-to-back on that one thread, so they can never overlap).
pub fn play_boing_then_panic() {
    std::thread::spawn(|| {
        if let Err(e) = play_boing_blocking() {
            eprintln!("hpwarn: could not play warning sound: {e}");
        }
        if let Err(e) = play_panic_blocking() {
            eprintln!("hpwarn: could not play panic sound: {e}");
        }
    });
}

fn play_boing_blocking() -> anyhow::Result<()> {
    let (_stream, handle) = OutputStream::try_default()?;
    let sink = Sink::try_new(&handle)?;

    let sample_rate = 44100u32;
    let duration_s = 0.38f32;
    let n = (sample_rate as f32 * duration_s) as usize;

    let freq_start = 950.0f32;
    let freq_end = 220.0f32;

    let mut samples = Vec::with_capacity(n);
    let mut phase = 0.0f32;
    for i in 0..n {
        let t = i as f32 / sample_rate as f32;
        let progress = t / duration_s;
        // Exponential-ish downward pitch sweep: classic "boing".
        let freq = freq_start * (freq_end / freq_start).powf(progress);
        phase += 2.0 * std::f32::consts::PI * freq / sample_rate as f32;

        // Quick attack, smooth decay envelope.
        let attack = (t / 0.015).min(1.0);
        let decay = (-(t) * 6.0).exp();
        let envelope = attack * decay;

        samples.push(phase.sin() * envelope * 0.9);
    }

    let buffer = SamplesBuffer::new(1, sample_rate, samples);
    sink.append(buffer);
    sink.sleep_until_end();
    Ok(())
}

/// Synthesizes the more urgent "panic" tone: three sharp, high-pitched
/// staccato pulses (a klaxon-ish character), deliberately distinct from the
/// smooth, friendlier downward-sweep "boing" - this plays at a lower, more
/// dangerous HP% so it needs to read as more alarming at a glance.
fn play_panic_blocking() -> anyhow::Result<()> {
    let (_stream, handle) = OutputStream::try_default()?;
    let sink = Sink::try_new(&handle)?;

    let sample_rate = 44100u32;
    let pulse_s = 0.11f32;
    let gap_s = 0.045f32;
    let pulse_n = (sample_rate as f32 * pulse_s) as usize;
    let gap_n = (sample_rate as f32 * gap_s) as usize;

    let freq_a = 1500.0f32;
    let freq_b = 1150.0f32;

    let mut samples = Vec::with_capacity((pulse_n + gap_n) * 3);
    for pulse in 0..3 {
        // Alternate between two close, high pitches each pulse for a more
        // frantic, siren-like edge rather than a single flat beep.
        let freq = if pulse % 2 == 0 { freq_a } else { freq_b };
        let mut phase = 0.0f32;
        for i in 0..pulse_n {
            let t = i as f32 / sample_rate as f32;
            phase += 2.0 * std::f32::consts::PI * freq / sample_rate as f32;

            // Sharp attack, short hold, sharp cutoff - staccato rather than
            // boing's smooth decay.
            let attack = (t / 0.005).min(1.0);
            let release_start = pulse_s - 0.02;
            let release = if t > release_start {
                ((pulse_s - t) / 0.02).max(0.0)
            } else {
                1.0
            };
            let envelope = attack * release;

            samples.push(phase.sin() * envelope * 0.95);
        }
        if pulse < 2 {
            samples.extend(std::iter::repeat(0.0f32).take(gap_n));
        }
    }

    let buffer = SamplesBuffer::new(1, sample_rate, samples);
    sink.append(buffer);
    sink.sleep_until_end();
    Ok(())
}
