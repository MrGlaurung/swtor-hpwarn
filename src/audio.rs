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
