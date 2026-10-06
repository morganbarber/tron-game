// Thin shim: socket + input in, one vertex upload + a few draw calls out.
// All game logic, prediction and geometry live in client.wasm.

const params = new URLSearchParams(location.search);
const canvas = document.getElementById('c');
const gl = canvas.getContext('webgl2', {
  antialias: params.get('aa') !== '0',
  alpha: false,
  depth: true,
  stencil: false,
  desynchronized: true,
  powerPreference: 'high-performance',
  preserveDrawingBuffer: false,
});
if (!gl) {
  document.getElementById('center').textContent = 'WebGL2 is required';
  throw new Error('no webgl2');
}

const dec = new TextDecoder();
const enc = new TextEncoder();
let ex = null;
let ws = null;
const bytes = (p, n) => new Uint8Array(ex.memory.buffer, p, n);
const $ = (id) => document.getElementById(id);

// ---------------------------------------------------------------- audio (synthesized, no assets)
let ac = null;
let engine = null;
function audio() {
  if (ac) return ac;
  try {
    ac = new AudioContext();
  } catch {
    return null;
  }
  const master = ac.createGain();
  master.gain.value = 0.5;
  master.connect(ac.destination);
  ac.out = master;
  // Engine: two detuned saws through a lowpass, pitch follows speed.
  const g = ac.createGain();
  g.gain.value = 0;
  const lp = ac.createBiquadFilter();
  lp.type = 'lowpass';
  lp.frequency.value = 500;
  lp.connect(g).connect(master);
  const oscs = [0, 7].map((detune) => {
    const o = ac.createOscillator();
    o.type = 'sawtooth';
    o.detune.value = detune;
    o.frequency.value = 50;
    o.connect(lp);
    o.start();
    return o;
  });
  engine = { g, lp, oscs };
  // Grind: looped noise through a bandpass plus a thin whine, both rise with speed.
  const buf = ac.createBuffer(1, ac.sampleRate, ac.sampleRate);
  const d = buf.getChannelData(0);
  for (let i = 0; i < d.length; i++) d[i] = Math.random() * 2 - 1;
  const src = ac.createBufferSource();
  src.buffer = buf;
  src.loop = true;
  const bp = ac.createBiquadFilter();
  bp.type = 'bandpass';
  bp.Q.value = 2.5;
  bp.frequency.value = 2500;
  const gg = ac.createGain();
  gg.gain.value = 0;
  src.connect(bp).connect(gg).connect(master);
  src.start();
  const whine = ac.createOscillator();
  whine.type = 'triangle';
  whine.frequency.value = 700;
  const wg = ac.createGain();
  wg.gain.value = 0;
  whine.connect(wg).connect(master);
  whine.start();
  grind = { gg, bp, whine, wg };
  return ac;
}
let grind = null;
function tone(freq, dur, type = 'square', vol = 0.15, slide = 0) {
  if (!ac) return;
  const t = ac.currentTime;
  const o = ac.createOscillator();
  const g = ac.createGain();
  o.type = type;
  o.frequency.setValueAtTime(freq, t);
  if (slide) o.frequency.exponentialRampToValueAtTime(Math.max(20, freq * slide), t + dur);
  g.gain.setValueAtTime(vol, t);
  g.gain.exponentialRampToValueAtTime(0.0001, t + dur);
  o.connect(g).connect(ac.out);
  o.start(t);
  o.stop(t + dur);
}
function noise(dur, vol) {
  if (!ac) return;
  const t = ac.currentTime;
  const buf = ac.createBuffer(1, Math.floor(ac.sampleRate * dur), ac.sampleRate);
  const d = buf.getChannelData(0);
  for (let i = 0; i < d.length; i++) d[i] = (Math.random() * 2 - 1) * (1 - i / d.length) ** 2;
  const src = ac.createBufferSource();
  src.buffer = buf;
  const lp = ac.createBiquadFilter();
  lp.type = 'lowpass';
  lp.frequency.setValueAtTime(3000, t);
  lp.frequency.exponentialRampToValueAtTime(120, t + dur);
  const g = ac.createGain();
  g.gain.value = vol;
  src.connect(lp).connect(g).connect(ac.out);
  src.start(t);
}
const muted = () => params.has('mute');
function sfx(kind, a, b) {
  if (!ac || muted()) return;
  const t = ac.currentTime;
  switch (kind) {
    case 0: {
      // engine
      const on = a >= 0;
      engine.g.gain.setTargetAtTime(on ? 0.05 : 0, t, 0.08);
      if (on) {
        const f = 38 + a * 95 - b * 8;
        for (const o of engine.oscs) o.frequency.setTargetAtTime(f, t, 0.06);
        engine.lp.frequency.setTargetAtTime(300 + a * 1500, t, 0.1);
      }
      break;
    }
    case 1: tone(880, 0.05, 'square', 0.04); break;
    case 2: noise(0.9, 0.5 * a); tone(160, 0.6, 'sawtooth', 0.12 * a, 0.2); break;
    case 3: tone(1320, 0.07, 'square', 0.1); setTimeout(() => tone(1760, 0.12, 'square', 0.1), 60); break;
    case 4: tone(440, 0.15, 'square', 0.08); break;
    case 5: tone(880, 0.35, 'square', 0.1); break;
    case 6:
      (a ? [523, 659, 784, 1047] : [392, 330, 262]).forEach((f, i) => setTimeout(() => tone(f, 0.18, 'triangle', 0.12), i * 110));
      break;
    case 7: {
      const k = a ** 1.5;
      grind.gg.gain.setTargetAtTime(k * 0.09, t, 0.04);
      grind.bp.frequency.setTargetAtTime(1800 + b * 3500, t, 0.08);
      grind.wg.gain.setTargetAtTime(k * 0.025, t, 0.05);
      grind.whine.frequency.setTargetAtTime(500 + b * 1100 + a * 200, t, 0.08);
      break;
    }
    case 8: tone(320, 0.22, 'sawtooth', 0.07, 3.5); noise(0.25, 0.25); break;
    case 9: [660, 990, 1320].forEach((f, i) => setTimeout(() => tone(f, 0.12, 'triangle', 0.13), i * 55)); break;
    case 10: {
      // Power chord sting; higher tiers climb in pitch.
      const root = 196 * 2 ** (Math.min(a, 6) / 12);
      for (const m of [1, 1.5, 2]) tone(root * m, 0.45, 'square', 0.05, 1.02);
      break;
    }
  }
}

