// rustyboi library-sweep gallery behavior. Dependency-free, inlined into the
// generated HTML via include_str!. Handles lazy media, region/search/status
// filtering, sort, collapse, deep-links, URL-hash state and localStorage prefs.
(function () {
  'use strict';

  var LS_KEY = 'rbsweep';
  var REGION_LABEL = { all: 'All', us: 'US', jp: 'Japan', eu: 'Europe', global: 'Global' };

  // View state. `sort`/`dense` are also persisted to localStorage; the rest
  // live only in the URL hash so a shared link restores the exact view.
  var state = { tab: null, region: 'all', sort: 'name', q: '', fail: false, dense: false };

  var els = {
    q: document.getElementById('q'),
    sort: document.getElementById('sort'),
    fail: document.getElementById('failonly'),
    dense: document.getElementById('dense'),
    count: document.getElementById('count'),
  };

  function $tabs() { return Array.prototype.slice.call(document.querySelectorAll('.tab')); }
  function $chips() { return Array.prototype.slice.call(document.querySelectorAll('.chip')); }
  function activePanel() { return document.getElementById('panel-' + state.tab); }
  function cardsIn(root) {
    return Array.prototype.slice.call((root || document).querySelectorAll('.card'));
  }

  // ---- lazy media -----------------------------------------------------------
  // Cards ship a cheap <img> poster (native loading="lazy"); only cards near the
  // viewport are UPGRADED to a live hero — a <canvas> playing the native .rbr
  // recording (decoded by the wasm module) or a legacy <video> for mp4 — and
  // never more than CAP at once. Materializing a media element per card is what
  // froze large galleries; lazy upgrade + cap keeps live media O(viewport).
  var CAP = 40;   // hard ceiling on simultaneous live heroes
  var live = [];  // currently-upgraded hero nodes (<canvas> or <video>)

  function heroOf(card) { return card.querySelector('.hero'); }

  function upgrade(card) {
    var img = heroOf(card);
    if (!img || img.tagName !== 'IMG') { return; }          // already live
    if (img.dataset.rbr && window.RbrDecoder) { startCanvas(img); }
    else if (img.dataset.src) { startVideo(img); }
  }
  function downgrade(card) {
    var el = heroOf(card);
    if (el && el.tagName !== 'IMG') { demote(el); }
  }
  function demote(el) {
    if (el.tagName === 'CANVAS') { stopCanvas(el); }
    else if (el.tagName === 'VIDEO') { stopVideo(el); }
  }

  // -- audio: one shared AudioContext, at most ONE audible card ----------------
  // The context runs natively at 44100 (the recording rate), so buffers are
  // never resampled — per-buffer resampling of tiny chunks is audible crackle.
  // The audible card keeps a ~150ms window of buffers scheduled AHEAD of the
  // clock (refilled each tick), so rAF jitter can't starve the stream; the
  // audio decode cursor runs independently of the video frame index (both loop
  // at the same period, staying phase-locked within the cushion).
  var actx = null, again = null, audio = null; // audio = {canvas, dec, frame, nextTime}
  var AHEAD = 0.15;   // keep this many seconds scheduled
  var CUSHION = 0.05; // (re)start this far ahead of the clock

  function audioCtxUp() {
    if (!actx) {
      var AC = window.AudioContext || window.webkitAudioContext;
      try { actx = new AC({ sampleRate: 44100, latencyHint: 'playback' }); }
      catch (e) { actx = new AC(); }
      again = actx.createGain();
      again.gain.value = 0.35;
      again.connect(actx.destination);
    }
    if (actx.state === 'suspended') { actx.resume(); }
  }
  function stopAudio() {
    if (!audio) { return; }
    audio.canvas.classList.remove('audible');
    if (audio.dec && audio.dec.free) { audio.dec.free(); }
    audio = null;
  }
  function startAudio(canvas) {
    stopAudio();
    audioCtxUp();
    var st = { canvas: canvas, dec: null, frame: 0, nextTime: 0 };
    audio = st;
    canvas.classList.add('audible');
    fetch(canvas.dataset.audio).then(function (r) { return r.arrayBuffer(); }).then(function (buf) {
      if (audio !== st) { return; }             // switched away while fetching
      try { st.dec = new window.RbrAudioDecoder(new Uint8Array(buf)); } catch (e) { stopAudio(); return; }
      var vs = canvas.st;
      st.frame = vs ? vs.frameIdx : 0;          // join at the video's position
      st.dec.seekFrame(st.frame);
      st.nextTime = 0;                          // fresh cushion on first fill
    }).catch(function () {});
  }
  // Top the schedule up to AHEAD seconds; called from the audible card's tick.
  // The audio cursor is SLAVED to the video position: rAF pacing and the audio
  // hardware clock tick at slightly different real rates (display-rate rounding,
  // DAC clock skew), so two free-running cursors drift apart without bound.
  // Whenever the audio cursor leaves its expected band just ahead of the video
  // frame, it reseeks — audio seeks are cheap (run-skipping); video seeks aren't.
  function refillAudio(st, total) {
    if (!st.dec) { return; }
    var vs = st.canvas.st;
    if (vs && total > 1) {
      // Expected: audio decoded a bit past the shown frame (the scheduled
      // lookahead). Out of band (behind, or > ~2x the window ahead) -> resync.
      var lead = (st.frame - vs.frameIdx + total) % total;
      var maxLead = Math.ceil((AHEAD * 2) * 59.7275) + 2;
      if (lead < 1 || lead > maxLead) {
        st.frame = (vs.frameIdx + 2) % total;
        st.dec.seekFrame(st.frame);
        st.nextTime = 0; // fresh cushion
      }
    }
    var now = actx.currentTime;
    if (st.nextTime < now + 0.001) { st.nextTime = now + CUSHION; } // (re)prime
    var guard = 64; // bound work per tick even if AHEAD grows
    while (st.nextTime - now < AHEAD && guard-- > 0) {
      var inter;
      try { inter = st.dec.nextFrame(); } catch (e) { stopAudio(); return; }
      st.frame++;
      if (!inter.length || (total > 0 && st.frame >= total)) {
        st.frame = 0;
        st.dec.seekFrame(0);                    // loop with the video's period
        if (!inter.length) { continue; }
      }
      var n = inter.length / 2;
      var buf = actx.createBuffer(2, n, 44100); // ctx is 44100: no resampling
      var l = buf.getChannelData(0), r = buf.getChannelData(1);
      for (var i = 0; i < n; i++) { l[i] = inter[2 * i]; r[i] = inter[2 * i + 1]; }
      var src = actx.createBufferSource();
      src.buffer = buf;
      src.connect(again);
      src.start(st.nextTime);
      st.nextTime += n / 44100;
    }
  }

  // -- native .rbr playback: <canvas> driven by the wasm Decoder --
  function startCanvas(img) {
    var canvas = document.createElement('canvas');
    canvas.className = img.className;
    canvas.width = 160; canvas.height = 144;
    canvas.dataset.rbr = img.dataset.rbr;
    if (img.dataset.audio) { canvas.dataset.audio = img.dataset.audio; }
    canvas.dataset.poster = img.getAttribute('src') || '';
    var st = { stopped: false, raf: 0, dec: null, frameIdx: 0 };
    canvas.st = st;
    img.replaceWith(canvas);
    live.push(canvas);
    enforceCap();
    fetch(canvas.dataset.rbr).then(function (r) { return r.arrayBuffer(); }).then(function (buf) {
      if (st.stopped) { return; }
      var dec;
      try { dec = new window.RbrDecoder(new Uint8Array(buf)); } catch (e) { return; }
      st.dec = dec;
      canvas.width = dec.width; canvas.height = dec.height;
      var ctx = canvas.getContext('2d');
      var frame = ctx.createImageData(dec.width, dec.height);
      var ms = dec.frameMs || 16.7, last = -1e9;
      function tick(t) {
        if (st.stopped) { return; }
        st.raf = requestAnimationFrame(tick);
        if (t - last < ms) { return; }        // pace to the recording's fps
        // Accumulate (don't snap to t): snapping loses the sub-tick remainder
        // every frame, which at display rates that don't divide the recording
        // fps runs the video measurably slow and drifts it against the audio
        // clock. Clamp after long gaps (jank, background tab) instead of
        // bursting to catch up.
        last += ms;
        if (t - last > 250) { last = t; }
        try { frame.data.set(dec.nextFrame()); ctx.putImageData(frame, 0, 0); }
        catch (e) { st.stopped = true; return; }
        st.frameIdx = (st.frameIdx + 1) % Math.max(dec.frames, 1);
        // This card is the audible one: keep its audio scheduled ~150ms ahead
        // (the audio cursor loops on the same period, so A/V stay in phase).
        if (audio && audio.canvas === canvas) { refillAudio(audio, dec.frames); }
      }
      st.raf = requestAnimationFrame(tick);
    }).catch(function () {});
  }
  function stopCanvas(canvas) {
    var k = live.indexOf(canvas); if (k >= 0) { live.splice(k, 1); }
    if (audio && audio.canvas === canvas) { stopAudio(); }
    var st = canvas.st;
    if (st) {
      st.stopped = true;
      if (st.raf) { cancelAnimationFrame(st.raf); }
      if (st.dec && st.dec.free) { st.dec.free(); }
    }
    var img = document.createElement('img');
    img.className = canvas.className;
    img.loading = 'lazy';
    img.setAttribute('src', canvas.dataset.poster || '');
    img.dataset.rbr = canvas.dataset.rbr || '';
    if (canvas.dataset.audio) { img.dataset.audio = canvas.dataset.audio; }
    canvas.replaceWith(img);
  }

  // -- legacy mp4 playback: <video> --
  function startVideo(img) {
    var v = document.createElement('video');
    v.className = img.className;
    v.muted = true; v.loop = true; v.playsInline = true;
    v.setAttribute('muted', ''); v.setAttribute('playsinline', '');
    v.preload = 'auto';
    v.poster = img.getAttribute('src') || '';
    v.dataset.src = img.dataset.src;
    v.src = img.dataset.src;
    img.replaceWith(v);
    v.play().catch(function () {});
    live.push(v);
    enforceCap();
  }
  function stopVideo(v) {
    var k = live.indexOf(v); if (k >= 0) { live.splice(k, 1); }
    var img = document.createElement('img');
    img.className = v.className.replace(/\s*audible/, '');
    img.loading = 'lazy';
    img.setAttribute('src', v.poster || '');
    img.dataset.src = v.dataset.src || '';
    v.pause(); v.removeAttribute('src'); try { v.load(); } catch (e) { /* ignore */ }
    v.replaceWith(img);
  }

  // Over CAP: drop the live heroes FARTHEST from the viewport so visible cards
  // stay animated. Cheap — at most CAP rects measured.
  function vdist(el) {
    var r = el.getBoundingClientRect(), mid = (r.top + r.bottom) / 2;
    return mid < 0 ? -mid : (mid > innerHeight ? mid - innerHeight : 0);
  }
  function enforceCap() {
    if (live.length <= CAP) { return; }
    live.sort(function (a, b) { return vdist(a) - vdist(b); });
    while (live.length > CAP) { demote(live[live.length - 1]); }
  }

  // Observe CARDS (stable), not the swappable media node, so upgrade/downgrade
  // never churns the observer. Scoped to the active panel; activate() re-scopes.
  var io = new IntersectionObserver(function (entries) {
    entries.forEach(function (e) {
      var card = e.target;
      var panel = card.closest('.tab-panel');
      var active = panel && panel.classList.contains('active');
      if (e.isIntersecting && active && !card.classList.contains('is-hidden')) { upgrade(card); }
      else { downgrade(card); }
    });
  }, { rootMargin: '300px 0px' });

  function observePanel(p) { if (p) { cardsIn(p).forEach(function (c) { io.observe(c); }); } }
  function unwatchPanel(p) { if (p) { cardsIn(p).forEach(function (c) { io.unobserve(c); downgrade(c); }); } }

  // The decoder module loads async; when ready, upgrade the .rbr cards already
  // in the viewport (the observer won't re-fire for them on its own).
  window.addEventListener('rbr-ready', function () {
    var p = activePanel();
    if (!p) { return; }
    cardsIn(p).forEach(function (c) {
      if (c.classList.contains('is-hidden')) { return; }
      var r = c.getBoundingClientRect();
      if (r.top < innerHeight + 300 && r.bottom > -300) { upgrade(c); }
    });
  });

  // Delegated click: toggle audio on the clicked hero. A playing .rbr canvas
  // with a recording unmutes via the wasm AudioDecoder (one audible card, the
  // click satisfying the AudioContext gesture requirement); legacy mp4 videos
  // keep their built-in track. Static posters ignore clicks.
  document.addEventListener('click', function (ev) {
    var hero = ev.target.closest ? ev.target.closest('.hero') : null;
    if (!hero) { return; }
    var card = hero.closest('.card'); if (!card) { return; }
    if (hero.tagName === 'CANVAS') {
      if (!hero.dataset.audio || !window.RbrAudioDecoder) { return; }
      ev.preventDefault();
      if (audio && audio.canvas === hero) { stopAudio(); } else { startAudio(hero); }
      return;
    }
    if (hero.tagName === 'IMG') {
      if (!hero.dataset.src) { return; }   // static poster or not-yet-live card
      startVideo(hero); hero = heroOf(card);
    }
    if (!hero || hero.tagName !== 'VIDEO') { return; }
    ev.preventDefault();
    if (hero.muted) {
      (activePanel() || document).querySelectorAll('video.hero').forEach(function (o) {
        if (o !== hero) { o.muted = true; o.classList.remove('audible'); }
      });
      hero.muted = false; hero.classList.add('audible'); hero.play().catch(function () {});
    } else {
      hero.muted = true; hero.classList.remove('audible');
    }
  });

  // ---- prefs (localStorage) ----
  function loadPrefs() {
    try {
      var p = JSON.parse(localStorage.getItem(LS_KEY) || '{}');
      if (p.sort) { state.sort = p.sort; }
      if (typeof p.dense === 'boolean') { state.dense = p.dense; }
    } catch (e) { /* ignore */ }
  }
  function savePrefs() {
    try { localStorage.setItem(LS_KEY, JSON.stringify({ sort: state.sort, dense: state.dense })); }
    catch (e) { /* ignore */ }
  }

  // ---- URL hash: shareable view state, or a bare card anchor ----
  function parseHash() {
    var h = location.hash.replace(/^#/, '');
    if (!h) { return null; }
    if (h.indexOf('=') < 0) { return { anchor: h }; }
    var o = {};
    h.split('&').forEach(function (kv) {
      var i = kv.indexOf('=');
      if (i > 0) { o[decodeURIComponent(kv.slice(0, i))] = decodeURIComponent(kv.slice(i + 1)); }
    });
    return o;
  }
  function writeHash() {
    var p = ['tab=' + state.tab];
    if (state.region !== 'all') { p.push('region=' + state.region); }
    if (state.sort !== 'name') { p.push('sort=' + state.sort); }
    if (state.q) { p.push('q=' + encodeURIComponent(state.q)); }
    if (state.fail) { p.push('fail=1'); }
    history.replaceState(null, '', '#' + p.join('&'));
  }

  // ---- sort (via CSS order, so video nodes never detach from the observer) ----
  function statusRank(s) { return s === 'err' ? 0 : s === 'fail' ? 1 : 2; }
  function compareCards(a, b) {
    switch (state.sort) {
      case 'fps': return (+b.dataset.fps) - (+a.dataset.fps);
      case 'size': return (+b.dataset.size) - (+a.dataset.size);
      case 'status':
        return statusRank(a.dataset.status) - statusRank(b.dataset.status)
          || a.dataset.name.localeCompare(b.dataset.name);
      case 'mapper':
        return a.dataset.mapper.localeCompare(b.dataset.mapper)
          || a.dataset.name.localeCompare(b.dataset.name);
      default: return a.dataset.name.localeCompare(b.dataset.name);
    }
  }
  function sortPanel(panel) {
    panel.querySelectorAll('.region-group .grid').forEach(function (grid) {
      var cards = Array.prototype.slice.call(grid.querySelectorAll('.card'));
      cards.sort(compareCards);
      cards.forEach(function (c, i) { c.style.order = i; });
    });
  }

  // ---- filter + chip counts, scoped to the active tab ----
  function apply() {
    var panel = activePanel();
    if (!panel) { return; }
    var cards = panel.querySelectorAll('.region-group .card');
    var q = state.q.toLowerCase();
    var shown = 0, total = 0;
    var rc = { us: 0, jp: 0, eu: 0, global: 0 };

    for (var i = 0; i < cards.length; i++) {
      var c = cards[i];
      var reg = c.dataset.region;
      var passQ = !q || c.dataset.name.indexOf(q) >= 0;
      var passFail = !state.fail || c.dataset.status !== 'ok';
      var passQF = passQ && passFail;
      if (passQF) { total++; if (rc[reg] != null) { rc[reg]++; } }
      var vis = passQF && (state.region === 'all' || state.region === reg);
      c.classList.toggle('is-hidden', !vis);
      if (vis) { shown++; }
    }

    // Hide region sections with nothing visible under the current filter.
    panel.querySelectorAll('.region-group').forEach(function (g) {
      g.style.display = g.querySelector('.card:not(.is-hidden)') ? '' : 'none';
    });

    sortPanel(panel);

    // Chip counts reflect the q/fail filter (ignoring the region selection).
    $chips().forEach(function (chip) {
      var key = chip.dataset.region;
      var n = key === 'all' ? total : (rc[key] || 0);
      chip.textContent = REGION_LABEL[key] + ' (' + n + ')';
    });
    els.count.textContent = shown + ' shown';
  }

  function applyDense() {
    document.querySelectorAll('.grid').forEach(function (g) {
      g.classList.toggle('dense', state.dense);
    });
    els.dense.checked = state.dense;
  }

  function activate(tab, noHash) {
    if (!document.getElementById('panel-' + tab)) {
      var first = $tabs()[0];
      tab = first ? first.dataset.tab : tab;
    }
    var prev = state.tab;
    if (prev && prev !== tab) { unwatchPanel(document.getElementById('panel-' + prev)); }
    state.tab = tab;
    $tabs().forEach(function (b) { b.classList.toggle('active', b.dataset.tab === tab); });
    document.querySelectorAll('.tab-panel').forEach(function (p) {
      p.classList.toggle('active', p.id === 'panel-' + tab);
    });
    observePanel(activePanel());
    apply();
    if (!noHash) { writeHash(); }
  }

  // ---- wiring ----
  loadPrefs();

  $tabs().forEach(function (b) {
    b.addEventListener('click', function () { activate(b.dataset.tab); });
  });

  $chips().forEach(function (b) {
    b.addEventListener('click', function () {
      state.region = b.dataset.region;
      $chips().forEach(function (c) { c.classList.toggle('active', c === b); });
      apply();
      writeHash();
    });
  });

  document.querySelectorAll('.region-head').forEach(function (h) {
    h.addEventListener('click', function () { h.parentElement.classList.toggle('collapsed'); });
  });

  document.querySelectorAll('.lk').forEach(function (b) {
    b.addEventListener('click', function () {
      var url = location.origin + location.pathname + '#' + b.dataset.id;
      var done = function () {
        var o = b.textContent;
        b.textContent = '✓ copied';
        setTimeout(function () { b.textContent = o; }, 900);
      };
      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(url).then(done, done);
      } else { done(); }
    });
  });

  var qTimer;
  els.q.addEventListener('input', function () {
    state.q = els.q.value.trim();
    clearTimeout(qTimer);
    qTimer = setTimeout(function () { apply(); writeHash(); }, 120);
  });
  els.sort.addEventListener('change', function () {
    state.sort = els.sort.value; savePrefs(); apply(); writeHash();
  });
  els.fail.addEventListener('change', function () {
    state.fail = els.fail.checked; apply(); writeHash();
  });
  els.dense.addEventListener('change', function () {
    state.dense = els.dense.checked; savePrefs(); applyDense();
  });

  // ---- initial state: URL hash wins over prefs; a bare anchor deep-links ----
  var hash = parseHash();
  var initTab = ($tabs()[0] || {}).dataset ? $tabs()[0].dataset.tab : null;
  var anchorCard = null;
  if (hash && hash.anchor) {
    anchorCard = document.getElementById(hash.anchor);
    if (anchorCard) { initTab = anchorCard.closest('.tab-panel').id.replace('panel-', ''); }
  } else if (hash) {
    if (hash.tab) { initTab = hash.tab; }
    if (hash.region) { state.region = hash.region; }
    if (hash.sort) { state.sort = hash.sort; }
    if (hash.q) { state.q = hash.q; }
    if (hash.fail) { state.fail = true; }
  }

  els.q.value = state.q;
  els.fail.checked = state.fail;
  els.sort.value = state.sort;
  $chips().forEach(function (c) { c.classList.toggle('active', c.dataset.region === state.region); });
  applyDense();
  activate(initTab, true);

  if (anchorCard) {
    var grp = anchorCard.closest('.region-group');
    if (grp) { grp.classList.remove('collapsed'); grp.style.display = ''; }
    anchorCard.classList.remove('is-hidden');
    anchorCard.scrollIntoView({ block: 'center' });
    anchorCard.classList.add('hilite');
  } else {
    writeHash();
  }
})();
