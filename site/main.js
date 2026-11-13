(() => {
  const REPO = 'KasunRocz-Softwares/SoundSync';
  const reduce = matchMedia('(prefers-reduced-motion: reduce)').matches;

  // Latest installer straight from GitHub Releases.
  fetch(`https://api.github.com/repos/${REPO}/releases/latest`)
    .then(r => (r.ok ? r.json() : Promise.reject(r.status)))
    .then(rel => {
      const exe = (rel.assets || []).find(a => a.name.endsWith('-setup.exe'));
      if (!exe) return;
      for (const id of ['download', 'download-2']) {
        const a = document.getElementById(id);
        if (a) a.href = exe.browser_download_url;
      }
      const mb = (exe.size / 1048576).toFixed(1);
      const v = String(rel.tag_name || '').replace(/^v/, '');
      document.getElementById('release-meta').textContent = `Version ${v} · ${mb} MB · Windows 11 x64 · MIT licence · Updates itself`;
    })
    .catch(() => { /* keep the link to the releases page */ });

  // Scroll reveal.
  const items = document.querySelectorAll('.reveal');
  if (reduce || !('IntersectionObserver' in window)) {
    items.forEach(el => el.classList.add('in'));
  } else {
    const io = new IntersectionObserver(entries => {
      for (const e of entries) if (e.isIntersecting) { e.target.classList.add('in'); io.unobserve(e.target); }
    }, { rootMargin: '0px 0px -8% 0px' });
    items.forEach(el => io.observe(el));
  }

  // Sync demo: beats scroll toward the playhead ("now, at your seat").
  const laneA = document.getElementById('lane-a');
  const laneB = document.getElementById('lane-b');
  const offsetEl = document.getElementById('offset');
  const calBtn = document.getElementById('calibrate');
  const resetBtn = document.getElementById('reset');
  const soundBtn = document.getElementById('sound');
  if (!laneA || !laneB) return;

  const W = 600, H = 48, WINDOW_MS = 2400, BEAT_MS = 600, BAR_W = 7;
  const BT_LATENCY = 255;
  const pxPerMs = W / WINDOW_MS;
  let wiredDelay = 0;          // what SyncSound adds to the wired speaker
  let anim = null;             // calibration animation
  let audio = null;
  let lastBeat = { a: -1, b: -1 };

  const bars = lane => {
    const n = Math.ceil(WINDOW_MS / BEAT_MS) + 2;
    const list = [];
    for (let i = 0; i < n; i++) {
      const r = document.createElementNS('http://www.w3.org/2000/svg', 'rect');
      r.setAttribute('y', 8); r.setAttribute('height', H - 16); r.setAttribute('width', BAR_W); r.setAttribute('rx', 2);
      lane.appendChild(r); list.push(r);
    }
    return list;
  };
  const barsA = bars(laneA), barsB = bars(laneB);

  const place = (list, now, latency) => {
    // Beat k is played at k*BEAT_MS and heard at k*BEAT_MS + latency.
    const first = Math.floor((now - latency - WINDOW_MS / 2) / BEAT_MS);
    list.forEach((r, i) => {
      const heard = (first + i) * BEAT_MS + latency;
      const x = W / 2 + (heard - now) * pxPerMs - BAR_W / 2;
      r.setAttribute('x', x.toFixed(1));
      const near = Math.abs(heard - now) < 60;
      r.style.opacity = near ? 1 : 0.55;
    });
  };

  const click = (freq, pan) => {
    if (!audio) return;
    const t = audio.currentTime;
    const o = audio.createOscillator(), g = audio.createGain(), p = audio.createStereoPanner();
    o.frequency.value = freq; o.type = 'triangle';
    g.gain.setValueAtTime(0.0001, t);
    g.gain.exponentialRampToValueAtTime(0.35, t + 0.004);
    g.gain.exponentialRampToValueAtTime(0.0001, t + 0.09);
    p.pan.value = pan;
    o.connect(g).connect(p).connect(audio.destination);
    o.start(t); o.stop(t + 0.1);
  };

  const showOffset = () => {
    const off = Math.round(Math.abs(BT_LATENCY - wiredDelay));
    offsetEl.textContent = `${off} ms`;
    offsetEl.classList.toggle('ok', off < 5);
  };

  const start = performance.now();
  const frame = nowAbs => {
    const now = reduce ? 1500 : nowAbs - start;
    if (anim) {
      const k = Math.min(1, (nowAbs - anim.t0) / anim.dur);
      const ease = 1 - Math.pow(1 - k, 3);
      wiredDelay = anim.from + (anim.to - anim.from) * ease;
      showOffset();
      if (k >= 1) { anim = null; resetBtn.disabled = false; calBtn.disabled = wiredDelay > 0; calBtn.querySelector('span').textContent = 'Calibrated'; }
    }
    place(barsA, now, wiredDelay);
    place(barsB, now, BT_LATENCY);
    // Clicks when a beat reaches the listener.
    const ka = Math.floor((now - wiredDelay) / BEAT_MS), kb = Math.floor((now - BT_LATENCY) / BEAT_MS);
    if (ka !== lastBeat.a) { lastBeat.a = ka; click(880, -0.6); }
    if (kb !== lastBeat.b) { lastBeat.b = kb; click(660, 0.6); }
    if (!reduce || anim) requestAnimationFrame(frame);
  };
  requestAnimationFrame(frame);

  const animateTo = to => {
    const from = wiredDelay;
    calBtn.disabled = true; resetBtn.disabled = true;
    if (reduce) {
      wiredDelay = to; showOffset();
      place(barsA, 1500, wiredDelay);
      resetBtn.disabled = to === 0; calBtn.disabled = to > 0;
      calBtn.querySelector('span').textContent = to > 0 ? 'Calibrated' : 'Calibrate';
      return;
    }
    anim = { from, to, t0: performance.now(), dur: 1400 };
  };

  calBtn.addEventListener('click', () => {
    calBtn.querySelector('span').textContent = 'Measuring…';
    calBtn.disabled = true;
    setTimeout(() => animateTo(BT_LATENCY), reduce ? 0 : 900);
  });
  resetBtn.addEventListener('click', () => {
    calBtn.querySelector('span').textContent = 'Calibrate';
    animateTo(0);
    if (!reduce) setTimeout(() => { calBtn.disabled = false; }, 1450);
  });
  soundBtn.addEventListener('click', () => {
    const on = soundBtn.getAttribute('aria-pressed') !== 'true';
    if (on && !audio) {
      try { audio = new (window.AudioContext || window.webkitAudioContext)(); } catch { return; }
    }
    if (audio) (on ? audio.resume() : audio.suspend());
    soundBtn.setAttribute('aria-pressed', String(on));
    soundBtn.innerHTML = on
      ? '<i class="ph ph-speaker-high"></i><span>Sound on</span>'
      : '<i class="ph ph-speaker-slash"></i><span>Sound off</span>';
    if (reduce && on) requestAnimationFrame(frame);
  });
})();