// Announcer: the browser's own speech engine, pitched down. No audio files.
let voice;
function say(text) {
  if (muted() || !('speechSynthesis' in window)) return;
  const ss = speechSynthesis;
  voice ??= ss.getVoices().find((v) => /^en(-|_)/.test(v.lang)) || null;
  if (ss.pending) ss.cancel(); // don't fall behind a burst of callouts
  const u = new SpeechSynthesisUtterance(text.toLowerCase());
  if (voice) u.voice = voice;
  u.pitch = 0.4;
  u.rate = 1.1;
  u.volume = 0.9;
  ss.speak(u);
}

// ---------------------------------------------------------------- HUD
const esc = (s) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
const sw = (hex) => `<i style="background:#${hex};box-shadow:0 0 8px #${hex}"></i>`;
const hitEl = $('hit');
const edgeEl = $('edge');
const calloutsEl = $('callouts');
const fx = (el, parent) => {
  parent.appendChild(el);
  el.addEventListener('animationend', () => el.remove());
};
function setHud(slot, text) {
  switch (slot) {
    case 0:
      if (!text) {
        $('board').innerHTML = '';
        break;
      }
      $('board').innerHTML =
        '<div class="row head"><i></i><span></span><em>K</em><em>D</em><b>PTS</b></div>' +
        text.split('\n').filter(Boolean).map((line) => {
          const [hex, name, score, flags, kills, deaths] = line.split('\t');
          const cls = (flags.includes('a') ? '' : ' dead') + (flags.includes('m') ? ' me' : '');
          return `<div class="row${cls}">${sw(hex)}<span>${esc(name)}${flags.includes('b') ? ' <small>bot</small>' : ''}</span>` +
            `<em>${kills}</em><em>${deaths}</em><b>${score}</b></div>`;
        }).join('');
      break;
    case 4:
      hitEl.querySelector('span').textContent = text;
      hitEl.classList.remove('go');
      void hitEl.offsetWidth; // restart the animation
      hitEl.classList.add('go');
      break;
    case 5:
      $('feed').innerHTML = text.split('\n').filter(Boolean).map((line) => {
        const [h1, n1, verb, h2, n2] = line.split('\t');
        const who = (h, n) => `<b style="color:#${h}">${esc(n)}</b>`;
        return n2 ? `<div>${who(h1, n1)} ${esc(verb)} ${who(h2, n2)}</div>` : `<div>${who(h1, n1)} ${esc(verb)}</div>`;
      }).join('');
      break;
    case 6: {
      const m = $('meters');
      if (!text) {
        m.hidden = true;
        break;
      }
      const [speed, frac, brake] = text.split(' ').map(Number);
      m.hidden = false;
      $('spd').textContent = speed.toFixed(0);
      $('spdbar').style.transform = `scaleX(${frac})`;
      $('brkbar').style.transform = `scaleX(${brake})`;
      break;
    }
    case 8: {
      const [hex, alpha] = text.split(' ');
      edgeEl.style.opacity = alpha || 0;
      if (hex) edgeEl.style.setProperty('--ec', '#' + hex);
      break;
    }
    case 9: {
      const [style, msg] = text.split('\t');
      if (style !== 'close' && style !== 'save') say(msg);
      if (style === 'voice') break;
      const el = document.createElement('div');
      el.className = style;
      el.textContent = msg;
      fx(el, calloutsEl);
      while (calloutsEl.children.length > 4) calloutsEl.firstChild.remove();
      if (style === 'save' || style === 'close') {
        edgeEl.classList.remove('flash');
        void edgeEl.offsetWidth;
        edgeEl.classList.add('flash');
      }
      break;
    }
    case 10: {
      const [pos, msg] = text.split('\t');
      const [x, y] = pos.split(' ').map(Number);
      const el = document.createElement('div');
      el.className = 'pop hud';
      el.textContent = msg;
      el.style.left = x * 100 + '%';
      el.style.top = y * 100 + '%';
      fx(el, document.body);
      break;
    }
    default:
      $(['board', 'center', 'stats', 'hint', 'hit', 'feed', 'meters', 'round'][slot]).textContent = text;
  }
}

