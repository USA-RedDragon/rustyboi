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
  function vidsIn(root) {
    return Array.prototype.slice.call((root || document).querySelectorAll('video.hero'));
  }
  function activePanel() { return document.getElementById('panel-' + state.tab); }

  // ---- lazy media: load/play only near-viewport videos in the active panel ----
  function loadAndPlay(v) {
    if (!v.src) { v.poster = v.dataset.poster; v.src = v.dataset.src; v.load(); }
    v.play().catch(function () {});
  }
  function release(v) {
    v.pause();
    if (!v.muted) { v.muted = true; v.classList.remove('audible'); }
    if (v.src) { v.removeAttribute('src'); v.load(); }
  }
  var io = new IntersectionObserver(function (entries) {
    entries.forEach(function (e) {
      var v = e.target;
      var panel = v.closest('.tab-panel');
      var active = panel && panel.classList.contains('active');
      var hidden = v.closest('.card') && v.closest('.card').classList.contains('is-hidden');
      if (e.isIntersecting && active && !hidden) { loadAndPlay(v); }
      else { release(v); }
    });
  }, { rootMargin: '400px 0px' });

  vidsIn(document).forEach(function (v) {
    io.observe(v);
    // Click toggles a single audible video (all others re-muted).
    v.addEventListener('click', function (ev) {
      ev.preventDefault();
      if (v.muted) {
        vidsIn(document).forEach(function (o) {
          if (o !== v) { o.muted = true; o.classList.remove('audible'); }
        });
        v.muted = false; v.classList.add('audible'); loadAndPlay(v);
      } else {
        v.muted = true; v.classList.remove('audible');
      }
    });
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

    // Play newly-visible near-viewport videos; release the rest.
    vidsIn(panel).forEach(function (v) {
      var card = v.closest('.card');
      if (!card || card.classList.contains('is-hidden')) { release(v); return; }
      var r = v.getBoundingClientRect();
      if (r.top < innerHeight && r.bottom > 0) { loadAndPlay(v); }
    });
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
    state.tab = tab;
    $tabs().forEach(function (b) { b.classList.toggle('active', b.dataset.tab === tab); });
    document.querySelectorAll('.tab-panel').forEach(function (p) {
      var on = p.id === 'panel-' + tab;
      p.classList.toggle('active', on);
      if (!on) { vidsIn(p).forEach(release); }
    });
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
