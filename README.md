# Retro Cycles

A remake of Armagetron/Retro Cycles light-cycle combat: Rust server, Rust→WebAssembly client, WebGL2 in the browser.
Both crates have **zero dependencies**.

```sh
./build.sh
./target/release/server --addr 0.0.0.0:8080 --bots 4
# open http://localhost:8080
```

Server flags: `--addr`, `--bots N` (main lobby: top every round up to N cycles with AI), `--rounds N` (main lobby's match length, default 10), `--web DIR`, `--threads N` (simulation threads, default: cores up to 8), `--snapshot-hz N` (position updates per second, default 60; 30 halves bandwidth), `--max-lobbies N` (default 256), `--max-per-ip N` (lobbies one address may open, default 3), `--stats` (print tick timing every 10 s).
URL params: `?lobby=CODE` open that lobby (invite links look like this), `?name=X&color=ff2d6f` play straight away (in `?lobby` or the public grid), `?watch` spectate, `?cam=1-4` start camera, `?mute`, `?aa=0` disable MSAA, `?scale=0.5` render resolution.

## Playing

| Keys | |
|---|---|
| ←/→, A/D (or tap left/right half) | turn |
| ↓, S, Space (hold) | brake — limited tank, recharges |
| 1–4 or C | camera: chase · cockpit · overhead · arena |
| M | minimap |
| Esc | menu: lobbies, change name / light colour mid-game, invite, leave |

- **Lobbies:** the menu lists public lobbies with their players, bots and round; PLAY or WATCH any of them, or join one by its code. **Create a lobby** with your own name, 0–15 bots (exactly that many play alongside the humans), 1–20 rounds, public or private, and an optional password. Private lobbies never appear in the list: share the invite link (`?lobby=CODE`) or the code. Locked lobbies ask for the password, whether you come from the list, a code or a link. The permanent **Public Grid** (code `GRID`) is what `--bots`/`--rounds` configure; other lobbies close after a minute with nobody in them. Limits: 256 lobbies (`--max-lobbies`), 16 players + 48 spectators per lobby, 3 lobbies per address (`--max-per-ip`), one create or password attempt per second per connection.
- A match is 10 rounds by default; each round lasts until one cycle is left. Then the final standings show and a new match starts.
- Ride close to any wall to accelerate (45 → 140 u/s).
- **Sudden death:** 60 s into a round a red zone closes in from the rim and derezzes anyone it passes. Once every human is out it starts after 3 s and closes fast, so nobody waits long for the next round. Meanwhile the camera follows whoever got you.
- **Scoring:** kill +2 (+1 more if they were boxed in, with walls close on both sides) · derezzed −1 · own wall or rim −3 · round win +3. Kills, deaths and points are on the scoreboard; a hit marker and kill feed show who got whom.
- **Feedback:** riding a wall throws sparks, adds a grinding whine and a camera rumble, and the screen edges glow in your colour as you speed up. Turning away from a wall about 120 ms before impact calls a CLOSE CALL; escaping after you've already hit one calls SAVED. Kills float their points, shake the camera and trigger the announcer (FIRST BLOOD, BOXED, DOUBLE/TRIPLE KILL, REVENGE, and streaks from KILLING SPREE to GODLIKE), spoken through the browser's built-in speech engine.
- Bots fill empty seats and pick colours that don't clash with players'.

## Layout

| Path | What |
|---|---|
| `common/` | Constants, binary wire protocol, axis-aligned wall collision (shared so client prediction matches the server). |
| `server/` | std-only HTTP + WebSocket (hand-rolled SHA-1/base64/framing), lobbies (one game each, all stepped by a single 60 Hz tick thread on one shared clock), authoritative sim, bots. |
| `client/` | `cdylib` for `wasm32-unknown-unknown`: networking state, clock sync, prediction, camera, all geometry. |
| `web/` | `index.html` + `main.js`: a ~250-line shim that forwards socket bytes/input and draws. |

## Latency

- `TCP_NODELAY` on every socket. Each client has one reader thread; outgoing messages are written straight from whichever thread produces them with non-blocking `send(2)` (no writer thread, so no context switch per message). Bytes the kernel can't take wait in a small per-client backlog, and a client more than ~2 s behind is dropped, so a slow client never stalls the sim.
- Pings are answered on the reader thread without taking the game lock.
- The client keeps a server clock estimate (NTP-style, trusting the lowest-RTT samples) and renders every cycle at *server present time*.
- A turn is sent the moment the key event fires, stamped with that time. The server rewinds the cycle along its current straight segment to the stamp (≤ 250 ms back) and re-simulates — so at any ping you turn exactly where you saw yourself turn. Stamps between ticks are applied exactly after the next tick.
- Turn events are broadcast immediately, not on the next snapshot. Snapshots carry only live cycles at 9 bytes each (positions as 1/128-unit integers) and go out every tick (`--snapshot-hz`), or 4 times a second while nothing moves.
- Hitting a wall leaves you pinned for 150 ms before you derez, so a turn already in flight still saves you.

## Frame rate

- Per frame, Rust writes the entire scene (walls, floor light, cycles, particles, minimap) into one vertex buffer of 16-byte vertices.
- JS does one `bufferSubData` (ring of two VBOs) and five draws: procedural grid floor, opaque bikes/arena, floor glow (MAX blend, so trail and bike glows merge instead of stacking), additive light walls/trim/particles, minimap. No sorting needed.
- Light walls are thin boxes with a hot top face, so they stay visible edge-on.
- `frame()` costs ~0.2 ms of CPU with 9 cycles mid-round (~25k vertices); the DOM HUD is only touched when text changes (≤20 Hz).
- Sound is synthesized with WebAudio — no assets to download.
- The WebGL2 context asks for `desynchronized` and `high-performance` hints.

## Tests

`cargo test -p common -p server` covers collision edge cases, the WebSocket handshake, boxed-in deaths and lobbies.

Capacity benchmarks (ignored by default):
`cargo test --release -p server bench -- --ignored --nocapture` times the tick for 1–200 full 16-bot lobbies and for a worst-case dense round (16 cycles, ~2000 wall points).

## Deploying

Every push to `master` builds static Linux binaries and publishes them as the **latest-build** pre-release; pushing a tag like `v1.0.0` publishes a versioned release (`.github/workflows/build.yml`, which runs `./package.sh`). Each download holds `server` and the `web/` files it serves:

```sh
curl -LO https://github.com/morganbarber/tron/releases/download/latest-build/retro-cycles-linux-x86_64.tar.gz
tar xzf retro-cycles-linux-x86_64.tar.gz && cd retro-cycles-linux-x86_64
./server --addr 0.0.0.0:8080 --snapshot-hz 30
```

Use `aarch64` instead of `x86_64` for ARM servers (Graviton, Raspberry Pi). The binaries are statically linked, so they run on any distribution; `server --version` shows the build. `./package.sh` builds the same archives locally into `dist/`.

Each connection uses 1 thread and 3 file descriptors, so raise the open-file limit (`ulimit -n 65536`, or `LimitNOFILE=65536` in a systemd unit). Most systems default to 1024, which caps the server at about 330 connections.

Capacity, measured on an i9-10850K over loopback: connections spread over lobbies of 16 bots, up to 64 connections each (every connection receives the same snapshots a player does):

| Snapshots | 1,000 connections | 4,000 connections | Per player |
|---|---|---|---|
| 60 Hz | 0.35 cores, 18 MB | 1.65 cores, 52 MB, tick p99 6 ms | ~8 KB/s of messages; ~45 MB/hour on the wire incl. TCP/IP and inbound ACKs |
| 30 Hz | — | 1.05 cores, 54 MB, tick p99 5 ms | ~4 KB/s of messages; ~25 MB/hour on the wire |

Lobbies are stepped in parallel; a worst-case crowded round (16 cycles, ~2,000 walls) costs ~4 µs per tick thanks to the wall grid.