// ---------------------------------------------------------------- wasm
const imports = {
  env: {
    ws_send(p, n) {
      if (ws && ws.readyState === 1) ws.send(bytes(p, n).slice());
    },
    hud(slot, p, n) {
      setHud(slot, dec.decode(bytes(p, n)));
    },
    sfx,
    console_log(p, n) {
      console.error(dec.decode(bytes(p, n)));
    },
  },
};
const { instance } = await WebAssembly.instantiateStreaming(fetch('client.wasm'), imports);
ex = instance.exports;
ex.init();

// ---------------------------------------------------------------- network + lobbies
// Lobby messages (10-12) are handled here; everything else goes to the wasm.
const M = { LOBBIES: 10, ENTERED: 11, LOBBY_ERR: 12, C_LIST: 0x14, C_CREATE: 0x15, C_ENTER: 0x16, C_LEAVE: 0x17 };
const LOCKED = 1, PRIVATE = 2, MAIN = 4;
const MAIN_CODE = 'GRID';
const ERRORS = {
  1: 'That lobby doesn\'t exist (any more).',
  2: 'Wrong password.',
  3: 'Too many lobbies open right now — join one instead.',
  4: 'Slow down — try again in a second.',
  5: 'Check the lobby settings.',
  6: 'That lobby is full.',
};
let profile = null; // { name, color } once the player has chosen
let session = null; // { code, name, flags, pw, play } while in a lobby
let pending = null; // { code?, pw, play } while an enter / create is in flight
let lobbies = [];

function sendRaw(kind, ...parts) {
  if (!ws || ws.readyState !== 1) return false;
  const out = [kind];
  for (const p of parts) {
    if (typeof p === 'number') out.push(p & 255);
    else {
      const b = enc.encode(p).subarray(0, 255);
      out.push(b.length, ...b);
    }
  }
  ws.send(new Uint8Array(out));
  return true;
}
function readLobby(d, o) {
  const str = () => {
    const n = d[o];
    const s = dec.decode(d.subarray(o + 1, o + 1 + n));
    o += 1 + n;
    return s;
  };
  const code = str();
  const [humans, bots, watchers, round, rounds, flags] = d.subarray(o, o + 6);
  o += 6;
  const name = str();
  return [{ code, humans, bots, watchers, round, rounds, flags, name }, o];
}

