// "You mined a block": a jackpot sound (fanfare, then coins pouring) (Web Audio, synthesized) and an event
// the top banner shows as a green "+X SM64 mined" popup.
export function playPayout() {
  try {
    const ctx = new (window.AudioContext || window.webkitAudioContext)();
    const out = ctx.createGain();
    out.gain.value = 0.9;
    out.connect(ctx.destination);
    const t0 = ctx.currentTime + 0.02;
    // One metal clink: a few inharmonic partials with a fast decay.
    const clink = (at, freq, vol, pan) => {
      const p = ctx.createStereoPanner ? ctx.createStereoPanner() : null;
      const bus = ctx.createGain();
      bus.gain.value = vol;
      if (p) { p.pan.value = pan; bus.connect(p).connect(out); } else bus.connect(out);
      [[1, 1, 0.16], [2.76, 0.5, 0.09], [5.4, 0.25, 0.05], [8.93, 0.12, 0.03]].forEach(([ratio, amp, len]) => {
        const o = ctx.createOscillator();
        const g = ctx.createGain();
        o.type = 'sine';
        o.frequency.value = freq * ratio;
        g.gain.setValueAtTime(0, t0 + at);
        g.gain.linearRampToValueAtTime(amp, t0 + at + 0.002);
        g.gain.exponentialRampToValueAtTime(0.0005, t0 + at + len);
        o.connect(g).connect(bus);
        o.start(t0 + at);
        o.stop(t0 + at + len + 0.02);
      });
    };
    // Jackpot fanfare: a fast rising arpeggio into a ringing major chord.
    const tone = (at, freq, len, type, vol) => {
      const o = ctx.createOscillator();
      const g = ctx.createGain();
      o.type = type;
      o.frequency.value = freq;
      g.gain.setValueAtTime(0, t0 + at);
      g.gain.linearRampToValueAtTime(vol, t0 + at + 0.008);
      g.gain.exponentialRampToValueAtTime(0.0008, t0 + at + len);
      o.connect(g).connect(out);
      o.start(t0 + at);
      o.stop(t0 + at + len + 0.03);
    };
    const run = [523.25, 659.25, 783.99, 1046.5, 1318.5, 1568.0, 2093.0];
    run.forEach((f, i) => { tone(i * 0.055, f, 0.18, 'square', 0.05); tone(i * 0.055, f * 2, 0.12, 'sine', 0.03); });
    const chordAt = run.length * 0.055;
    [1046.5, 1318.5, 1568.0, 2093.0].forEach((f) => {
      tone(chordAt, f, 1.4, 'triangle', 0.09);
      // A little vibrato shimmer on top.
      tone(chordAt + 0.02, f * 1.004, 1.2, 'sine', 0.04);
    });
    // Then the coins pour out: dense in the middle, each bouncing once or twice.
    const pourAt = chordAt + 0.15;
    const n = 40;
    for (let i = 0; i < n; i++) {
      const x = i / n;
      const at = pourAt + 1.8 * x * x * 0.6 + 1.8 * x * 0.4 + Math.random() * 0.04;
      const f = 2300 + Math.random() * 2200;
      const pan = Math.random() * 1.4 - 0.7;
      const v = 0.25 + Math.random() * 0.25;
      clink(at, f, v, pan);
      const bounce = 0.05 + Math.random() * 0.07;
      clink(at + bounce, f * (0.97 + Math.random() * 0.06), v * 0.45, pan);
      if (Math.random() < 0.5) clink(at + bounce * 1.8, f, v * 0.2, pan);
    }
    setTimeout(() => ctx.close(), 4500);
  } catch { /* no audio */ }
}

export function announceMined(height, atomicReward) {
  playPayout();
  window.dispatchEvent(new CustomEvent('sm64coin-mined', { detail: { height, reward: atomicReward } }));
}