function enterLobby(code, play, pw = '') {
  pending = { code, play, pw };
  showError('');
  sendRaw(M.C_ENTER, code, pw);
}
function onLobbyMessage(d) {
  switch (d[0]) {
    case M.LOBBIES: {
      lobbies = [];
      let o = 2;
      for (let i = 0; i < d[1]; i++) {
        const [l, next] = readLobby(d, o);
        lobbies.push(l);
        o = next;
      }
      renderLobbies();
      break;
    }
    case M.ENTERED: {
      const [l] = readLobby(d, 1);
      const p = pending || { play: false, pw: '' };
      pending = null;
      session = { ...l, pw: p.pw, play: p.play };
      ex.on_open(performance.now()); // fresh client state for the new game
      if (session.play && profile) sendJoin();
      params.set('lobby', l.code);
      history.replaceState(null, '', '?' + params);
      hidePassword();
      renderHere();
      if (p.created && l.flags & PRIVATE) {
        copyInvite();
        renderHere('Private lobby — invite link copied. Only people with it can find this lobby.');
      } else if (!p.keepMenu) closeMenu();
      break;
    }
    case M.LOBBY_ERR: {
      const code = d[1];
      if (code === 2 && pending?.code) {
        // Locked: ask for the password, keeping what they were trying to do.
        askPassword(pending, !!pending.pw);
      } else {
        showError(ERRORS[code] || 'Something went wrong.');
        if (code === 1 && session === null) {
          params.delete('lobby');
          history.replaceState(null, '', '?' + params);
        }
      }
      if (code !== 2) pending = null;
      openMenu();
      break;
    }
  }
}

function connect() {
  const proto = location.protocol === 'https:' ? 'wss://' : 'ws://';
  ws = new WebSocket(proto + location.host + '/ws');
  ws.binaryType = 'arraybuffer';
  ws.onopen = () => {
    ex.on_open(performance.now());
    sendRaw(M.C_LIST);
    // Rejoin where we were after a reconnect, else follow the URL.
    if (session) {
      const s = session;
      session = null;
      enterLobby(s.code, s.play, s.pw);
    } else if (startup) {
      startup();
      startup = null;
    }
  };
  ws.onmessage = (e) => {
    const d = new Uint8Array(e.data);
    if (d[0] >= M.LOBBIES && d[0] <= M.LOBBY_ERR) return onLobbyMessage(d);
    const p = ex.inbox(d.length);
    bytes(p, d.length).set(d);
    ex.on_message(d.length, performance.now());
  };
  ws.onclose = () => {
    ex.on_close();
    setTimeout(connect, 1000);
  };
}

function sendJoin() {
  const name = enc.encode(profile.name).subarray(0, 48);
  const p = ex.inbox(name.length);
  bytes(p, name.length).set(name);
  const c = parseInt(profile.color.slice(1), 16);
  ex.join(name.length, (c >> 16) & 255, (c >> 8) & 255, c & 255);
}

// ---------------------------------------------------------------- menu
const form = $('join');
const nameIn = $('name');
const colorIn = $('color');
const swatches = $('swatches');
const PRESETS = ['#00e5ff', '#ff8a14', '#ff2d6f', '#7dff3a', '#b45cff', '#ffe640', '#2dffc8', '#ff6ad5', '#4d7cff', '#ffffff'];
function pickColor(hex) {
  colorIn.value = hex;
  for (const b of swatches.children) b.classList.toggle('on', b.dataset.c === hex);
  $('preview').style.setProperty('--c', hex);
}
for (const c of PRESETS) {
  const b = document.createElement('button');
  b.type = 'button';
  b.dataset.c = c;
  b.style.setProperty('--c', c);
  b.title = c;
  b.addEventListener('click', () => pickColor(c));
  swatches.appendChild(b);
}
colorIn.addEventListener('input', () => pickColor(colorIn.value));
try {
  nameIn.value = localStorage.getItem('rc-name') || '';
  pickColor(localStorage.getItem('rc-color') || PRESETS[0]);
} catch {
  pickColor(PRESETS[0]);
}
function saveProfile() {
  profile = { name: nameIn.value.trim() || 'Program', color: colorIn.value };
  try {
    localStorage.setItem('rc-name', profile.name);
    localStorage.setItem('rc-color', profile.color);
  } catch {}
}

let listTimer = 0;
function openMenu() {
  form.hidden = false;
  renderHere();
  sendRaw(M.C_LIST);
  clearInterval(listTimer);
  listTimer = setInterval(() => sendRaw(M.C_LIST), 2500);
}
function closeMenu() {
  form.hidden = true;
  nameIn.blur();
  clearInterval(listTimer);
}
function showError(text) {
  $('err').textContent = text;
}

// Lobby list rows: name, players, round, PLAY / WATCH.
function renderLobbies() {
  const box = $('lobbies');
  box.replaceChildren();
  if (!lobbies.length) {
    box.innerHTML = '<div class="empty">No public lobbies.</div>';
    return;
  }
  for (const l of lobbies) {
    const row = document.createElement('div');
    row.className = 'lobby' + (session?.code === l.code ? ' here' : '');
    const people = l.humans === 1 ? '1 player' : `${l.humans} players`;
    const bots = l.bots ? ` · ${l.bots} bot${l.bots === 1 ? '' : 's'}` : '';
    const watching = l.watchers ? ` · ${l.watchers} watching` : '';
    row.innerHTML =
      `<div class="info"><b></b>${l.flags & LOCKED ? '<span class="tag">PASSWORD</span>' : ''}` +
      `${session?.code === l.code ? '<span class="tag here">HERE</span>' : ''}` +
      `<small>${people}${bots}${watching} · round ${l.round}/${l.rounds}</small></div>` +
      '<button type="button" class="play">PLAY</button><button type="button" class="watch">WATCH</button>';
    row.querySelector('b').textContent = l.name;
    row.querySelector('.play').addEventListener('click', () => pick(l, true));
    row.querySelector('.watch').addEventListener('click', () => pick(l, false));
    box.appendChild(row);
  }
}
function pick(l, play) {
  audio();
  if (play) saveProfile();
  if (session?.code === l.code) {
    // Already here: just (re)join or carry on watching.
    if (play) sendJoin();
    session.play ||= play;
    return closeMenu();
  }
  if (l.flags & LOCKED) askPassword({ code: l.code, name: l.name, play }, false);
  else enterLobby(l.code, play);
}

// Password prompt shared by the list, join-by-code and invite links.
let pwTarget = null;
function askPassword(target, wrong) {
  pwTarget = target;
  $('pwbox').hidden = false;
  $('pwfor').textContent = target.name || target.code;
  showError(wrong ? 'Wrong password.' : '');
  $('pw').value = '';
  $('pw').focus();
}
function hidePassword() {
  $('pwbox').hidden = true;
  pwTarget = null;
}
function submitPassword() {
  if (!pwTarget) return;
  const t = pwTarget;
  enterLobby(t.code, t.play, $('pw').value);
  pending.name = t.name;
  pending.keepMenu = t.keepMenu;
}
$('pwgo').addEventListener('click', submitPassword);
$('pw').addEventListener('keydown', (e) => {
  if (e.key === 'Enter') {
    e.preventDefault();
    e.stopPropagation();
    submitPassword();
  }
});

$('codego').addEventListener('click', () => {
  const code = $('code').value.trim().toUpperCase();
  if (!code) return;
  saveProfile();
  enterLobby(code, true);
});

// The block shown when you're in a lobby: where you are, invite, leave.
function renderHere(note) {
  const here = $('here');
  here.hidden = !session;
  $('lobbytag').textContent = session ? `${session.name} · ${session.code}` : '';
  $('go').textContent = session ? (session.play ? 'APPLY' : 'PLAY HERE') : 'QUICK PLAY';
  $('watch').textContent = session ? 'BACK TO GAME' : 'WATCH THE PUBLIC GRID';
  if (!session) return;
  $('here-name').textContent = session.name;
  $('here-code').textContent = session.code;
  $('here-note').textContent = note || (session.flags & PRIVATE ? 'Private: only people with the invite link or code can join.' : '');
  $('leave').hidden = session.code === MAIN_CODE;
}
function inviteLink() {
  return `${location.origin}${location.pathname}?lobby=${session.code}`;
}
function copyInvite() {
  try {
    navigator.clipboard.writeText(inviteLink()).catch(() => {});
  } catch {}
  $('invite').textContent = 'COPIED!';
  setTimeout(() => ($('invite').textContent = 'COPY INVITE LINK'), 1500);
}
$('invite').addEventListener('click', copyInvite);
$('leave').addEventListener('click', () => {
  sendRaw(M.C_LEAVE);
  session = null;
  ex.on_open(performance.now());
  params.delete('lobby');
  history.replaceState(null, '', '?' + params);
  renderHere();
  renderLobbies();
});

// Primary button: quick play in the public grid, or play / apply where we are.
form.addEventListener('submit', (e) => {
  e.preventDefault();
  audio();
  saveProfile();
  if (session) {
    session.play = true;
    sendJoin();
    closeMenu();
  } else enterLobby(MAIN_CODE, true);
});
$('watch').addEventListener('click', () => {
  audio();
  if (session) closeMenu();
  else enterLobby(MAIN_CODE, false);
});
$('refresh').addEventListener('click', () => sendRaw(M.C_LIST));

// Create a lobby.
const visBtns = [...document.querySelectorAll('#vis button')];
let privateLobby = false;
for (const b of visBtns) {
  b.addEventListener('click', () => {
    privateLobby = b.dataset.v === 'private';
    for (const o of visBtns) o.classList.toggle('on', o === b);
  });
}
$('bots').addEventListener('input', () => ($('botsv').textContent = $('bots').value));
$('create-toggle').addEventListener('click', () => {
  const c = $('create');
  c.hidden = !c.hidden;
  if (!c.hidden && !$('lname').value) $('lname').value = `${nameIn.value.trim() || 'Program'}'s grid`;
});
$('create-go').addEventListener('click', () => {
  audio();
  saveProfile();
  const name = $('lname').value.trim() || `${profile.name}'s grid`;
  const pw = $('lpw').value;
  pending = { play: true, pw, created: true };
  showError('');
  sendRaw(M.C_CREATE, +$('bots').value, +$('rounds').value, privateLobby ? 1 : 0, name, pw);
});

// URL: ?lobby=CODE enters that lobby, ?name=X plays straight away, ?watch spectates.
let startup = () => {
  const code = (params.get('lobby') || '').toUpperCase();
  if (params.has('name')) {
    nameIn.value = params.get('name');
    if (params.has('color')) pickColor('#' + params.get('color'));
    saveProfile();
    enterLobby(code || MAIN_CODE, true);
  } else if (params.has('watch')) {
    enterLobby(code || MAIN_CODE, false);
  } else if (code) {
    // Invite link: watch while they pick a name and colour, then PLAY HERE.
    enterLobby(code, false);
    pending.keepMenu = true;
    openMenu();
  } else openMenu();
};
connect();

if (/^[1-4]$/.test(params.get('cam') || '')) ex.on_key(9 + +params.get('cam'), 0);

// ---------------------------------------------------------------- input
// Turns are forwarded the instant the event fires, not on the next frame.
const LEFT = new Set(['ArrowLeft', 'KeyA', 'KeyJ', 'KeyQ', 'Comma']);
const RIGHT = new Set(['ArrowRight', 'KeyD', 'KeyL', 'KeyE', 'Period']);
const BRAKE = new Set(['ArrowDown', 'KeyS', 'KeyK', 'Space']);
addEventListener('keydown', (e) => {
  if (!form.hidden) {
    if (e.code === 'Escape' && session) closeMenu();
    return;
  }
  audio();
  if (e.repeat) return e.preventDefault();
  const t = performance.now();
  if (LEFT.has(e.code)) ex.on_key(1, t);
  else if (RIGHT.has(e.code)) ex.on_key(2, t);
  else if (BRAKE.has(e.code)) ex.on_key(4, t);
  else if (e.code === 'KeyC' || e.code === 'KeyV') ex.on_key(3, t);
  else if (e.code === 'KeyM') ex.on_key(6, t);
  else if (/^Digit[1-4]$/.test(e.code)) ex.on_key(9 + +e.code[5], t);
  else if (e.code === 'Escape' || (e.code === 'Enter' && !session?.play)) openMenu();
  else return;
  e.preventDefault();
});
addEventListener('keyup', (e) => {
  if (BRAKE.has(e.code)) ex.on_key(5, performance.now());
});
addEventListener('blur', () => ex.on_key(5, performance.now()));
canvas.addEventListener('pointerdown', (e) => {
  audio();
  const t = performance.now();
  if (e.pointerType === 'mouse') ex.on_key(e.button === 2 ? 2 : e.button === 0 ? 1 : 0, t);
  else ex.on_key(e.clientX < innerWidth / 2 ? 1 : 2, t);
});
canvas.addEventListener('contextmenu', (e) => e.preventDefault());
$('menu').addEventListener('click', openMenu);

// ---------------------------------------------------------------- WebGL
function program(vs, fs) {
  const p = gl.createProgram();
  for (const [type, src] of [[gl.VERTEX_SHADER, vs], [gl.FRAGMENT_SHADER, fs]]) {
    const s = gl.createShader(type);
    gl.shaderSource(s, src);
    gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s));
    gl.attachShader(p, s);
  }
  gl.linkProgram(p);
  if (!gl.getProgramParameter(p, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(p));
  return p;
}

const FOG = 'exp(-d * 0.0018)';
const sceneProg = program(
  `#version 300 es
  layout(location=0) in vec3 p;
  layout(location=1) in vec4 c;
  uniform mat4 vp;
  uniform vec3 eye;
  out vec3 vc;
  void main() {
    gl_Position = vp * vec4(p, 1.0);
    float d = distance(p, eye);
    vc = c.rgb * ${FOG};
  }`,
  `#version 300 es
  precision mediump float;
  in vec3 vc;
  out vec4 o;
  void main() { o = vec4(vc, 1.0); }`,
);
const floorProg = program(
  `#version 300 es
  layout(location=0) in vec2 q;
  uniform mat4 vp;
  uniform float arena;
  out vec2 w;
  void main() {
    w = mix(vec2(-arena), vec2(arena * 2.0), q * 0.5 + 0.5);
    gl_Position = vp * vec4(w.x, 0.0, -w.y, 1.0);
  }`,
  `#version 300 es
  precision highp float;
  in vec2 w;
  uniform vec3 eye;
  uniform float arena;
  out vec4 o;
  float grid(vec2 p, float s, float th) {
    vec2 g = p / s;
    vec2 d = abs(fract(g - 0.5) - 0.5) / fwidth(g);
    return 1.0 - min(min(d.x, d.y) / th, 1.0);
  }
  void main() {
    float d = distance(vec3(w.x, 0.0, -w.y), eye);
    bool inside = w.x >= 0.0 && w.y >= 0.0 && w.x <= arena && w.y <= arena;
    float minor = grid(w, 10.0, 1.0) * clamp(1.6 - d / 250.0, 0.0, 1.0);
    float major = grid(w, 50.0, 1.6);
    vec3 base = vec3(0.010, 0.014, 0.028);
    vec3 col = inside
      ? base + vec3(0.025, 0.08, 0.15) * minor + vec3(0.05, 0.15, 0.27) * major
      : base * 0.4;
    o = vec4(col * ${FOG}, 1.0);
  }`,
);
const U = {
  scene: { vp: gl.getUniformLocation(sceneProg, 'vp'), eye: gl.getUniformLocation(sceneProg, 'eye') },
  floor: {
    vp: gl.getUniformLocation(floorProg, 'vp'),
    eye: gl.getUniformLocation(floorProg, 'eye'),
    arena: gl.getUniformLocation(floorProg, 'arena'),
  },
};
const IDENTITY = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
const ORIGIN = new Float32Array(3);

const floorVao = gl.createVertexArray();
gl.bindVertexArray(floorVao);
gl.bindBuffer(gl.ARRAY_BUFFER, gl.createBuffer());
gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
gl.enableVertexAttribArray(0);
gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);

// Two ring-buffered VBOs so we never write into one the GPU may still be reading.
const STRIDE = 16;
const slots = [0, 1].map(() => {
  const vao = gl.createVertexArray();
  const vbo = gl.createBuffer();
  gl.bindVertexArray(vao);
  gl.bindBuffer(gl.ARRAY_BUFFER, vbo);
  gl.bufferData(gl.ARRAY_BUFFER, 1 << 21, gl.DYNAMIC_DRAW);
  gl.enableVertexAttribArray(0);
  gl.vertexAttribPointer(0, 3, gl.FLOAT, false, STRIDE, 0);
  gl.enableVertexAttribArray(1);
  gl.vertexAttribPointer(1, 4, gl.UNSIGNED_BYTE, true, STRIDE, 12);
  return { vao, vbo, cap: 1 << 21 };
});
gl.bindVertexArray(null);
gl.clearColor(0.004, 0.006, 0.012, 1);

const scale = Math.min(parseFloat(params.get('scale')) || 1, 2);
let frameNo = 0;
function loop() {
  requestAnimationFrame(loop);
  const dpr = devicePixelRatio * scale;
  const w = Math.round(canvas.clientWidth * dpr);
  const h = Math.round(canvas.clientHeight * dpr);
  if (canvas.width !== w || canvas.height !== h) {
    canvas.width = w;
    canvas.height = h;
  }

  const n = ex.frame(performance.now(), w, h);
  const u = new Float32Array(ex.memory.buffer, ex.uniforms_ptr(), 32);
  const vp = u.subarray(0, 16);
  const eye = u.subarray(16, 19);
  const [opaque, glow, additive] = [u[20], u[22], u[23]];

  gl.viewport(0, 0, w, h);
  gl.depthMask(true); // clear() honours the mask, which the glow passes leave off
  gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT);

  // Floor: no depth write, everything else sits on top of it.
  gl.disable(gl.BLEND);
  gl.enable(gl.DEPTH_TEST);
  gl.depthMask(false);
  gl.useProgram(floorProg);
  gl.uniformMatrix4fv(U.floor.vp, false, vp);
  gl.uniform3fv(U.floor.eye, eye);
  gl.uniform1f(U.floor.arena, u[21]);
  gl.bindVertexArray(floorVao);
  gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);

  if (n > 0) {
    const s = slots[frameNo++ & 1];
    gl.bindBuffer(gl.ARRAY_BUFFER, s.vbo);
    const size = n * STRIDE;
    if (size > s.cap) {
      s.cap = Math.ceil(size * 1.5);
      gl.bufferData(gl.ARRAY_BUFFER, s.cap, gl.DYNAMIC_DRAW);
    }
    gl.bufferSubData(gl.ARRAY_BUFFER, 0, bytes(ex.verts_ptr(), size));
    gl.useProgram(sceneProg);
    gl.uniformMatrix4fv(U.scene.vp, false, vp);
    gl.uniform3fv(U.scene.eye, eye);
    gl.bindVertexArray(s.vao);
    // 1. Opaque bikes and arena panels write depth.
    gl.depthMask(true);
    if (opaque > 0) gl.drawArrays(gl.TRIANGLES, 0, opaque);
    gl.depthMask(false);
    gl.enable(gl.BLEND);
    // 2. Floor light: MAX so overlapping glows merge rather than stack.
    gl.blendEquation(gl.MAX);
    if (glow > opaque) gl.drawArrays(gl.TRIANGLES, opaque, glow - opaque);
    // 3. Light walls, trim and particles: additive.
    gl.blendEquation(gl.FUNC_ADD);
    gl.blendFunc(gl.ONE, gl.ONE);
    if (additive > glow) gl.drawArrays(gl.TRIANGLES, glow, additive - glow);
    // 4. Minimap overlay in clip space.
    if (n > additive) {
      gl.disable(gl.BLEND);
      gl.disable(gl.DEPTH_TEST);
      gl.uniformMatrix4fv(U.scene.vp, false, IDENTITY);
      gl.uniform3fv(U.scene.eye, ORIGIN);
      gl.drawArrays(gl.TRIANGLES, additive, n - additive);
    }
  }
}
requestAnimationFrame(loop);
